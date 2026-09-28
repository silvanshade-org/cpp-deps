//! Fresh compiler discovery, action validation and complete-result execution.

use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use std::env;
use std::fs;
use std::fs::File;
use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use crate::BuildError;
use crate::cache::Action;
use crate::cache::Destination;
use crate::cache::Field;
use crate::cache::Key;
use crate::cache::Lookup;
use crate::cache::ModuleCache;
use crate::cache::Policy;
use crate::cache::Role;
use crate::runner::CompilerKind;

/// Shared invocation context; output locking is owned by the build session.
pub struct Session<'build>
{
    /// Native compiler and wrapper selection.
    pub tool: &'build cc::Tool,
    /// Selected compiler adapter.
    pub kind: CompilerKind,
    /// Explicit local-cache policy.
    pub cache: &'build Policy,
    /// Captured compiler working directory.
    pub cwd: &'build Path,
    /// Admitted output root held under an exclusive session lock.
    pub output_root: &'build Path,
    /// Cross-unit ownership of persistent compiler products.
    pub owners: std::sync::Mutex<BTreeMap<PathBuf, blake3::Hash>>,
}

/// Complete live output and imported-artifact bindings for one compilation.
pub struct Invocation<'plan>
{
    /// Primary source for contextual errors and dependency evidence.
    pub source: &'plan Path,
    /// Object path also names the private discovery files.
    pub object: &'plan Path,
    /// All required output roles, including structured metadata and mappings.
    pub destinations: Vec<Destination>,
    /// Imported BMI bytes and their validated textual input closure.
    pub imports: Vec<PathBuf>,
}

/// Preserve compiler inputs while replacing compile-only output controls.
///
/// # Specification
/// - ensures: preprocessing receives one output and depfile destination while
///   retaining ordered semantic arguments, environment and working directory.
/// - panics: none.
fn discovery_command(command: &Command) -> Command
{
    let mut copy = Command::new(command.get_program());
    let mut arguments = command.get_args();
    while let Some(argument) = arguments.next() {
        if argument == "-o" || argument == "-MF" {
            let _ = arguments.next();
        }
        else if argument != "-c" && argument != "-MD" && argument != "-MMD" {
            copy.arg(argument);
        }
    }
    copy_context(command, &mut copy);
    copy
}

/// Preserve effective environment overrides and the invocation directory.
///
/// # Specification
/// - ensures: the destination observes the source command environment and cwd.
/// - panics: none.
fn copy_context(
    command: &Command,
    copy: &mut Command,
)
{
    for (name, value) in command.get_envs() {
        match value {
            | Some(value) => {
                copy.env(name, value);
            },
            | None => {
                copy.env_remove(name);
            },
        }
    }
    if let Some(directory) = command.get_current_dir() {
        copy.current_dir(directory);
    }
}

/// Launch a compiler command with contextual diagnostics.
///
/// # Specification
/// - ensures: unsuccessful native exits never become cache misses or hits.
/// - fails: launch or compiler errors.
/// - panics: none.
fn run(
    command: &mut Command,
    source: &Path,
) -> Result<(), BuildError>
{
    let output = command.output().map_err(|error| BuildError::Io {
        operation: "launch C++ compiler",
        path: source.to_path_buf(),
        source: error,
    })?;
    if !output.status.success() {
        return Err(BuildError::CompileFailed {
            source: source.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Validate every destination before either compilation or restoration.
///
/// # Specification
/// - ensures: live compiler metadata cannot authorize output writes outside the
///   admitted root, through parent traversal, or through a symlink destination.
/// - fails: invalid layout, duplicate destinations or filesystem errors.
/// - panics: none.
fn admit(
    destinations: &[Destination],
    root: &Path,
) -> io::Result<()>
{
    let mut seen = BTreeSet::new();
    for destination in destinations {
        let path = &destination.path;
        if !path.is_absolute()
            || !path.starts_with(root)
            || path
                .components()
                .any(|component| component == Component::ParentDir)
            || !seen.insert(path)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "artifact destination outside admitted root or duplicated: {}",
                    path.display()
                ),
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "artifact has no parent"))?;
        let relative = parent.strip_prefix(root).map_err(io::Error::other)?;
        let mut admitted = root.to_path_buf();
        for component in relative.components() {
            admitted.push(component);
            match fs::symlink_metadata(&admitted) {
                | Ok(metadata) if metadata.is_dir() => {},
                | Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "artifact parent is a symlink or not a directory",
                    ));
                },
                | Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    #[expect(
                        clippy::create_dir,
                        reason = "create only the admitted immediate child; never traverse unchecked ancestors"
                    )]
                    fs::create_dir(&admitted)?;
                },
                | Err(error) => return Err(error),
            }
        }
        match fs::symlink_metadata(path) {
            | Ok(metadata) if !metadata.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "artifact destination is not a regular file",
                ));
            },
            | Ok(_) => {},
            | Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            | Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Resolve current textual inputs, including negative and shadowing includes.
///
/// # Specification
/// - ensures: dependency evidence is generated afresh through the configured
///   wrapper, using the same compiler arguments and actual imported BMIs.
/// - fails: missing inputs, preprocessing failures or malformed depfiles.
/// - panics: none.
fn discover(
    command: &Command,
    invocation: &Invocation<'_>,
    cwd: &Path,
) -> Result<(PathBuf, Vec<PathBuf>), BuildError>
{
    let preprocessed = invocation.object.with_extension("validation.ii");
    let depfile = invocation.object.with_extension("validation.d");
    let mut discover = discovery_command(command);
    discover
        .arg("-E")
        .arg("-MD")
        .arg("-MF")
        .arg(&depfile)
        .arg("-o")
        .arg(&preprocessed);
    run(&mut discover, invocation.source)?;
    let paths = crate::depfile::read(&depfile, cwd).map_err(|source| BuildError::Io {
        operation: "read fresh textual dependency witness",
        path: depfile,
        source,
    })?;
    Ok((preprocessed, paths))
}

/// Hash file bytes without incidental timestamps of freshly generated evidence.
///
/// # Specification
/// - ensures: preprocessing and scanner output are hashed in a separate field,
///   without materializing the whole file or making regeneration itself a miss.
/// - fails: open/read/key-encoding failures.
/// - panics: none.
fn evidence(
    key: &mut Key,
    path: &Path,
) -> io::Result<()>
{
    let mut hash = blake3::Hasher::new_derive_key("cpp-deps/evidence/v1");
    hash.update_reader(File::open(path)?)?;
    key.field(Field(hash.finalize().as_bytes()))
}

/// Query the actual driver, never wrapper-cached diagnostic output.
///
/// # Specification
/// - ensures: response expansion and subordinate commands use live arguments.
/// - fails: driver launch or expansion failure.
/// - panics: none.
fn driver_trace(
    tool: &cc::Tool,
    command: &Command,
) -> io::Result<std::process::Output>
{
    let mut driver = Command::new(tool.path());
    driver.args(tool.args());
    let prefix = tool.to_command().get_args().count();
    driver.args(command.get_args().skip(prefix));
    copy_context(command, &mut driver);
    if tool.is_like_gnu() {
        driver.arg("-pipe");
    }
    let trace = driver.arg("-###").output()?;
    if !trace.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&trace.stderr).into_owned(),
        ));
    }
    Ok(trace)
}

/// Extend the live inventory with persistent outputs declared by the driver.
///
/// # Specification
/// - ensures: saved intermediates, split debug information, coverage notes and
///   time traces join the same admitted, verified result as the object and BMI;
///   transient pipe/temporary outputs and runtime coverage data are excluded.
/// - fails: malformed driver command words or non-UTF-8 diagnostic paths.
/// - panics: none.
fn driver_outputs(
    trace: &std::process::Output,
    invocation: &mut Invocation<'_>,
    cwd: &Path,
) -> io::Result<()>
{
    let text = core::str::from_utf8(&trace.stderr).map_err(io::Error::other)?;
    let saved = text.contains("-save-temps");
    let mut paths = BTreeSet::new();
    for line in text.lines().filter(|line| line.starts_with(' ')) {
        let words = shlex::split(line).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed compiler subprocess words",
            )
        })?;
        for pair in words.windows(2) {
            let [ref flag, ref value] = *pair
            else {
                continue;
            };
            if matches!(
                flag.as_str(),
                "-split-dwarf-output"
                    | "-coverage-notes-file"
                    | "-serialize-diagnostic-file"
                    | "-fthin-link-bitcode"
            ) || (saved && flag == "-o" && value != "-")
            {
                paths.insert(cwd.join(value));
            }
        }
        for word in &words {
            if let Some((flag, value)) = word.split_once('=')
                && matches!(
                    flag,
                    "-coverage-notes-file"
                        | "-split-dwarf-output"
                        | "-serialize-diagnostic-file"
                        | "-fthin-link-bitcode"
                        | "-ftime-trace"
                )
            {
                paths.insert(cwd.join(value));
            }
        }
        if words.iter().any(|word| word == "--extract-dwo")
            && let Some(path) = words.last()
        {
            paths.insert(cwd.join(path));
        }
        // GCC derives coverage notes from its explicit auxiliary dump stem.
        if words.iter().any(|word| word == "-ftest-coverage")
            && !words.iter().any(|word| word == "-E")
        {
            if let Some(path) = words
                .iter()
                .find_map(|word| word.strip_prefix("-fprofile-note="))
            {
                paths.insert(cwd.join(path));
            }
            else {
                let mut directory = cwd.to_path_buf();
                let mut base = invocation
                    .object
                    .file_name()
                    .map_or_else(PathBuf::new, PathBuf::from);
                let mut extension = "";
                for pair in words.windows(2) {
                    let [ref flag, ref value] = *pair
                    else {
                        continue;
                    };
                    match flag.as_str() {
                        | "-dumpdir" => directory = cwd.join(value),
                        | "-dumpbase" => base = PathBuf::from(value),
                        | "-dumpbase-ext" => extension = value,
                        | _ => {},
                    }
                }
                let stem = base.to_str().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 coverage stem")
                })?;
                let stem = stem.strip_suffix(extension).unwrap_or(stem);
                paths.insert(directory.join(format!("{stem}.gcno")));
            }
        }
    }
    for path in paths {
        if !invocation
            .destinations
            .iter()
            .any(|destination| destination.path == path)
        {
            invocation.destinations.push(Destination {
                role: Role::Additional,
                path,
            });
        }
    }
    Ok(())
}
/// Build complete compiler-observable action identity from fresh evidence.
///
/// # Specification
/// - requires: caller attests the immutable complete toolchain closure.
/// - ensures: ordered arguments, effective environment, current preprocessor
///   result, resolved input bytes/times, and actual imported BMIs contribute.
/// - fails: unreadable input, unstable evidence or compiler driver query
///   failure.
/// - panics: none.
fn identify(
    cache: &ModuleCache,
    command: &Command,
    invocation: &Invocation<'_>,
    trace: &std::process::Output,
    cwd: &Path,
) -> Result<Action, BuildError>
{
    let (preprocessed, inputs) = discover(command, invocation, cwd)?;
    let result = (|| -> io::Result<Action> {
        let mut key = Key::default();
        key.field(Field(cache.toolchain().0.as_bytes()))?;
        key.field(Field(cwd.as_os_str().as_encoded_bytes()))?;
        key.field(Field(command.get_program().as_encoded_bytes()))?;
        let arguments: Vec<_> = command.get_args().collect();
        key.field(Field(&arguments.len().to_le_bytes()))?;
        for argument in arguments {
            key.field(Field(argument.as_encoded_bytes()))?;
        }
        let mut environment: BTreeMap<_, _> = env::vars_os().collect();
        for (name, value) in command.get_envs() {
            match value {
                | Some(value) => {
                    environment.insert(name.to_owned(), value.to_owned());
                },
                | None => {
                    environment.remove(name);
                },
            }
        }
        key.field(Field(&environment.len().to_le_bytes()))?;
        for (name, value) in environment {
            key.field(Field(name.as_encoded_bytes()))?;
            key.field(Field(value.as_encoded_bytes()))?;
        }
        // Hash the current driver expansion as well as original arguments.
        key.field(Field(&trace.stdout))?;
        key.field(Field(&trace.stderr))?;
        evidence(&mut key, &preprocessed)?;
        key.file(invocation.source)?;
        // Mappers are generated semantic inputs: byte-identical regeneration
        // must not invalidate results solely because its timestamp changed.
        let file = |key: &mut Key, path: &Path| -> io::Result<()> {
            if invocation.destinations.iter().any(|destination| {
                matches!(destination.role, Role::Mapping) && destination.path == path
            }) {
                key.field(Field(path.as_os_str().as_encoded_bytes()))?;
                evidence(key, path)
            }
            else {
                key.file(path)
            }
        };
        key.field(Field(&inputs.len().to_le_bytes()))?;
        for input in inputs {
            file(&mut key, &input)?;
        }
        key.field(Field(&invocation.imports.len().to_le_bytes()))?;
        for import in &invocation.imports {
            file(&mut key, import)?;
        }
        key.field(Field(&invocation.destinations.len().to_le_bytes()))?;
        for destination in &invocation.destinations {
            key.field(Field(destination.path.as_os_str().as_encoded_bytes()))?;
            // Scanner serialization order is not compiler semantics; resolved
            // bindings already contribute through arguments and imported files.
            if matches!(destination.role, Role::Mapping) {
                evidence(&mut key, &destination.path)?;
            }
        }
        Ok(key.finish())
    })();
    result.map_err(|source| BuildError::Io {
        operation: "validate module compilation identity",
        path: invocation.source.to_path_buf(),
        source,
    })
}

/// Reserve compiler products against cross-unit destination collisions.
///
/// # Specification
/// - ensures: only coordinator-owned mappings can be shared between sources;
///   saved intermediates cannot race or replace another unit's cached output.
/// - fails: conflicting ownership or a poisoned registry.
/// - panics: none.
fn reserve(
    session: &Session<'_>,
    invocation: &Invocation<'_>,
) -> io::Result<()>
{
    let identity = blake3::hash(invocation.source.as_os_str().as_encoded_bytes());
    let mut owners = session
        .owners
        .lock()
        .map_err(|error| io::Error::other(error.to_string()))?;
    for destination in &invocation.destinations {
        if matches!(destination.role, Role::Mapping) {
            continue;
        }
        if let Some(owner) = owners.get(&destination.path) {
            if *owner != identity {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "artifact destination shared by different sources: {}",
                        destination.path.display()
                    ),
                ));
            }
        }
        else {
            owners.insert(destination.path.clone(), identity);
        }
    }
    drop(owners);
    Ok(())
}
/// Execute one complete operation, restoring only after current validation.
///
/// # Specification
/// - requires: imported providers are complete and output root is exclusively
///   locked for this session; filesystem mutations are cooperative.
/// - ensures: object-only wrapper hits cannot reuse stale companion outputs;
///   input changes across compilation prevent publication; all restored outputs
///   are ready before any dependent can be released.
/// - fails: unsupported cache adapter, native failure, incomplete inventory,
///   unstable inputs or cache I/O failures.
/// - panics: none.
pub fn execute(
    session: &Session<'_>,
    command: &mut Command,
    mut invocation: Invocation<'_>,
) -> Result<(), BuildError>
{
    let context = |source| BuildError::Io {
        operation: "materialize complete module result",
        path: invocation.source.to_path_buf(),
        source,
    };
    admit(&invocation.destinations, session.output_root).map_err(context)?;
    let action = match *session.cache {
        | Policy::Disabled => {
            reserve(session, &invocation).map_err(context)?;
            None
        },
        | Policy::Enabled(ref cache) => {
            if matches!(session.kind, CompilerKind::Msvc) {
                return Err(context(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "MSVC does not provide this adapter's complete textual discovery witness",
                )));
            }
            let trace = driver_trace(session.tool, command).map_err(context)?;
            driver_outputs(&trace, &mut invocation, session.cwd).map_err(context)?;
            admit(&invocation.destinations, session.output_root).map_err(context)?;
            reserve(session, &invocation).map_err(context)?;
            let action = identify(cache, command, &invocation, &trace, session.cwd)?;
            match cache
                .restore(action, &invocation.destinations, session.output_root)
                .map_err(context)?
            {
                | Lookup::Restored => return Ok(()),
                | Lookup::Absent | Lookup::Corrupt => {},
            }
            Some(action)
        },
    };
    // Remove only admitted compiler products. Scanner output and the shared
    // mapper are current inputs owned by the coordinator, not stale products.
    for destination in &invocation.destinations {
        if matches!(destination.role, Role::Structured | Role::Mapping) {
            continue;
        }
        match fs::remove_file(&destination.path) {
            | Ok(()) => {},
            | Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            | Err(error) => return Err(context(error)),
        }
    }
    run(command, invocation.source)?;
    for destination in &invocation.destinations {
        let metadata = fs::metadata(&destination.path).map_err(context)?;
        if !metadata.is_file() {
            return Err(context(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing regular compiler artifact",
            )));
        }
    }
    if let Some(before) = action
        && let Policy::Enabled(ref cache) = *session.cache
    {
        let trace = driver_trace(session.tool, command).map_err(context)?;
        let after = identify(cache, command, &invocation, &trace, session.cwd)?;
        if before != after {
            return Err(context(io::Error::other(
                "inputs changed across compilation; result not published",
            )));
        }
        let mut witnessed = crate::depfile::read(
            &invocation.object.with_extension("validation.d"),
            session.cwd,
        )
        .map_err(context)?
        .into_iter()
        .collect::<BTreeSet<_>>();
        witnessed.extend(invocation.imports.iter().cloned());
        let compiled = crate::depfile::read(&invocation.object.with_extension("d"), session.cwd)
            .map_err(context)?;
        if compiled.iter().any(|input| !witnessed.contains(input)) {
            return Err(context(io::Error::new(
                io::ErrorKind::InvalidData,
                "compilation read an input absent from validated dependency evidence",
            )));
        }
        cache
            .publish(after, &invocation.destinations)
            .map_err(context)?;
    }
    Ok(())
}

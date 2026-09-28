use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use p1689::r5::DepFile;

use crate::BuildError;
use crate::graph::Unit;

/// Scanner bytes awaiting native command/path representation.
#[derive(Clone, Copy)]
pub struct ScannerBytes<'source>(pub &'source [u8]);

impl<'source> TryFrom<ScannerBytes<'source>> for &'source std::ffi::OsStr
{
    type Error = io::Error;

    /// Borrow native text without validating Unix byte strings.
    ///
    /// # Specification
    /// - ensures: Unix preserves every byte without allocation or validation.
    /// - fails: non-Unix platforms require representable UTF-8 scanner text.
    /// - panics: none.
    ///
    /// # Errors
    /// Returns an encoding failure only on non-Unix targets.
    #[inline]
    fn try_from(bytes: ScannerBytes<'source>) -> Result<Self, Self::Error>
    {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            Ok(std::ffi::OsStr::from_bytes(bytes.0))
        }
        #[cfg(not(unix))]
        {
            let text = core::str::from_utf8(bytes.0)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Ok(std::ffi::OsStr::new(text))
        }
    }
}

/// Convert scanner text at the native encoding boundary.
///
/// # Specification
/// - ensures: no string decoding or validation on Unix.
/// - fails: unrepresentable scanner bytes on other platforms.
/// - panics: none.
///
/// # Errors
/// Returns an encoding error tied to its translation unit.
pub fn native_text<'source>(
    bytes: ScannerBytes<'source>,
    source: &Path,
) -> Result<&'source std::ffi::OsStr, BuildError>
{
    <&std::ffi::OsStr>::try_from(bytes).map_err(|error| BuildError::Io {
        operation: "represent scanner text in native encoding",
        path: source.to_path_buf(),
        source: error,
    })
}

/// Resolve the scanner's optional working directory.
///
/// # Specification
/// - ensures: absent directory uses invocation context; relative paths join it.
/// - fails: a reported directory cannot use the native encoding.
/// - panics: none.
///
/// # Errors
/// Returns the contextual native encoding failure.
pub fn rule_directory(
    unit: &Unit<'_>,
    cwd: &Path,
) -> Result<PathBuf, BuildError>
{
    match unit.rule.work_directory.as_deref() {
        | Some(bytes) => {
            let path = native_text(ScannerBytes(bytes), unit.source)?;
            Ok(cwd.join(path))
        },
        | None => Ok(cwd.to_path_buf()),
    }
}

/// Join a flag prefix and native path with one exact-capacity allocation.
///
/// # Specification
/// - ensures: preserves native path bytes without display conversion.
/// - panics: none, except allocation failure.
fn path_argument(
    prefix: &std::ffi::OsStr,
    path: &Path,
) -> OsString
{
    let mut argument = OsString::with_capacity(prefix.len().saturating_add(path.as_os_str().len()));
    argument.push(prefix);
    argument.push(path);
    argument
}

/// Scanner buffers and invocation paths owned by the compilation session.
pub struct Scan
{
    /// Source passed to the scanner.
    pub source: PathBuf,
    /// Primary object destination.
    pub object: PathBuf,
    /// Complete scanner JSON, retained until all graph views are dropped.
    pub json: Vec<u8>,
}

/// Native compiler family used to select module scan and output flags.
#[derive(Clone, Copy)]
pub enum CompilerKind
{
    /// Clang with `clang-scan-deps` and PCM interfaces.
    Clang,
    /// GCC with P1689 preprocessing and a GCM mapper.
    Gcc,
    /// MSVC with directive scanning and IFC interfaces.
    Msvc,
}

impl CompilerKind
{
    /// Select the scanner and object format without inspecting a command's
    /// text.
    ///
    /// # Specification
    /// - ensures: an unsupported compiler fails before any unit is scanned.
    /// - errors: compiler family is not Clang, GCC, or MSVC.
    /// - panics: none.
    pub fn from_tool(tool: &cc::Tool) -> Result<Self, BuildError>
    {
        if tool.is_like_clang() {
            Ok(Self::Clang)
        }
        else if tool.is_like_gnu() {
            Ok(Self::Gcc)
        }
        else if tool.is_like_msvc() {
            Ok(Self::Msvc)
        }
        else {
            Err(BuildError::UnsupportedCompiler {
                compiler: tool.path().to_path_buf(),
            })
        }
    }
}
/// Scanner selection; native compiler families do their own scanning.
pub enum Scanner
{
    /// Standalone Clang dependency scanner.
    Clang
    {
        /// `clang-scan-deps` executable.
        path: PathBuf,
        /// The compiler's own resource directory, which holds its builtin
        /// headers such as `stddef.h`.
        resource_dir: PathBuf,
    },
    /// GCC or MSVC scans with the compiler itself.
    Native,
}

/// Ask the configured compiler, wrapper and flags included, for one path.
///
/// # Specification
/// - ensures: the answer comes from the compiler that will build the units, so
///   a shim, wrapper, or versioned executable name reports its real
///   installation.
/// - errors: the compiler cannot start, exits unsuccessfully, or prints no
///   path.
/// - panics: none.
fn query_compiler(
    tool: &cc::Tool,
    query: &'static str,
) -> Result<PathBuf, BuildError>
{
    let output = tool
        .to_command()
        .arg(query)
        .output()
        .map_err(|source| BuildError::Io {
            operation: "launch C++ compiler",
            path: tool.path().to_path_buf(),
            source,
        })?;
    let answer = String::from_utf8_lossy(&output.stdout);
    let answer = answer.trim();
    if !output.status.success() || answer.is_empty() {
        return Err(BuildError::CompilerQuery {
            compiler: tool.path().to_path_buf(),
            query,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(PathBuf::from(answer))
}

/// Select the Clang scanner and the resource directory it must scan with.
///
/// # Specification
/// - ensures: without an override, the scanner is the `clang-scan-deps` the
///   compiler reports beside itself, then the one beside the configured
///   compiler path, then `clang-scan-deps` on `PATH`; the resource directory is
///   always the compiler's own, because `clang-scan-deps` otherwise derives it
///   from the compiler path it is given, which is wrong for shims.
/// - errors: the compiler cannot report its resource directory.
/// - panics: none.
pub fn clang_scanner(
    tool: &cc::Tool,
    scanner: Option<PathBuf>,
) -> Result<Scanner, BuildError>
{
    let resource_dir = query_compiler(tool, "-print-resource-dir")?;
    let path = match scanner {
        | Some(path) => path,
        | None => query_compiler(tool, "-print-prog-name=clang-scan-deps")
            .ok()
            .filter(|path| path.is_absolute() && path.is_file())
            .unwrap_or_else(|| {
                let sibling = tool.path().with_file_name("clang-scan-deps");
                if sibling.is_absolute() && !sibling.is_file() {
                    PathBuf::from("clang-scan-deps")
                }
                else {
                    sibling
                }
            }),
    };
    Ok(Scanner::Clang { path, resource_dir })
}

/// GCC requires a module mapper while the other families do not.
pub enum Mapper
{
    /// GCC mapper file under `OUT_DIR`.
    Gcc(PathBuf),
    /// Native compilation needs no GCC mapper.
    NotRequired,
}

/// Stable translation-unit index in the dependency graph.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct UnitIndex(pub usize);

/// Identify an absolute source path independently of arena and insertion order.
///
/// # Specification
/// - requires: source is the canonical absolute path used by the invocation.
/// - ensures: exact platform path bytes contribute in a dedicated domain;
///   content changes retain the destination while action keys change.
/// - panics: none.
fn source_identity(source: &Path) -> blake3::Hash
{
    let mut hash = blake3::Hasher::new_derive_key("cpp-deps/source-path/v1");
    hash.update(source.as_os_str().as_encoded_bytes());
    hash.finalize()
}

/// Select a unique BMI path that never derives a filename from an untrusted
/// module name.
///
/// # Specification
/// - ensures: all interfaces live under `OUT_DIR`, with the compiler's native
///   suffix.
/// - panics: none.
pub fn interface_path(
    kind: CompilerKind,
    out_dir: &Path,
    source: &Path,
) -> PathBuf
{
    let suffix = match kind {
        | CompilerKind::Clang => "pcm",
        | CompilerKind::Gcc => "gcm",
        | CompilerKind::Msvc => "ifc",
    };
    let identity = source_identity(source).to_hex();
    out_dir.join(format!("module-{identity}.{suffix}"))
}

/// Resolve a wrapped compiler to its actual executable for clang-scan-deps.
///
/// # Specification
/// - ensures: the scanner is never fed `ccache` as a compiler or a bare
///   executable name that its subprocess cannot find on `PATH`.
/// - errors: no executable is found on `PATH`, or path resolution fails.
/// - panics: none.
fn compiler_path(tool: &cc::Tool) -> Result<PathBuf, BuildError>
{
    let path = tool.path();
    if path.is_absolute() || path.components().count() > 1 {
        return fs::canonicalize(path).map_err(|source| BuildError::Io {
            operation: "resolve C++ compiler",
            path: path.to_path_buf(),
            source,
        });
    }
    for directory in env::split_paths(&env::var_os("PATH").unwrap_or_default()) {
        let candidate = directory.join(path);
        if candidate.is_file() {
            return fs::canonicalize(&candidate).map_err(|source| BuildError::Io {
                operation: "resolve C++ compiler",
                path: candidate,
                source,
            });
        }
    }
    Err(BuildError::UnsupportedCompiler {
        compiler: path.to_path_buf(),
    })
}

/// Preserve a caller's standard selection, defaulting only when none was
/// configured.
///
/// # Specification
/// - ensures: a provided `-std=` or `/std:` survives unmodified, otherwise
///   C++20 is selected.
/// - panics: none.
fn ensure_standard(
    tool: &cc::Tool,
    kind: CompilerKind,
    command: &mut Command,
)
{
    if tool.args().iter().any(|arg| {
        let arg = arg.to_string_lossy();
        arg.starts_with("-std=") || arg.starts_with("/std:")
    }) {
        return;
    }
    command.arg(match kind {
        | CompilerKind::Msvc => "/std:c++20",
        | CompilerKind::Clang | CompilerKind::Gcc => "-std=c++20",
    });
}

/// Run the family-specific scanner and extract one P1689 rule for this source.
///
/// # Specification
/// - requires: `source` is an absolute path and `out_dir` exists.
/// - ensures: compiler flags and environment from `cc::Build` are retained; the
///   Clang scanner receives the unwrapped compiler, while native GCC/MSVC
///   invocations retain the configured wrapper. Every scanner command selects
///   the same planned object destination as its compilation command.
/// - errors: process, scanner, JSON, rule-count, or unsupported lookup failure.
/// - panics: none.
pub fn scan_unit(
    tool: &cc::Tool,
    kind: CompilerKind,
    scanner: &Scanner,
    out_dir: &Path,
    source: PathBuf,
) -> Result<Scan, BuildError>
{
    let identity = source_identity(&source).to_hex();
    let object = out_dir.join(format!(
        "unit-{identity}.{}",
        if matches!(kind, CompilerKind::Msvc) {
            "obj"
        }
        else {
            "o"
        }
    ));
    let json = match kind {
        | CompilerKind::Clang => {
            let (scanner, resource_dir) = match *scanner {
                | Scanner::Clang {
                    ref path,
                    ref resource_dir,
                } => (path, resource_dir),
                | Scanner::Native => return Err(BuildError::InvalidCompilerConfiguration),
            };
            let mut command = Command::new(scanner);
            let compiler = compiler_path(tool)?;
            let mut resource_flag = OsString::from("-resource-dir=");
            resource_flag.push(resource_dir);
            // Before the caller's flags, so an explicit `-resource-dir` still
            // wins.
            command
                .arg("-format=p1689")
                .arg("--")
                .arg(compiler)
                .arg(resource_flag);
            command.args(tool.args());
            command.envs(tool.env().iter().map(|pair| (&pair.0, &pair.1)));
            ensure_standard(tool, kind, &mut command);
            command
                .arg("-x")
                .arg("c++")
                .arg("-c")
                .arg(&source)
                .arg("-o")
                .arg(&object);
            let output = command.output().map_err(|error| BuildError::Io {
                operation: "launch Clang dependency scanner",
                path: source.clone(),
                source: error,
            })?;
            if !output.status.success() {
                return Err(BuildError::ScanFailed {
                    source,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
            output.stdout
        },
        | CompilerKind::Gcc => {
            let deps = object.with_extension("p1689.json");
            let mut command = tool.to_command();
            ensure_standard(tool, kind, &mut command);
            command
                .arg("-fmodules-ts")
                .arg("-fdeps-format=p1689r5")
                .arg(path_argument("-fdeps-file=".as_ref(), &deps))
                .arg(path_argument("-fdeps-target=".as_ref(), &object))
                .arg("-E")
                .arg("-MD")
                .arg("-MF")
                .arg(object.with_extension("scan.d"))
                .arg("-x")
                .arg("c++")
                .arg(&source)
                .stdout(Stdio::null());
            let output = command.output().map_err(|error| BuildError::Io {
                operation: "launch GCC dependency scanner",
                path: source.clone(),
                source: error,
            })?;
            if !output.status.success() {
                return Err(BuildError::ScanFailed {
                    source,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
            fs::read(&deps).map_err(|error| BuildError::Io {
                operation: "read GCC dependency file",
                path: deps,
                source: error,
            })?
        },
        | CompilerKind::Msvc => {
            let deps = object.with_extension("p1689.json");
            let mut command = tool.to_command();
            ensure_standard(tool, kind, &mut command);
            command
                .arg("/scanDependencies")
                .arg(&deps)
                .arg("/interface")
                .arg(path_argument("/Fo".as_ref(), &object))
                .arg("/Tp")
                .arg(&source);
            let output = command.output().map_err(|error| BuildError::Io {
                operation: "launch MSVC dependency scanner",
                path: source.clone(),
                source: error,
            })?;
            if !output.status.success() {
                return Err(BuildError::ScanFailed {
                    source,
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
            fs::read(&deps).map_err(|error| BuildError::Io {
                operation: "read MSVC dependency file",
                path: deps,
                source: error,
            })?
        },
    };
    fs::write(object.with_extension("p1689.json"), &json).map_err(|error| BuildError::Io {
        operation: "retain structured dependency artifact",
        path: object.with_extension("p1689.json"),
        source: error,
    })?;
    Ok(Scan {
        source,
        object,
        json,
    })
}

/// Borrow a complete dependency rule from session-owned scanner output.
///
/// # Specification
/// - ensures: retains every P1689 rule and module field without owning copies
///   of unescaped strings; exactly one rule and at most one provider are
///   admitted.
/// - fails: invalid JSON, rule count or multiple providers.
/// - panics: none.
///
/// # Errors
/// Returns the parser or named scan-shape failure.
///
/// # Adequacy
/// - hypothesis: scanner buffers outlive every parsed string by construction;
///   metadata fixtures distinguish loss at this ownership boundary.
/// - witness: parser and native module smoke scenarios.
pub fn parse_scan(scan: &Scan) -> Result<Unit<'_>, BuildError>
{
    let file = DepFile::parse(p1689::JsonInput(&scan.json)).map_err(|error| {
        BuildError::InvalidDependencies {
            source: scan.source.clone(),
            error,
        }
    })?;
    let mut rules = file.rules;
    if rules.len() != 1 {
        return Err(BuildError::InvalidRules {
            source: scan.source.clone(),
        });
    }
    let rule = rules.pop().ok_or_else(|| BuildError::InvalidRules {
        source: scan.source.clone(),
    })?;
    if rule.provides.len() > 1 {
        return Err(BuildError::MultipleInterfaces {
            source: scan.source.clone(),
        });
    }
    Ok(Unit {
        source: &scan.source,
        object: &scan.object,
        rule,
    })
}

/// Planned BMI destination for one node in the unit arena.
#[derive(Debug)]
pub enum InterfacePath
{
    /// The unit produces no BMI.
    Absent,
    /// Compiler-reported or explicitly planned destination.
    Produced(PathBuf),
}

/// Resolve a provider index to its advertised artifact destination.
///
/// # Specification
/// - ensures: returns a path only for a producing node.
/// - fails: invalid index or a node without a BMI.
/// - panics: none.
///
/// # Errors
/// Returns `GraphInvariant` for an inconsistent plan.
///
/// # Adequacy
/// - hypothesis: producer and nonproducer nodes cannot be confused.
/// - witness: native module smoke scenarios.
pub fn artifact_path(
    interfaces: &[InterfacePath],
    index: UnitIndex,
) -> Result<&Path, BuildError>
{
    let destination = interfaces.get(index.0).ok_or(BuildError::GraphInvariant)?;
    match *destination {
        | InterfacePath::Produced(ref path) => Ok(path),
        | InterfacePath::Absent => Err(BuildError::GraphInvariant),
    }
}

/// Plan destinations without replacing compiler-reported BMI paths.
///
/// # Specification
/// - ensures: relative reported paths use rule or invocation working directory;
///   only absent paths use the adapter's per-node output mapping.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: explicit paths override generated names without changing node
///   identity.
/// - witness: artifact binding and native smoke scenarios.
pub fn interface_paths(
    units: &[Unit<'_>],
    kind: CompilerKind,
    out_dir: &Path,
    cwd: &Path,
) -> Result<Vec<InterfacePath>, BuildError>
{
    let mut reported = alloc::collections::BTreeMap::new();
    for unit in units {
        let base = rule_directory(unit, cwd)?;
        for module in unit
            .rule
            .provides
            .iter()
            .map(|provided| &provided.desc)
            .chain(unit.rule.requires.iter().map(|required| &required.desc))
        {
            if let Some(path) = module.compiled_module_path.as_deref() {
                let identity = crate::graph::identity(module, unit, cwd)?;
                let path = native_text(ScannerBytes(path), unit.source)?;
                let path = base.join(path);
                if let Some(previous) = reported.insert(identity, path.clone())
                    && previous != path
                {
                    return Err(BuildError::Io {
                        operation: "bind reported module artifact",
                        path,
                        source: io::Error::new(
                            io::ErrorKind::InvalidData,
                            "one module identity reports conflicting compiled-module paths",
                        ),
                    });
                }
            }
        }
    }
    units
        .iter()
        .map(|unit| {
            let Some(module) = unit.rule.provides.first()
            else {
                return Ok(InterfacePath::Absent);
            };
            let identity = crate::graph::identity(&module.desc, unit, cwd)?;
            let path = reported
                .remove(&identity)
                .unwrap_or_else(|| interface_path(kind, out_dir, unit.source));
            Ok(InterfacePath::Produced(path))
        })
        .collect()
}

/// Emit the GCC mapper from node-indexed artifact destinations.
///
/// # Specification
/// - ensures: each provided name maps to its advertised destination.
/// - fails: directory, mapper creation or write errors.
/// - panics: none.
///
/// # Errors
/// Returns a contextual filesystem or graph error.
///
/// # Adequacy
/// - hypothesis: the GCC smoke consumer loads the mapped partition artifacts.
/// - witness: native GCC module smoke scenario.
pub fn write_mapper(
    kind: CompilerKind,
    out_dir: &Path,
    units: &[Unit<'_>],
    interfaces: &[InterfacePath],
) -> Result<Mapper, BuildError>
{
    if !matches!(kind, CompilerKind::Gcc) {
        return Ok(Mapper::NotRequired);
    }
    let path = out_dir.join("module-mapper.txt");
    let mut writer = Vec::new();
    for (index, unit) in units.iter().enumerate() {
        if let Some(module) = unit.rule.provides.first() {
            let bmi = artifact_path(interfaces, UnitIndex(index))?;
            writer.extend_from_slice(module.desc.logical_name.as_ref());
            writer.push(b' ');
            writer.extend_from_slice(bmi.as_os_str().as_encoded_bytes());
            writer.push(b'\n');
        }
    }
    match fs::read(&path) {
        | Ok(previous) if previous == writer => return Ok(Mapper::Gcc(path)),
        | Ok(_) => {},
        | Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        | Err(source) => {
            return Err(BuildError::Io {
                operation: "read GCC module mapper",
                path,
                source,
            });
        },
    }
    fs::write(&path, writer).map_err(|source| BuildError::Io {
        operation: "write GCC module mapper",
        path: path.clone(),
        source,
    })?;
    Ok(Mapper::Gcc(path))
}

/// Compile a ready unit with its imported artifacts already materialized.
///
/// # Specification
/// - requires: every imported provider is complete; mapper and destinations
///   belong to the same arena; compiler wrapper and caller flags remain intact.
/// - ensures: successful compilation produces the object and advertised BMI;
///   GCC and Clang emit an ordinary depfile including system headers.
/// - fails: inconsistent graph, launch, native exit, or missing artifact.
/// - panics: none.
///
/// # Errors
/// Returns the contextual graph, compiler or artifact failure.
///
/// # Adequacy
/// - hypothesis: linked importer results distinguish stale or missing producer
///   artifacts; deleted BMIs cannot masquerade as successful object-only hits.
/// - witness: native module and cache restoration scenarios.
pub fn compile_unit(
    session: &crate::cache_run::Session<'_>,
    index: UnitIndex,
    imports: &[UnitIndex],
    units: &[Unit<'_>],
    interfaces: &[InterfacePath],
    mapper: &Mapper,
) -> Result<(), BuildError>
{
    let unit = units.get(index.0).ok_or(BuildError::GraphInvariant)?;
    let tool = session.tool;
    let kind = session.kind;
    let mut imported_paths = alloc::collections::BTreeSet::new();
    for imported in imports {
        imported_paths.insert(artifact_path(interfaces, *imported)?.to_path_buf());
        if !matches!(kind, CompilerKind::Msvc) {
            let provider = units.get(imported.0).ok_or(BuildError::GraphInvariant)?;
            let path = provider.object.with_extension("d");
            imported_paths.extend(crate::depfile::read(&path, session.cwd).map_err(|source| {
                BuildError::Io {
                    operation: "read imported module textual inputs",
                    path,
                    source,
                }
            })?);
        }
    }
    let mut command = tool.to_command();
    ensure_standard(tool, kind, &mut command);
    match kind {
        | CompilerKind::Clang | CompilerKind::Msvc => {
            for required in imports {
                let provider = units.get(required.0).ok_or(BuildError::GraphInvariant)?;
                let provided = provider
                    .rule
                    .provides
                    .first()
                    .ok_or(BuildError::GraphInvariant)?;
                let path = artifact_path(interfaces, *required)?;
                let name = native_text(
                    ScannerBytes(provided.desc.logical_name.as_ref()),
                    provider.source,
                )?;
                let prefix = if matches!(kind, CompilerKind::Clang) {
                    "-fmodule-file="
                }
                else {
                    command.arg("/reference");
                    ""
                };
                let mut argument = OsString::with_capacity(
                    prefix
                        .len()
                        .saturating_add(name.len())
                        .saturating_add(1)
                        .saturating_add(path.as_os_str().len()),
                );
                argument.push(prefix);
                argument.push(name);
                argument.push("=");
                argument.push(path);
                command.arg(argument);
            }
            if let Some(provided) = unit.rule.provides.first() {
                let path = artifact_path(interfaces, index)?;
                if matches!(kind, CompilerKind::Clang) {
                    command
                        .arg("-x")
                        .arg("c++-module")
                        .arg(path_argument("-fmodule-output=".as_ref(), path));
                }
                else {
                    command.arg(if provided.is_interface.0 {
                        "/interface"
                    }
                    else {
                        "/internalPartition"
                    });
                    command.arg("/ifcOutput").arg(path);
                }
            }
            else if matches!(kind, CompilerKind::Clang) {
                command.arg("-x").arg("c++");
            }
            if matches!(kind, CompilerKind::Clang) {
                command
                    .arg("-MD")
                    .arg("-MF")
                    .arg(unit.object.with_extension("d"))
                    .arg("-c")
                    .arg(unit.source)
                    .arg("-o")
                    .arg(unit.object);
            }
            else {
                command
                    .arg("/TP")
                    .arg("/c")
                    .arg(unit.source)
                    .arg(path_argument("/Fo".as_ref(), unit.object));
            }
        },
        | CompilerKind::Gcc => {
            let path = match *mapper {
                | Mapper::Gcc(ref path) => path,
                | Mapper::NotRequired => return Err(BuildError::InvalidCompilerConfiguration),
            };
            command
                .arg("-fmodules-ts")
                .arg(path_argument("-fmodule-mapper=".as_ref(), path))
                .arg("-MD")
                .arg("-Mno-modules")
                .arg("-MF")
                .arg(unit.object.with_extension("d"))
                .arg("-x")
                .arg("c++")
                .arg("-c")
                .arg(unit.source)
                .arg("-o")
                .arg(unit.object);
        },
    }
    let mut destinations = vec![crate::cache::Destination {
        role: crate::cache::Role::Object,
        path: unit.object.to_path_buf(),
    }];
    if !unit.rule.provides.is_empty() {
        destinations.push(crate::cache::Destination {
            role: crate::cache::Role::Interface,
            path: artifact_path(interfaces, index)?.to_path_buf(),
        });
    }
    if !matches!(kind, CompilerKind::Msvc) {
        destinations.push(crate::cache::Destination {
            role: crate::cache::Role::Depfile,
            path: unit.object.with_extension("d"),
        });
    }
    destinations.push(crate::cache::Destination {
        role: crate::cache::Role::Structured,
        path: unit.object.with_extension("p1689.json"),
    });
    if let Mapper::Gcc(ref path) = *mapper {
        destinations.push(crate::cache::Destination {
            role: crate::cache::Role::Mapping,
            path: path.clone(),
        });
    }
    let base = rule_directory(unit, session.cwd)?;
    for reported in unit.rule.primary_output.iter().chain(&unit.rule.outputs) {
        let path = native_text(ScannerBytes(reported.as_ref()), unit.source)?;
        let path = base.join(path);
        if !destinations
            .iter()
            .any(|destination| destination.path == path)
        {
            destinations.push(crate::cache::Destination {
                role: crate::cache::Role::Additional,
                path,
            });
        }
    }
    crate::cache_run::execute(session, &mut command, crate::cache_run::Invocation {
        source: unit.source,
        object: unit.object,
        destinations,
        imports: imported_paths.into_iter().collect(),
    })
}

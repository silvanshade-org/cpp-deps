use alloc::collections::BTreeMap;
use std::env;
use std::fs;
use std::fs::File;
use std::io::BufWriter;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use p1689::r5::DepFile;
use p1689::r5::LookupMethod;
use p1689::r5::ModuleName;

use crate::BuildError;
use crate::graph::Unit;

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
    Clang(PathBuf),
    /// GCC or MSVC scans with the compiler itself.
    Native,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct UnitIndex(pub usize);

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
    index: UnitIndex,
) -> PathBuf
{
    let suffix = match kind {
        | CompilerKind::Clang => "pcm",
        | CompilerKind::Gcc => "gcm",
        | CompilerKind::Msvc => "ifc",
    };
    out_dir.join(format!("module-{}.{suffix}", index.0))
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
///   invocations retain the configured wrapper.
/// - errors: process, scanner, JSON, rule-count, or unsupported lookup failure.
/// - panics: none.
pub fn scan_unit(
    tool: &cc::Tool,
    kind: CompilerKind,
    scanner: &Scanner,
    out_dir: &Path,
    index: UnitIndex,
    source: PathBuf,
) -> Result<Unit, BuildError>
{
    let object = out_dir.join(format!(
        "unit-{}.{}",
        index.0,
        if matches!(kind, CompilerKind::Msvc) {
            "obj"
        }
        else {
            "o"
        }
    ));
    let json = match kind {
        | CompilerKind::Clang => {
            let scanner = match *scanner {
                | Scanner::Clang(ref path) => path,
                | Scanner::Native => return Err(BuildError::InvalidCompilerConfiguration),
            };
            let mut command = Command::new(scanner);
            let compiler = compiler_path(tool)?;
            command.arg("-format=p1689").arg("--").arg(compiler);
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
            let deps = out_dir.join(format!("unit-{}.p1689.json", index.0));
            let mut command = tool.to_command();
            ensure_standard(tool, kind, &mut command);
            command
                .arg("-fmodules-ts")
                .arg("-fdeps-format=p1689r5")
                .arg(format!("-fdeps-file={}", deps.display()))
                .arg(format!("-fdeps-target={}", object.display()))
                .arg("-E")
                .arg("-MD")
                .arg("-MF")
                .arg(out_dir.join(format!("unit-{}.scan.d", index.0)))
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
            let deps = out_dir.join(format!("unit-{}.p1689.json", index.0));
            let mut command = tool.to_command();
            ensure_standard(tool, kind, &mut command);
            command
                .arg("/scanDependencies")
                .arg(&deps)
                .arg("/interface")
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
    let file: DepFile =
        serde_json::from_slice(&json).map_err(|error| BuildError::InvalidDependencies {
            source: source.clone(),
            error,
        })?;
    let mut rules = file.rules;
    if rules.len() != 1 {
        return Err(BuildError::InvalidRules { source });
    }
    let Some(rule) = rules.pop()
    else {
        return Err(BuildError::InvalidRules { source });
    };
    let provides: Vec<_> = rule
        .provides
        .into_iter()
        .filter(|module| module.is_interface.0)
        .map(|module| module.desc.logical_name)
        .collect();
    if provides.len() > 1 {
        return Err(BuildError::MultipleInterfaces { source });
    }
    let mut requires = Vec::with_capacity(rule.requires.len());
    for required in rule.requires {
        if required.lookup_method != LookupMethod::ByName {
            return Err(BuildError::UnsupportedLookup {
                source,
                module: required.desc.logical_name,
            });
        }
        requires.push(required.desc.logical_name);
    }
    Ok(Unit {
        source,
        object,
        provides,
        requires,
    })
}

/// Emit a GCC mapper that points every logical name at a unique path in
/// `OUT_DIR`.
///
/// # Specification
/// - ensures: GCC emits and loads its GCM files under `OUT_DIR` without
///   changing the build-script cwd (and invalidating relative include
///   directories).
/// - errors: mapper file could not be created or populated.
/// - panics: none.
pub fn write_mapper(
    kind: CompilerKind,
    out_dir: &Path,
    interfaces: &BTreeMap<ModuleName, PathBuf>,
) -> Result<Mapper, BuildError>
{
    if !matches!(kind, CompilerKind::Gcc) {
        return Ok(Mapper::NotRequired);
    }
    let path = out_dir.join("module-mapper.txt");
    let file = File::create(&path).map_err(|source| BuildError::Io {
        operation: "create GCC module mapper",
        path: path.clone(),
        source,
    })?;
    let mut writer = BufWriter::new(file);
    for (name, bmi) in interfaces {
        writeln!(writer, "{} {}", name.as_ref(), bmi.display()).map_err(|source| {
            BuildError::Io {
                operation: "write GCC module mapper",
                path: path.clone(),
                source,
            }
        })?;
    }
    writer.flush().map_err(|source| BuildError::Io {
        operation: "flush GCC module mapper",
        path: path.clone(),
        source,
    })?;
    Ok(Mapper::Gcc(path))
}

/// Resolve a planned provider without partial indexing or copying its name.
///
/// # Specification
/// - ensures: a provider index identifies one scanned interface.
/// - errors: an impossible graph index or provider state is reported, not
///   panicked.
/// - panics: none.
fn required_name(
    units: &[Unit],
    index: UnitIndex,
) -> Result<&ModuleName, BuildError>
{
    units
        .get(index.0)
        .and_then(|unit| unit.provides.first())
        .ok_or(BuildError::GraphInvariant)
}

/// Compile a ready unit with its required interfaces already available.
///
/// # Specification
/// - requires: `plan` placed each provider before this unit and the mapper is
///   present for GCC; flags/includes/environment/wrapper come from `cc::Build`.
/// - ensures: object and any interface land at their advertised paths, with
///   `-MD` and a per-unit depfile for native ccache depend mode.
/// - errors: process launch or nonzero native exit with source and stderr.
/// - panics: none.
pub fn compile_unit(
    tool: &cc::Tool,
    kind: CompilerKind,
    unit: &Unit,
    imports: &[UnitIndex],
    units: &[Unit],
    interfaces: &BTreeMap<ModuleName, PathBuf>,
    mapper: &Mapper,
) -> Result<(), BuildError>
{
    let mut command = tool.to_command();
    ensure_standard(tool, kind, &mut command);
    match kind {
        | CompilerKind::Clang => {
            for &required in imports {
                let name = required_name(units, required)?;
                let Some(path) = interfaces.get(name)
                else {
                    return Err(BuildError::MissingImport {
                        module: name.clone(),
                        source: unit.source.clone(),
                    });
                };
                command.arg(format!(
                    "-fmodule-file={}={}",
                    name.as_ref(),
                    path.display()
                ));
            }
            if let Some(provided) = unit.provides.first() {
                let Some(path) = interfaces.get(provided)
                else {
                    return Err(BuildError::MissingImport {
                        module: provided.clone(),
                        source: unit.source.clone(),
                    });
                };
                command.arg("-x").arg("c++-module");
                command.arg(format!("-fmodule-output={}", path.display()));
            }
            else {
                command.arg("-x").arg("c++");
            }
            command
                .arg("-MD")
                .arg("-MF")
                .arg(unit.object.with_extension("d"));
            command
                .arg("-c")
                .arg(&unit.source)
                .arg("-o")
                .arg(&unit.object);
        },
        | CompilerKind::Gcc => {
            let mapper = match *mapper {
                | Mapper::Gcc(ref path) => path,
                | Mapper::NotRequired => return Err(BuildError::InvalidCompilerConfiguration),
            };
            command
                .arg("-fmodules-ts")
                .arg(format!("-fmodule-mapper={}", mapper.display()))
                .arg("-MD")
                .arg("-MF")
                .arg(unit.object.with_extension("d"))
                .arg("-x")
                .arg("c++")
                .arg("-c")
                .arg(&unit.source)
                .arg("-o")
                .arg(&unit.object);
        },
        | CompilerKind::Msvc => {
            for &required in imports {
                let name = required_name(units, required)?;
                let Some(path) = interfaces.get(name)
                else {
                    return Err(BuildError::MissingImport {
                        module: name.clone(),
                        source: unit.source.clone(),
                    });
                };
                command
                    .arg("/reference")
                    .arg(format!("{}={}", name.as_ref(), path.display()));
            }
            if let Some(provided) = unit.provides.first() {
                let Some(path) = interfaces.get(provided)
                else {
                    return Err(BuildError::MissingImport {
                        module: provided.clone(),
                        source: unit.source.clone(),
                    });
                };
                command.arg("/interface");
                command.arg("/ifcOutput").arg(path);
            }
            command
                .arg("/TP")
                .arg("/c")
                .arg(&unit.source)
                .arg(format!("/Fo{}", unit.object.display()));
        },
    }
    let output = command.output().map_err(|source| BuildError::Io {
        operation: "launch C++ compiler",
        path: unit.source.clone(),
        source,
    })?;
    if !output.status.success() {
        return Err(BuildError::CompileFailed {
            source: unit.source.clone(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

//! Cargo build-script API for C++20 named-module dependency scanning and
//! compilation.
//!
//! Configure a [`cc::Build`] as usual, then pass it to [`ModuleBuild`]. The
//! build scans all sources into P1689 rules, checks the dependency graph, and
//! compiles each ready layer with the configured compiler. Compiled objects and
//! module interfaces are returned under `OUT_DIR`; the caller can link the
//! objects.

extern crate alloc;

/// Topological scheduling and named provider resolution.
pub(crate) mod graph;
/// Compiler-specific P1689 scanning and module artifact generation.
pub(crate) mod runner;

use alloc::collections::BTreeMap;
use alloc::string::String;
use core::fmt;
use core::num::NonZeroUsize;
use std::env;
use std::fs;
use std::io;
use std::io::Write as _;
use std::path::PathBuf;
use std::thread;

use graph::plan;
use p1689::r5::ModuleName;
use runner::CompilerKind;
use runner::Scanner;
use runner::UnitIndex;

/// Compiler and scanner configuration for a set of C++20 translation units.
pub struct ModuleBuild
{
    /// Configured compiler, flags and include directories from the caller.
    build: cc::Build,
    /// Sources in caller order, before scanning resolves their absolute paths.
    sources: Vec<PathBuf>,
    /// Explicit output root, or Cargo-provided `OUT_DIR` when absent.
    out_dir: Option<PathBuf>,
    /// Clang dependency scanner override, if configured.
    scanner: Option<PathBuf>,
    /// Maximum number of ready units built at the same time.
    parallelism: NonZeroUsize,
}

impl ModuleBuild
{
    /// Adopt the flags, includes, environment, compiler wrapper, and target
    /// from `cc::Build`.
    ///
    /// # Specification
    /// - requires: a configured C++ compiler can be resolved by `cc::Build`.
    /// - ensures: compiler selection and caller-provided settings remain
    ///   attached to the build.
    /// - panics: none.
    ///
    /// # Adequacy
    /// - hypothesis: the smoke build exercises caller include paths and both
    ///   compiler families.
    /// - witness: crates/module-smoke/build.rs
    #[must_use]
    #[inline]
    pub fn new(mut build: cc::Build) -> Self
    {
        build.cpp(true);
        Self {
            build,
            sources: Vec::new(),
            out_dir: None,
            scanner: None,
            parallelism: thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
        }
    }

    /// Add a module interface, implementation, partition, or importer source.
    ///
    /// # Specification
    /// - ensures: the source is scanned once and retains its insertion order
    ///   among peers.
    /// - panics: none.
    #[inline]
    pub fn source<S>(
        &mut self,
        source: S,
    ) -> &mut Self
    where
        S: Into<PathBuf>,
    {
        self.sources.push(source.into());
        self
    }

    /// Use an explicit output directory rather than Cargo's `OUT_DIR`.
    ///
    /// # Specification
    /// - ensures: generated dependency files, objects, and BMIs reside beneath
    ///   this directory.
    /// - panics: none.
    #[inline]
    pub fn out_dir<P>(
        &mut self,
        out_dir: P,
    ) -> &mut Self
    where
        P: Into<PathBuf>,
    {
        self.out_dir = Some(out_dir.into());
        self
    }

    /// Override the `clang-scan-deps` executable selected for Clang scanning.
    ///
    /// # Specification
    /// - ensures: the scanner override applies only to Clang, not the actual
    ///   compiler.
    /// - panics: none.
    #[inline]
    pub fn scanner<P>(
        &mut self,
        scanner: P,
    ) -> &mut Self
    where
        P: Into<PathBuf>,
    {
        self.scanner = Some(scanner.into());
        self
    }

    /// Set the maximum number of independent units compiled concurrently.
    ///
    /// # Specification
    /// - ensures: dependency layers remain ordered regardless of this limit.
    /// - panics: none.
    #[inline]
    pub fn parallelism(
        &mut self,
        parallelism: NonZeroUsize,
    ) -> &mut Self
    {
        self.parallelism = parallelism;
        self
    }

    /// Scan, order, and compile all sources; return linkable objects and named
    /// BMIs.
    ///
    /// # Specification
    /// - requires: the configured compiler implements C++20 modules and the
    ///   source list is nonempty.
    /// - ensures: every imported interface is available before the importing
    ///   unit runs; independent units compile in parallel, and all generated
    ///   files live under `OUT_DIR`.
    /// - errors: an absent output directory or source, scan failure, malformed
    ///   P1689, duplicate provider, missing import, cycle, unsupported
    ///   compiler, or native failure.
    /// - panics: none; a native worker panic is returned with its source path.
    ///
    /// # Adequacy
    /// - hypothesis: a partition, interface, and importer compile, link, and
    ///   execute with relative include paths under both GCC 16 and Clang 22,
    ///   while graph tests reject missing, duplicate, and cyclic dependencies.
    /// - witness: crates/module-smoke/src/lib.rs;
    ///   `graph::tests::ordering_and_named_failures`
    ///
    /// # Errors
    /// Returns a named [`BuildError`] for an unavailable compiler or source,
    /// invalid P1689 output, an unschedulable graph, or a failed native
    /// compiler.
    #[inline]
    pub fn compile(mut self) -> Result<BuildOutput, BuildError>
    {
        if self.sources.is_empty() {
            return Err(BuildError::NoSources);
        }
        let out_dir = self
            .out_dir
            .or_else(|| env::var_os("OUT_DIR").map(PathBuf::from))
            .ok_or(BuildError::MissingOutDir)?;
        fs::create_dir_all(&out_dir).map_err(|source| BuildError::Io {
            operation: "create output directory",
            path: out_dir.clone(),
            source,
        })?;
        let out_dir = fs::canonicalize(&out_dir).map_err(|source| BuildError::Io {
            operation: "resolve output directory",
            path: out_dir,
            source,
        })?;
        self.build.out_dir(&out_dir);
        let tool = self
            .build
            .try_get_compiler()
            .map_err(BuildError::Compiler)?;
        let kind = CompilerKind::from_tool(&tool)?;
        let scanner = match kind {
            | CompilerKind::Clang => runner::clang_scanner(&tool, self.scanner)?,
            | CompilerKind::Gcc | CompilerKind::Msvc => Scanner::Native,
        };

        let mut units = Vec::with_capacity(self.sources.len());
        let cargo_output = io::stdout();
        let mut cargo_output = cargo_output.lock();
        for (index, source) in self.sources.iter().enumerate() {
            let source = fs::canonicalize(source).map_err(|error| BuildError::Io {
                operation: "resolve source",
                path: source.clone(),
                source: error,
            })?;
            writeln!(cargo_output, "cargo:rerun-if-changed={}", source.display()).map_err(
                |error| BuildError::Io {
                    operation: "emit Cargo rerun marker",
                    path: source.clone(),
                    source: error,
                },
            )?;
            let unit =
                runner::scan_unit(&tool, kind, &scanner, &out_dir, UnitIndex(index), source)?;
            units.push(unit);
        }
        drop(cargo_output);
        let ordering = plan(&units)?;
        let interfaces: BTreeMap<_, _> = ordering
            .providers
            .into_iter()
            .map(|(name, index)| {
                (
                    name,
                    runner::interface_path(kind, &out_dir, UnitIndex(index)),
                )
            })
            .collect();
        let mapper = runner::write_mapper(kind, &out_dir, &interfaces)?;

        for layer in &ordering.layers {
            for batch in layer.chunks(self.parallelism.get()) {
                compile_batch(&units, batch, |index| {
                    let unit = units.get(index.0).ok_or(BuildError::GraphInvariant)?;
                    let imports = ordering
                        .imports
                        .get(index.0)
                        .ok_or(BuildError::GraphInvariant)?;
                    runner::compile_unit(&tool, kind, unit, imports, &units, &interfaces, &mapper)
                })?;
            }
        }
        Ok(BuildOutput {
            objects: units.into_iter().map(|unit| unit.object).collect(),
            interfaces,
        })
    }
}
/// Compile one independent batch concurrently and join every worker.
///
/// # Specification
/// - requires: each batch index names an independent unit in `units`.
/// - ensures: every worker starts before the first is joined, and every worker
///   finishes before this function returns.
/// - errors: an invalid index, worker panic, or compiler failure.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: a serialized batch cannot pass two workers waiting for each
///   other to start, regardless of their launch order.
/// - witness: `tests::independent_units_overlap`
fn compile_batch<F>(
    units: &[graph::Unit],
    batch: &[UnitIndex],
    execute: F,
) -> Result<(), BuildError>
where
    F: Fn(UnitIndex) -> Result<(), BuildError> + Sync,
{
    thread::scope(|scope| -> Result<(), BuildError> {
        let mut workers = Vec::with_capacity(batch.len());
        let execute = &execute;
        for &index in batch {
            let unit = units.get(index.0).ok_or(BuildError::GraphInvariant)?;
            workers.push((unit, scope.spawn(move || execute(index))));
        }
        for (unit, worker) in workers {
            let result = worker.join().map_err(|panic| {
                let reason = panic.downcast_ref::<String>().map_or_else(
                    || {
                        panic.downcast_ref::<&str>().map_or_else(
                            || String::from("non-string panic payload"),
                            |text| (*text).to_owned(),
                        )
                    },
                    Clone::clone,
                );
                BuildError::WorkerPanic {
                    source: unit.source.clone(),
                    reason,
                }
            })?;
            result?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests
{
    use core::sync::atomic::AtomicUsize;
    use core::sync::atomic::Ordering;
    use std::sync::Mutex;
    use std::sync::mpsc;

    use super::compile_batch;
    use crate::graph::Unit;
    use crate::runner::UnitIndex;

    #[test]
    fn independent_units_overlap()
    {
        let units = ["first.cpp", "second.cpp"].map(|name| Unit {
            source: name.into(),
            object: name.into(),
            provides: Vec::new(),
            internal_partition: false,
            requires: Vec::new(),
        });
        let (first_sender, first_receiver) = mpsc::channel();
        let (second_sender, second_receiver) = mpsc::channel();
        let first_receiver = Mutex::new(first_receiver);
        let second_receiver = Mutex::new(second_receiver);
        let observed = AtomicUsize::new(0);
        compile_batch(&units, &[UnitIndex(0), UnitIndex(1)], |index| {
            let elapsed = core::time::Duration::from_secs(2);
            let concurrent = if index == UnitIndex(0) {
                first_sender.send(()).expect("receiver remains live");
                second_receiver
                    .lock()
                    .expect("receiver lock")
                    .recv_timeout(elapsed)
                    .is_ok()
            }
            else {
                second_sender.send(()).expect("receiver remains live");
                first_receiver
                    .lock()
                    .expect("receiver lock")
                    .recv_timeout(elapsed)
                    .is_ok()
            };
            if concurrent {
                observed.fetch_add(1, Ordering::Relaxed);
            }
            Ok(())
        })
        .expect("independent batch");
        assert_eq!(
            observed.load(Ordering::Relaxed),
            2,
            "both units must start before either can complete"
        );
    }
}

/// Generated objects and named compiled-module interfaces under `OUT_DIR`.
pub struct BuildOutput
{
    /// Linkable objects in the order sources were added to [`ModuleBuild`].
    pub objects: Vec<PathBuf>,
    /// Compiled interface path for each provided logical module or partition.
    pub interfaces: BTreeMap<ModuleName, PathBuf>,
}

/// Named scan, planning, compiler, or filesystem failure.
#[derive(Debug)]
pub enum BuildError
{
    /// No translation units were added to the build.
    NoSources,
    /// Neither `OUT_DIR` nor an explicit output directory was set.
    MissingOutDir,
    /// `cc::Build` could not resolve the configured compiler.
    Compiler(cc::Error),
    /// A requested filesystem operation failed.
    Io
    {
        /// Operation attempted.
        operation: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: io::Error,
    },
    /// The selected compiler does not implement this scanner path.
    UnsupportedCompiler
    {
        /// Compiler executable.
        compiler: PathBuf,
    },
    /// The compiler could not answer a query about its own installation.
    CompilerQuery
    {
        /// Compiler executable.
        compiler: PathBuf,
        /// Query flag, such as `-print-resource-dir`.
        query: &'static str,
        /// Compiler standard error.
        stderr: String,
    },
    /// Scanner and compiler families were configured inconsistently.
    InvalidCompilerConfiguration,
    /// An internal graph index or dependency count was inconsistent.
    GraphInvariant,
    /// A scanner exited unsuccessfully for the named source.
    ScanFailed
    {
        /// Translation unit being scanned.
        source: PathBuf,
        /// Scanner standard error.
        stderr: String,
    },
    /// Scanner output is not a valid P1689 dependency file.
    InvalidDependencies
    {
        /// Translation unit being scanned.
        source: PathBuf,
        /// Parser diagnostic.
        error: serde_json::Error,
    },
    /// Scanner returned a rule count other than one for a translation unit.
    InvalidRules
    {
        /// Translation unit being scanned.
        source: PathBuf,
    },
    /// A source provides more than one named interface.
    MultipleInterfaces
    {
        /// Translation unit with multiple provided interfaces.
        source: PathBuf,
    },
    /// A header-unit import cannot be compiled through the named-module API.
    UnsupportedLookup
    {
        /// Importing translation unit.
        source: PathBuf,
        /// Required module or header name.
        module: ModuleName,
    },
    /// A module import has no provider in this build.
    MissingImport
    {
        /// Importing translation unit.
        source: PathBuf,
        /// Missing logical module name.
        module: ModuleName,
    },
    /// Two sources provide the same interface.
    DuplicateProvider
    {
        /// Duplicated logical module name.
        module: ModuleName,
        /// Earlier source.
        first: PathBuf,
        /// Later source.
        second: PathBuf,
    },
    /// The listed translation units contain a dependency cycle.
    Cycle
    {
        /// Translation units still blocked when scheduling stops.
        sources: Vec<PathBuf>,
        /// Logical modules provided by the blocked translation units.
        modules: Vec<ModuleName>,
    },
    /// A native compiler failed to build the named translation unit.
    CompileFailed
    {
        /// Translation unit being compiled.
        source: PathBuf,
        /// Compiler standard error.
        stderr: String,
    },
    /// A scoped native worker panicked instead of returning its error.
    WorkerPanic
    {
        /// Translation unit owned by the worker.
        source: PathBuf,
        /// Message carried by the panic payload.
        reason: String,
    },
}

impl fmt::Display for BuildError
{
    /// Format a source-specific diagnostic for Cargo build-script output.
    ///
    /// # Specification
    /// - ensures: every diagnostic includes its affected module or path when
    ///   available.
    /// - panics: none.
    #[inline]
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result
    {
        match *self {
            | Self::NoSources => write!(f, "no C++20 module sources were added"),
            | Self::MissingOutDir => write!(f, "OUT_DIR is not set"),
            | Self::Compiler(ref error) => write!(f, "cannot select C++ compiler: {error}"),
            | Self::Io {
                operation,
                ref path,
                ref source,
            } => write!(f, "cannot {operation} {}: {source}", path.display()),
            | Self::UnsupportedCompiler { ref compiler } => {
                write!(f, "unsupported module compiler {}", compiler.display())
            },
            | Self::CompilerQuery {
                ref compiler,
                query,
                ref stderr,
            } => write!(
                f,
                "{} could not answer {query}: {stderr}",
                compiler.display()
            ),
            | Self::InvalidCompilerConfiguration => {
                write!(f, "scanner or mapper does not match compiler family")
            },
            | Self::GraphInvariant => write!(f, "module dependency graph invariant violated"),
            | Self::ScanFailed {
                ref source,
                ref stderr,
            } => write!(
                f,
                "dependency scan failed for {}: {stderr}",
                source.display()
            ),
            | Self::InvalidDependencies {
                ref source,
                ref error,
            } => write!(f, "invalid P1689 output for {}: {error}", source.display()),
            | Self::InvalidRules { ref source } => {
                write!(f, "expected one dependency rule for {}", source.display())
            },
            | Self::MultipleInterfaces { ref source } => write!(
                f,
                "multiple interfaces from one source {}",
                source.display()
            ),
            | Self::UnsupportedLookup {
                ref source,
                ref module,
            } => write!(
                f,
                "unsupported header-unit import {} in {}",
                module.as_ref(),
                source.display()
            ),
            | Self::MissingImport {
                ref source,
                ref module,
            } => write!(
                f,
                "missing import {} required by {}",
                module.as_ref(),
                source.display()
            ),
            | Self::DuplicateProvider {
                ref module,
                ref first,
                ref second,
            } => write!(
                f,
                "duplicate provider of {}: {} and {}",
                module.as_ref(),
                first.display(),
                second.display()
            ),
            | Self::Cycle {
                ref sources,
                ref modules,
            } => {
                write!(f, "module dependency cycle involving ")?;
                for (index, module) in modules.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", module.as_ref())?;
                }
                write!(f, " (blocked sources: ")?;
                for (index, source) in sources.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", source.display())?;
                }
                write!(f, ")")
            },
            | Self::CompileFailed {
                ref source,
                ref stderr,
            } => write!(
                f,
                "C++ compilation failed for {}: {stderr}",
                source.display()
            ),
            | Self::WorkerPanic {
                ref source,
                ref reason,
            } => write!(
                f,
                "native worker panicked while compiling {}: {reason}",
                source.display()
            ),
        }
    }
}

impl core::error::Error for BuildError
{
}

//! Cargo build-script API for C++20 named-module dependency scanning and
//! compilation.
//!
//! Configure a [`cc::Build`] as usual, then pass it to [`ModuleBuild`]. The
//! build scans all sources into P1689 rules, checks the dependency graph, and
//! compiles each ready layer with the configured compiler. Compiled objects and
//! module interfaces are returned under `OUT_DIR`; the caller can link the
//! objects.

extern crate alloc;

/// Validated local module caching and immutable-toolchain attestations.
pub mod cache;
/// Compiler discovery and coherent operation execution.
pub(crate) mod cache_run;
/// Compiler textual dependency witnesses and Cargo input tracking.
pub(crate) mod depfile;
/// Topological scheduling and named provider resolution.
pub(crate) mod graph;
/// Compiler-specific P1689 scanning and module artifact generation.
pub(crate) mod runner;

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
    /// Reuse is opt-in and requires an explicit toolchain attestation.
    cache: cache::Policy,
    /// Shared capacity supplied by the caller; one implicit slot needs no
    /// token.
    jobserver: Option<jobserver::Client>,
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
            parallelism: NonZeroUsize::MAX,
            cache: cache::Policy::Disabled,
            jobserver: None,
        }
    }

    /// Enable complete local module caching with an attested toolchain.
    ///
    /// # Specification
    /// - requires: the cache configuration meets its immutable-toolchain and
    ///   cooperative-filesystem obligations.
    /// - ensures: current dependency discovery precedes every cache hit.
    /// - panics: none.
    #[inline]
    pub fn cache(
        &mut self,
        cache: cache::ModuleCache,
    ) -> &mut Self
    {
        self.cache = cache::Policy::Enabled(cache);
        self
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

    /// Share caller-owned jobserver capacity for additional compiler workers.
    ///
    /// # Specification
    /// - requires: caller owns one implicit execution slot and supplies the
    ///   enclosing build jobserver, not an independent replacement pool.
    /// - ensures: every additional worker holds a token until its batch joins;
    ///   absent a client, compilation remains serial.
    /// - panics: none.
    #[inline]
    pub fn jobserver(
        &mut self,
        client: jobserver::Client,
    ) -> &mut Self
    {
        self.jobserver = Some(client);
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
        let lock_path = out_dir.join(".cpp-deps.lock");
        let output_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| BuildError::Io {
                operation: "open output-session lock",
                path: lock_path.clone(),
                source,
            })?;
        output_lock.lock().map_err(|source| BuildError::Io {
            operation: "lock module output session",
            path: lock_path,
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
        for source in &self.sources {
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
            let unit = runner::scan_unit(&tool, kind, &scanner, &out_dir, source)?;
            units.push(unit);
        }
        drop(cargo_output);
        let scans = units;
        let units: Vec<_> = scans
            .iter()
            .map(runner::parse_scan)
            .collect::<Result<_, _>>()?;
        let cwd = env::current_dir().map_err(|source| BuildError::Io {
            operation: "read invocation directory",
            path: out_dir.clone(),
            source,
        })?;
        let ordering = plan(&units, &cwd)?;
        let interfaces = runner::interface_paths(&units, kind, &out_dir, &cwd)?;
        let mapper = runner::write_mapper(kind, &out_dir, &units, &interfaces)?;
        let session = cache_run::Session {
            tool: &tool,
            kind,
            cache: &self.cache,
            cwd: &cwd,
            output_root: &out_dir,
            owners: std::sync::Mutex::default(),
        };
        let shared = if let Some(ref client) = self.jobserver {
            let (sender, receiver) = std::sync::mpsc::channel();
            let helper = client
                .clone()
                .into_helper_thread(move |permit| {
                    drop(sender.send(permit));
                })
                .map_err(|source| BuildError::Io {
                    operation: "start shared compiler capacity acquisition",
                    path: out_dir.clone(),
                    source,
                })?;
            Some((helper, receiver))
        }
        else {
            None
        };
        let mut requested = 0_usize;
        let mut permits = Vec::new();
        let mut scratch = Vec::new();
        for layer in &ordering.layers {
            let mut pending = layer.as_slice();
            while !pending.is_empty() {
                if let Some((ref helper, ref receiver)) = shared {
                    let additional = pending
                        .len()
                        .min(self.parallelism.get())
                        .checked_sub(1)
                        .ok_or(BuildError::GraphInvariant)?;
                    while requested < additional {
                        helper.request_token();
                        requested = requested.checked_add(1).ok_or(BuildError::GraphInvariant)?;
                    }
                    while permits.len() < additional {
                        match receiver.try_recv() {
                            | Ok(permit) => {
                                requested =
                                    requested.checked_sub(1).ok_or(BuildError::GraphInvariant)?;
                                permits.push(permit.map_err(|source| BuildError::Io {
                                    operation: "acquire shared compiler capacity",
                                    path: out_dir.clone(),
                                    source,
                                })?);
                            },
                            | Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                return Err(BuildError::Io {
                                    operation: "receive shared compiler capacity",
                                    path: out_dir.clone(),
                                    source: io::Error::new(
                                        io::ErrorKind::BrokenPipe,
                                        "jobserver acquisition helper disconnected",
                                    ),
                                });
                            },
                        }
                    }
                }
                let count = permits
                    .len()
                    .checked_add(1)
                    .ok_or(BuildError::GraphInvariant)?;
                let (batch, remaining) = pending.split_at(count);
                compile_batch(&units, batch, &mut scratch, |index, scratch| {
                    let imports = scratch.resolve(&ordering, index)?;
                    runner::compile_unit(&session, index, imports, &units, &interfaces, &mapper)
                })?;
                permits.clear();
                pending = remaining;
            }
        }
        drop(shared);
        if !matches!(kind, CompilerKind::Msvc) {
            let mut dependencies = alloc::collections::BTreeSet::new();
            for unit in &units {
                let path = unit.object.with_extension("d");
                let inputs = depfile::read(&path, &cwd).map_err(|source| BuildError::Io {
                    operation: "read compiler textual dependencies",
                    path,
                    source,
                })?;
                dependencies.extend(inputs);
            }
            // Generated artifacts are outputs, not Cargo source dependencies.
            for unit in &units {
                dependencies.remove(unit.object);
            }
            for interface in &interfaces {
                if let runner::InterfacePath::Produced(ref path) = *interface {
                    dependencies.remove(path);
                }
            }
            if let runner::Mapper::Gcc(ref path) = mapper {
                dependencies.remove(path);
            }
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            for path in dependencies {
                let text = path
                    .to_str()
                    .filter(|text| !text.contains(['\r', '\n']))
                    .ok_or_else(|| BuildError::Io {
                        operation: "encode Cargo dependency path",
                        path: path.clone(),
                        source: io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Cargo dependency paths must be UTF-8 without line breaks",
                        ),
                    })?;
                writeln!(stdout, "cargo:rerun-if-changed={text}").map_err(|source| {
                    BuildError::Io {
                        operation: "emit Cargo dependency",
                        path,
                        source,
                    }
                })?;
            }
        }
        drop(ordering);
        let mut output = BuildOutput {
            objects: Vec::with_capacity(units.len()),
            interfaces: Vec::new(),
        };
        for (unit, destination) in units.into_iter().zip(interfaces) {
            output.objects.push(unit.object.to_path_buf());
            if let runner::InterfacePath::Produced(path) = destination
                && let Some(provided) = unit.rule.provides.into_iter().next()
            {
                let desc = provided.desc;
                output.interfaces.push(ModuleArtifact {
                    description: p1689::r5::ModuleDesc {
                        logical_name: ModuleName(alloc::borrow::Cow::Owned(
                            desc.logical_name.0.into_owned(),
                        )),
                        source_path: desc
                            .source_path
                            .map(|path| alloc::borrow::Cow::Owned(path.into_owned())),
                        compiled_module_path: desc
                            .compiled_module_path
                            .map(|path| alloc::borrow::Cow::Owned(path.into_owned())),
                        unique_on_source_path: desc.unique_on_source_path,
                    },
                    path,
                });
            }
        }
        Ok(output)
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
    units: &[graph::Unit<'_>],
    batch: &[UnitIndex],
    scratch: &mut Vec<graph::ImportScratch>,
    execute: F,
) -> Result<(), BuildError>
where
    F: Fn(UnitIndex, &mut graph::ImportScratch) -> Result<(), BuildError> + Sync,
{
    if scratch.len() < batch.len() {
        scratch.resize_with(batch.len(), graph::ImportScratch::default);
    }
    thread::scope(|scope| -> Result<(), BuildError> {
        let mut workers = Vec::with_capacity(batch.len());
        let execute = &execute;
        for (&index, scratch) in batch.iter().zip(scratch.iter_mut()) {
            let unit = units.get(index.0).ok_or(BuildError::GraphInvariant)?;
            workers.push((unit, scope.spawn(move || execute(index, scratch))));
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
                    source: unit.source.to_path_buf(),
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
            source: std::path::Path::new(name),
            object: std::path::Path::new(name),
            rule: p1689::r5::DepInfo::default(),
        });
        let (first_sender, first_receiver) = mpsc::channel();
        let (second_sender, second_receiver) = mpsc::channel();
        let first_receiver = Mutex::new(first_receiver);
        let second_receiver = Mutex::new(second_receiver);
        let observed = AtomicUsize::new(0);
        compile_batch(
            &units,
            &[UnitIndex(0), UnitIndex(1)],
            &mut Vec::new(),
            |index, _scratch| {
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
            },
        )
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
    pub interfaces: Vec<ModuleArtifact>,
}

/// Complete module identity and its produced BMI destination.
pub struct ModuleArtifact
{
    /// Reported identity and paths, owned across the invocation boundary.
    pub description: p1689::r5::ModuleDesc<'static>,
    /// Actual compiler destination for this artifact.
    pub path: PathBuf,
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
        error: p1689::ParseError,
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
        module: ModuleName<'static>,
    },
    /// A module import has no provider in this build.
    MissingImport
    {
        /// Importing translation unit.
        source: PathBuf,
        /// Missing logical module name.
        module: ModuleName<'static>,
    },
    /// Two sources provide the same interface.
    DuplicateProvider
    {
        /// Duplicated logical module name.
        module: ModuleName<'static>,
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
        modules: Vec<ModuleName<'static>>,
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
                module.as_ref().escape_ascii(),
                source.display()
            ),
            | Self::MissingImport {
                ref source,
                ref module,
            } => write!(
                f,
                "missing import {} required by {}",
                module.as_ref().escape_ascii(),
                source.display()
            ),
            | Self::DuplicateProvider {
                ref module,
                ref first,
                ref second,
            } => write!(
                f,
                "duplicate provider of {}: {} and {}",
                module.as_ref().escape_ascii(),
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
                    write!(f, "{}", module.as_ref().escape_ascii())?;
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

// TODO:
// - verify files are c++ (or rather, accurately report errors when they are not)
// - support header units
// - support adding custom module mappings
// - handle error propagation
// - add test for clang with both .cpp and .cppm (to ensure `is-interface` is used correctly)
// - don't clobber std setting (but ensure c++20 is set)
// - replace `pending_count` with a set of names
//   - we can use this information to determine which compilation tasks failed and remove them from the set on failure, so we don't prevent shutdown and deadlock

// NOTE:
// - all errors except for send and thread errors are forwarded through the iterator
// - send and thread errors are returned by the iterator at the end if an error occurred

// NOTE:
// - gcc does not populate the source-path field, thus we must track paths separately

extern crate alloc;

mod analyzer;
#[cfg(feature = "cc")]
mod compiler;
mod queue;
#[cfg(feature = "sink")]
mod sink;
#[cfg(test)]
mod testing;
mod vendor;
mod worker;

#[cfg(feature = "cc")]
use alloc::sync::Arc;
use core::{marker::PhantomData, num::NonZeroUsize};

use p1689::r5::{self, yoke::DepInfoYoke};
use queue::TaskQueue;

pub use crate::analyzer::CppDepsAnalyzer;
#[cfg(feature = "cc")]
use crate::compiler::Compiler;
#[cfg(feature = "sink")]
pub use crate::sink::CppDepsSink;

#[cfg(feature = "memchr")]
struct Finders {
    #[cfg(target_feature = "avx2")]
    dotted: memchr::arch::x86_64::avx2::memchr::One,
    #[cfg(all(not(target_feature = "avx2"), target_feature = "sse2"))]
    dotted: memchr::arch::x86_64::sse2::memchr::One,
    #[cfg(all(not(target_feature = "avx2"), not(target_feature = "sse2")))]
    dotted: memchr::arch::all::memchr::One,
}
#[cfg(feature = "memchr")]
impl Finders {
    fn new() -> Result<Self, InnerError> {
        #[cfg(target_feature = "avx2")]
        let dotted = memchr::arch::x86_64::avx2::memchr::One::new(b'.')
            .ok_or_else(|| InnerError::new(InnerErrorKind::MemChrNew))?;
        #[cfg(all(not(target_feature = "avx2"), target_feature = "sse2"))]
        let dotted = memchr::arch::x86_64::sse2::memchr::One::new(b'.')
            .ok_or_else(|| InnerError::new(InnerErrorKind::MemChrNew))?;
        #[cfg(all(not(target_feature = "avx2"), not(target_feature = "sse2")))]
        let dotted = memchr::arch::all::memchr::One::new(b'.');
        Ok(Self { dotted })
    }
}

struct InnerError {
    #[allow(unused)]
    kind: InnerErrorKind,
    #[allow(unused)]
    backtrace: std::backtrace::Backtrace,
}
impl InnerError {
    fn new(kind: InnerErrorKind) -> Self {
        let backtrace = std::backtrace::Backtrace::capture();
        Self { kind, backtrace }
    }
}
impl core::fmt::Debug for InnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InnerError")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
enum InnerErrorKind {
    AnalyzerAlreadyReceivedExpectsCount,
    #[cfg(feature = "cc")]
    AnalyzerFailedSendingCompileItem,
    BuilderFailedSendingCppDepsItem,
    #[cfg(feature = "cc")]
    CcTryGetCompiler {
        #[allow(unused)]
        err: cc::Error,
    },
    #[cfg(feature = "cc")]
    CommandStatus {
        #[allow(unused)]
        err: std::io::Error,
    },
    #[cfg(feature = "cc")]
    CommandCompilerNonZeroExit,
    #[cfg(feature = "cc")]
    CompilerFamilyTryFromUnknownFamily,
    DepFileParse,
    #[cfg(feature = "cc")]
    EnvVar {
        #[allow(unused)]
        err: std::env::VarError,
    },
    #[cfg(feature = "cc")]
    FileCreate {
        #[allow(unused)]
        err: std::io::Error,
    },
    FileOpen {
        #[allow(unused)]
        err: std::io::Error,
    },
    #[cfg(feature = "cc")]
    FsCreateDirAll {
        #[allow(unused)]
        err: std::io::Error,
    },
    #[cfg(all(feature = "cc", feature = "memchr"))]
    MemChrNew,
    MmapMap {
        #[allow(unused)]
        err: std::io::Error,
    },
    NonZeroUsizeTryFromUsize {
        #[allow(unused)]
        err: core::num::TryFromIntError,
    },
    #[cfg(feature = "cc")]
    PathStripPrefix {
        #[allow(unused)]
        err: std::path::StripPrefixError,
    },
    OrderingSolutionBlocked,
    #[cfg(all(feature = "cc", feature = "sink"))]
    SinkFailedSendingCppDepsItem,
    QueueFailedSendingCompileItem,
    WorkerFailedSendingAnalyzerItem,
    WorkerFailedSendingExpects,
}
impl core::fmt::Display for InnerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self, f)
    }
}
impl std::error::Error for InnerError {}

pub struct Error(InnerError);
impl core::fmt::Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.0, f)
    }
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "error: {:?}", self.0.kind)?;
        if matches!(self.0.backtrace.status(), std::backtrace::BacktraceStatus::Captured) {
            writeln!(f, "backtrace:\n{}", self.0.backtrace)?;
        }
        Ok(())
    }
}
impl std::error::Error for Error {}
impl From<InnerError> for Error {
    fn from(inner: InnerError) -> Self {
        Error(inner)
    }
}

#[derive(Clone)]
pub struct CppDepsSrc<P> {
    pub src_base: P,
    pub src_path: P,
}
impl<P> CppDepsSrc<P> {
    pub fn new(src_base: P, src_path: P) -> Self {
        Self { src_base, src_path }
    }
}

#[non_exhaustive]
pub enum CppDepsItem<P, B> {
    #[cfg(feature = "cc")]
    #[non_exhaustive]
    SrcFile { src_file: CppDepsSrc<P> },
    #[non_exhaustive]
    DepFile {
        src_file: Option<CppDepsSrc<P>>,
        dep_path: P,
    },
    #[non_exhaustive]
    DepText {
        src_file: Option<CppDepsSrc<P>>,
        dep_path: P,
        dep_text: B,
    },
    #[non_exhaustive]
    DepInfo {
        src_file: Option<CppDepsSrc<P>>,
        dep_info: DepInfoYoke,
    },
}

pub struct CppDeps<P = r5::Utf8PathBuf, B = Vec<u8>> {
    #[cfg(feature = "cc")]
    compiler: Arc<Compiler>,
    parallelism: NonZeroUsize,
    cppdeps_tx: flume::Sender<CppDepsItem<P, B>>,
    cppdeps_rx: flume::Receiver<CppDepsItem<P, B>>,
    p: PhantomData<P>,
    b: PhantomData<B>,
}

impl<P, B> CppDeps<P, B>
where
    P: AsRef<r5::Utf8Path> + Send + Sync + 'static,
    B: AsRef<[u8]> + Send + Sync + 'static,
{
    pub fn new() -> Result<CppDeps<P, B>, Error> {
        let (cppdeps_tx, cppdeps_rx) = flume::unbounded();
        #[cfg(feature = "cc")]
        let compiler = {
            let build = cc::Build::default();
            let compiler = Compiler::new(build)?;
            Arc::from(compiler)
        };
        let parallelism = std::thread::available_parallelism()
            .or(NonZeroUsize::try_from(1)
                .map_err(|err| InnerError::new(InnerErrorKind::NonZeroUsizeTryFromUsize { err })))?;
        Ok(CppDeps {
            #[cfg(feature = "cc")]
            compiler,
            parallelism,
            cppdeps_tx,
            cppdeps_rx,
            p: PhantomData,
            b: PhantomData,
        })
    }
}

impl<P, B> CppDeps<P, B>
where
    P: AsRef<r5::Utf8Path> + Send + Sync + 'static,
    B: AsRef<[u8]> + Send + Sync + 'static,
{
    fn analyze(self) -> CppDepsAnalyzer<P, B> {
        let parallelism = self.parallelism.get();
        let queue = TaskQueue::new(
            self.cppdeps_rx,
            #[cfg(feature = "cc")]
            self.compiler,
            parallelism,
        );
        CppDepsAnalyzer::new(queue)
    }

    // FIXME: check for `std >= 20`
    #[cfg(feature = "cc")]
    pub fn compiler(&mut self, build: crate::vendor::cc::Build) -> Result<(), Error> {
        let compiler = self::Compiler::new(build)?;
        self.compiler = Arc::from(compiler);
        Ok(())
    }

    pub fn items<Is>(&mut self, items: Is) -> Result<(), Error>
    where
        Is: IntoIterator<Item = CppDepsItem<P, B>>,
    {
        for item in items.into_iter() {
            self.cppdeps_tx
                .send(item)
                .map_err(|_| InnerError::new(InnerErrorKind::BuilderFailedSendingCppDepsItem))?;
        }
        Ok(())
    }

    pub fn parallelism(&mut self, jobs: usize) -> Result<(), Error> {
        let jobs = NonZeroUsize::try_from(jobs)
            .map_err(|err| InnerError::new(InnerErrorKind::NonZeroUsizeTryFromUsize { err }))?;
        self.parallelism = jobs;
        Ok(())
    }

    #[cfg(feature = "sink")]
    pub fn sink(&self) -> CppDepsSink<P, B> {
        let sink = self.cppdeps_tx.clone().into_sink();
        CppDepsSink { sink }
    }
}

impl<P, B> IntoIterator for CppDeps<P, B>
where
    P: AsRef<r5::Utf8Path> + Send + Sync + 'static,
    B: AsRef<[u8]> + Send + Sync + 'static,
{
    type IntoIter = CppDepsAnalyzer<P, B>;
    type Item = Result<DepInfoYoke, Error>;

    fn into_iter(self) -> Self::IntoIter {
        self.analyze()
    }
}

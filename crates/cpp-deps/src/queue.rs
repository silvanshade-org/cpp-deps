#[cfg(feature = "cc")]
use alloc::sync::Arc;
use std::thread::JoinHandle;

use flume::{Receiver, Sender};
use p1689::r5::Utf8Path;

#[cfg(feature = "cc")]
use crate::compiler::Compiler;
use crate::{
    analyzer::{AnalyzerItem, WorkerItem},
    worker::Worker,
    CppDepsItem,
    InnerError,
    InnerErrorKind,
};

pub(crate) struct TaskQueue<P, B> {
    failure_tx: Sender<InnerError>,
    pub(crate) compile_tx: Sender<WorkerItem<P, B>>,
    pub(crate) failure_rx: Receiver<InnerError>,
    pub(crate) analyze_rx: Receiver<AnalyzerItem<P>>,
    threads: Vec<JoinHandle<()>>,
}
impl<P, B> Drop for TaskQueue<P, B> {
    fn drop(&mut self) {
        while let Some(thread) = self.threads.pop() {
            // thread.join().unwrap();
        }
    }
}
impl<P, B> TaskQueue<P, B>
where
    P: AsRef<Utf8Path> + Send + Sync + 'static,
    B: AsRef<[u8]> + Send + Sync + 'static,
{
    pub(crate) fn new(
        cppdeps_rx: Receiver<CppDepsItem<P, B>>,
        #[cfg(feature = "cc")] compiler: Arc<Compiler>,
        parallelism: usize,
    ) -> Self {
        let (failure_tx, failure_rx) = flume::bounded(0);
        let (analyze_tx, analyze_rx) = flume::unbounded();
        let (compile_tx, compile_rx) = flume::unbounded();
        let threads = Vec::with_capacity(parallelism + 1);
        let mut this = Self {
            failure_tx,
            compile_tx,
            failure_rx,
            analyze_rx,
            threads,
        };
        this.spawn_compile_workers(
            &analyze_tx,
            &compile_rx,
            #[cfg(feature = "cc")]
            &compiler,
            parallelism,
        );
        this.spawn_analyze_cppdeps(cppdeps_rx);
        this
    }

    fn spawn_analyze_cppdeps(&mut self, cppdeps_rx: flume::Receiver<CppDepsItem<P, B>>) {
        let compile_tx = self.compile_tx.clone();
        let thunk = move || -> Result<(), InnerError> {
            let mut item_count = 0;
            while let Ok(item) = cppdeps_rx.recv() {
                item_count += 1;
                let item = WorkerItem::Analyze(item);
                compile_tx
                    .send(item)
                    .map_err(|_| InnerError::new(InnerErrorKind::QueueFailedSendingCompileItem))?;
            }
            let item = WorkerItem::Expects(item_count);
            compile_tx
                .send(item)
                .map_err(|_| InnerError::new(InnerErrorKind::QueueFailedSendingCompileItem))?;
            Ok(())
        };
        self.threads.push(std::thread::spawn(move || {
            thunk().ok();
        }))
    }

    fn spawn_compile_workers(
        &mut self,
        analyze_tx: &Sender<AnalyzerItem<P>>,
        compile_rx: &Receiver<WorkerItem<P, B>>,
        #[cfg(feature = "cc")] compiler: &Arc<Compiler>,
        parallelism: usize,
    ) {
        for _ in 0 .. parallelism {
            let failure_tx = self.failure_tx.clone();
            let analyze_tx = analyze_tx.clone();
            let compile_rx = compile_rx.clone();
            #[cfg(feature = "cc")]
            let compiler = compiler.clone();
            let worker = Worker::new(
                failure_tx,
                analyze_tx,
                compile_rx,
                #[cfg(feature = "cc")]
                compiler,
            );
            self.threads.push(std::thread::spawn(worker.run()));
        }
    }
}
impl<P, B> TaskQueue<P, B> {
    pub(crate) fn shutdown(&mut self) {
        self.compile_tx = flume::bounded(0).0;
        self.failure_rx = flume::bounded(0).1; // FIXME: allow collecting all errors
    }
}

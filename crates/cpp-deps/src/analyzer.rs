use alloc::sync::Arc;
use std::collections::{BTreeSet, HashMap, VecDeque};

use p1689::r5::{
    self,
    yoke::{DepInfoNameYoke, DepInfoYoke, DepInfoYokeExt},
};
use qcell::{TCell, TCellOwner};
#[cfg(test)]
use ::{p1689::r5::Utf8Path, std::path::Path};

use crate::{queue::TaskQueue, CppDepsItem, CppDepsSrc, Error, InnerError, InnerErrorKind};

pub(crate) enum AnalyzerItem<P> {
    Expects(usize),
    Analyze(AnalyzeNode<P>),
    #[cfg(feature = "cc")]
    Resolve(ResolveNode),
}

struct NodeOwner;

enum GraphNode<P> {
    Blocking {
        blocked: Vec<Arc<TCell<NodeOwner, AnalyzeNode<P>>>>,
    },
    Resolved {
        phantom: core::marker::PhantomData<P>,
    },
}
impl<P> Default for GraphNode<P> {
    fn default() -> Self {
        let blocked = vec![];
        GraphNode::Blocking { blocked }
    }
}
impl<P> GraphNode<P> {
    const RESOLVED: Self = Self::Resolved {
        phantom: core::marker::PhantomData,
    };
}

pub(crate) struct AnalyzeNode<P> {
    #[allow(unused)]
    pub(crate) src_file: Option<Arc<CppDepsSrc<P>>>,
    pub(crate) dep_info: DepInfoYoke,
    pub(crate) bmi_dirs: BTreeSet<Arc<r5::Utf8PathBuf>>,
    pub(crate) bmi_maps: Vec<(DepInfoNameYoke, Arc<r5::Utf8PathBuf>)>,
}
#[cfg(feature = "cc")]
pub(crate) struct CompileNode<P> {
    pub(crate) src_file: Arc<CppDepsSrc<P>>,
    pub(crate) dep_info: DepInfoYoke,
    pub(crate) bmi_dirs: BTreeSet<Arc<r5::Utf8PathBuf>>,
    pub(crate) bmi_maps: Vec<(DepInfoNameYoke, Arc<r5::Utf8PathBuf>)>,
}
pub(crate) struct ResolveNode {
    pub(crate) bmi_path: Option<r5::Utf8PathBuf>,
    pub(crate) dep_info: DepInfoYoke,
    pub(crate) bmi_dirs: BTreeSet<Arc<r5::Utf8PathBuf>>,
    pub(crate) bmi_maps: Vec<(DepInfoNameYoke, Arc<r5::Utf8PathBuf>)>,
}

pub(crate) enum WorkerItem<P, B> {
    Analyze(CppDepsItem<P, B>),
    #[cfg(feature = "cc")]
    Compile(CompileNode<P>),
    Expects(usize),
}

pub struct CppDepsAnalyzer<P, B> {
    tasks: TaskQueue<P, B>,
    owner: TCellOwner<NodeOwner>,
    graph: HashMap<DepInfoNameYoke, GraphNode<P>>,
    infos: VecDeque<DepInfoYoke>,
    blocked_count: usize,
    analyze_count: usize,
    expects_count: Option<usize>,
    pending_count: usize,
}

impl<P, B> CppDepsAnalyzer<P, B> {
    pub(crate) fn new(tasks: TaskQueue<P, B>) -> Self {
        Self {
            tasks,
            owner: TCellOwner::default(),
            graph: HashMap::default(),
            infos: VecDeque::default(),
            blocked_count: 0,
            analyze_count: 0,
            expects_count: None,
            pending_count: 0,
        }
    }

    fn analyze(&mut self, node: AnalyzeNode<P>) -> Result<(), InnerError> {
        let node = Arc::new(self.owner.cell(node));
        for key in node.ro(&self.owner).dep_info.requires() {
            if let GraphNode::Blocking { ref mut blocked } = self.graph.entry(key).or_default() {
                blocked.push(node.clone());
            }
        }
        if let Some(node) = Arc::into_inner(node).map(TCell::into_inner) {
            self.enqueue(node)?;
        } else {
            self.blocked_count += 1;
        }
        Ok(())
    }

    #[cfg(feature = "cc")]
    fn compile(&mut self, node: CompileNode<P>) -> Result<(), InnerError> {
        self.pending_count += 1;
        self.tasks
            .compile_tx
            .send(WorkerItem::Compile(node))
            .map_err(|_| InnerError::new(InnerErrorKind::AnalyzerFailedSendingCompileItem))
    }

    fn enqueue(&mut self, node: AnalyzeNode<P>) -> Result<(), InnerError> {
        #[cfg(feature = "cc")]
        if let Some(src_file) = node.src_file {
            self.compile(CompileNode {
                src_file,
                dep_info: node.dep_info,
                bmi_dirs: node.bmi_dirs,
                bmi_maps: node.bmi_maps,
            })?;
            return Ok(());
        }
        self.resolve(ResolveNode {
            bmi_path: None,
            dep_info: node.dep_info,
            bmi_dirs: node.bmi_dirs,
            bmi_maps: node.bmi_maps,
        })
    }

    fn error(&self) -> Result<Option<DepInfoYoke>, InnerError> {
        Err(InnerError::new(InnerErrorKind::OrderingSolutionBlocked))
    }

    fn is_finished(&self) -> bool {
        0 == self.pending_count && Some(self.analyze_count) == self.expects_count
    }

    fn recv(&mut self) -> Option<Result<AnalyzerItem<P>, InnerError>> {
        // Release the channels if the analyzer is finished so that we don't deadlock.
        if self.is_finished() {
            self.shutdown();
        }
        let result = flume::Selector::new()
            .recv(&self.tasks.failure_rx, |result| result.ok().map(Err))
            .recv(&self.tasks.analyze_rx, |result| result.ok().map(Ok))
            .wait();
        // Release the channels if the analyzer is failing so that we don't deadlock.
        if matches!(result, Some(Err(..))) {
            self.shutdown()
        }
        result
    }

    fn resolve(&mut self, node: ResolveNode) -> Result<(), InnerError> {
        let mut queue = VecDeque::from([node]);
        while let Some(mut resolved) = queue.pop_front() {
            if let Some(provided) = resolved.dep_info.provides().next() {
                if let Some(bmi_path) = resolved.bmi_path {
                    resolved.bmi_maps.push((provided.clone(), Arc::new(bmi_path)));
                }
                if let Some(GraphNode::Blocking { blocked }) = self.graph.insert(provided, GraphNode::RESOLVED) {
                    for blocked in blocked.into_iter() {
                        {
                            let blocked = blocked.rw(&mut self.owner);
                            blocked.bmi_dirs.extend(resolved.bmi_dirs.iter().cloned());
                            blocked.bmi_maps.extend(resolved.bmi_maps.iter().cloned());
                        }
                        if let Some(blocked) = Arc::into_inner(blocked).map(TCell::into_inner) {
                            self.blocked_count -= 1;
                            #[cfg(feature = "cc")]
                            if let Some(src_file) = blocked.src_file {
                                self.compile(CompileNode {
                                    src_file,
                                    dep_info: blocked.dep_info,
                                    bmi_dirs: blocked.bmi_dirs,
                                    bmi_maps: blocked.bmi_maps,
                                })?;
                                continue;
                            }
                            queue.push_back(ResolveNode {
                                bmi_path: None,
                                dep_info: blocked.dep_info,
                                bmi_dirs: blocked.bmi_dirs,
                                bmi_maps: blocked.bmi_maps,
                            });
                        }
                    }
                }
            }
            self.infos.push_back(resolved.dep_info);
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        self.tasks.shutdown()
    }

    fn step(&mut self) -> Result<Option<DepInfoYoke>, InnerError> {
        while self.infos.is_empty() {
            match self.recv().transpose()? {
                Some(AnalyzerItem::Expects(count)) => {
                    if self.expects_count.replace(count).is_some() {
                        return Err(InnerError::new(InnerErrorKind::AnalyzerAlreadyReceivedExpectsCount));
                    }
                },
                Some(AnalyzerItem::Analyze(node)) => {
                    self.analyze_count += 1;
                    self.analyze(node)?;
                },
                #[cfg(feature = "cc")]
                Some(AnalyzerItem::Resolve(node)) => {
                    self.pending_count -= 1;
                    self.resolve(node)?;
                },
                None => break,
            }
        }
        if let Some(info) = self.infos.pop_front() {
            return Ok(Some(info));
        }
        self.validate_graph()
    }

    fn validate_graph(&self) -> Result<Option<DepInfoYoke>, InnerError> {
        if self.blocked_count > 0 {
            return self.error();
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(crate) fn validate_order<'i>(
        self,
        src_root: &'i Path,
        mut expected_outputs: BTreeSet<&'i Utf8Path>,
    ) -> crate::testing::BoxResult<()> {
        use alloc::collections::BTreeSet;
        let mut valid = BTreeSet::new();
        for result in self.into_iter() {
            let dep_info = result?;
            if let Some(primary_output) = dep_info.get().primary_output.as_deref() {
                // println!("output: {primary_output:?}");
                // NOTE: the `dep_text` tests don't append the tempdir prefix (though maybe they should)
                let primary_output = primary_output.strip_prefix(src_root).unwrap_or(primary_output);
                if !expected_outputs.remove(primary_output) {
                    // return Err("unexpected output or duplicate".into());
                }
            }
            for provide in dep_info.provides() {
                valid.insert(provide);
            }
            for require in dep_info.requires() {
                if !valid.contains(&require) {
                    return Err("missing requirement".into());
                }
            }
        }
        if !expected_outputs.is_empty() {
            // return Err("missing expected output".into());
        }
        Ok(())
    }
}

impl<P, B> Iterator for CppDepsAnalyzer<P, B> {
    type Item = Result<DepInfoYoke, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.step().map_err(Error::from).transpose()
    }
}

#[cfg(test)]
mod test {
    use crate::testing::BoxResult;

    #[test]
    fn analyze() -> BoxResult<()> {
        let paths = crate::testing::corpus::dep_text::items();
        let validate = crate::testing::corpus::dep_text::validate_order(paths)?;
        validate.run()
    }

    #[test]
    fn analyze_reverse() -> BoxResult<()> {
        let paths = crate::testing::corpus::dep_text::items().rev();
        let validate = crate::testing::corpus::dep_text::validate_order(paths)?;
        validate.run()
    }

    #[test]
    #[should_panic]
    fn analyze_incomplete() {
        fn inner() -> BoxResult<()> {
            let paths = crate::testing::corpus::dep_text::items()
                .enumerate()
                .filter_map(|(i, path)| (i != 3).then_some(path));
            let validate = crate::testing::corpus::dep_text::validate_order(paths)?;
            validate.run()
        }
        inner().unwrap()
    }

    #[test]
    #[should_panic]
    fn analyze_cycle() {
        fn inner() -> BoxResult<()> {
            let paths = [
                crate::testing::corpus::dep_text::foo_bar_cycle(),
                crate::testing::corpus::dep_text::bar_foo_cycle(),
            ]
            .into_iter();
            let validate = crate::testing::corpus::dep_text::validate_order(paths)?;
            validate.run()
        }
        inner().unwrap()
    }
}

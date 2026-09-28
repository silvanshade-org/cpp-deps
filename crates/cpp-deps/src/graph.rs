//! Flat dependency planning over borrowed scanner records.

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;

use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use std::path::Path;
use std::path::PathBuf;

use p1689::r5::DepInfo;
use p1689::r5::LookupMethod;
use p1689::r5::ModuleDesc;
use p1689::r5::ModuleName;

use crate::BuildError;
use crate::runner::UnitIndex;

/// One translation unit borrowing its scan buffer and invocation paths.
pub struct Unit<'scan>
{
    /// Source path used by the scanner and compiler.
    pub source: &'scan Path,
    /// Planned primary object output.
    pub object: &'scan Path,
    /// Complete P1689 rule, including artifact and identity metadata.
    pub rule: DepInfo<'scan>,
}

/// Identity used to join provided and required module records.
#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Identity<'arena>
{
    /// Ordinary named module or partition.
    Name(&'arena [u8]),
    /// Source-unique module, resolved against its rule's directory.
    Source(PathBuf),
}

/// Graph topology; indices address the caller's flat unit arena.
pub struct Plan
{
    /// Deterministic independent frontiers in dependency order.
    pub layers: Vec<Vec<UnitIndex>>,
    /// Direct dependencies, deduplicated per importing unit.
    pub dependencies: Vec<Vec<UnitIndex>>,
}

/// Resolve identity without conflating source-unique and named modules.
///
/// # Specification
/// - ensures: source identity uses the reported work directory or invocation
///   directory; named identities borrow decoded names from the unit arena.
/// - fails: a source-unique record omits its source path.
/// - panics: none.
///
/// # Errors
/// Returns `InvalidRules` for incomplete source-unique identity.
///
/// # Adequacy
/// - hypothesis: equal names with distinct source paths must remain distinct;
///   equal source paths resolve together despite different diagnostic names.
/// - witness: graph identity tests.
pub fn identity<'arena>(
    module: &'arena ModuleDesc<'_>,
    unit: &Unit<'_>,
    cwd: &Path,
) -> Result<Identity<'arena>, BuildError>
{
    if module.unique_on_source_path != Some(true) {
        return Ok(Identity::Name(module.logical_name.as_ref()));
    }
    let source = module
        .source_path
        .as_deref()
        .ok_or_else(|| BuildError::InvalidRules {
            source: unit.source.to_path_buf(),
        })?;
    let base = crate::runner::rule_directory(unit, cwd)?;
    let source = crate::runner::native_text(crate::runner::ScannerBytes(source), unit.source)?;
    Ok(Identity::Source(base.join(source)))
}

/// Own a diagnostic name only when crossing the invocation lifetime boundary.
///
/// # Specification
/// trivial.
pub fn owned_name(name: &ModuleName<'_>) -> ModuleName<'static>
{
    ModuleName(alloc::borrow::Cow::Owned(name.as_ref().to_owned()))
}

/// Construct deterministic frontiers without rescanning all nodes per layer.
///
/// # Specification
/// - ensures: every supported import has one matching provider; each node is
///   released once, after all distinct direct providers have been released.
/// - fails: duplicates, missing providers, unsupported lookup, incomplete
///   source identity, dependency cycles, or inconsistent internal indices.
/// - panics: none.
/// - intension: graph storage is flat; frontier traversal is linear in nodes
///   and deduplicated edges, excluding ordered identity-map operations.
///
/// # Errors
/// Returns a named planning error with affected source and module identities.
///
/// # Adequacy
/// - hypothesis: reversed order, diamonds, duplicate imports, partial cycles,
///   source-unique identities and missing imports distinguish planning faults.
/// - witness: graph planning tests.
pub fn plan(
    units: &[Unit<'_>],
    cwd: &Path,
) -> Result<Plan, BuildError>
{
    let mut providers = BTreeMap::new();
    for (index, unit) in units.iter().enumerate() {
        for provided in &unit.rule.provides {
            let key = identity(&provided.desc, unit, cwd)?;
            match providers.entry(key) {
                | Entry::Vacant(entry) => {
                    entry.insert(UnitIndex(index));
                },
                | Entry::Occupied(entry) => {
                    let first = units.get(entry.get().0).ok_or(BuildError::GraphInvariant)?;
                    return Err(BuildError::DuplicateProvider {
                        module: owned_name(&provided.desc.logical_name),
                        first: first.source.to_path_buf(),
                        second: unit.source.to_path_buf(),
                    });
                },
            }
        }
    }
    let mut dependencies = Vec::with_capacity(units.len());
    let mut dependents = vec![Vec::new(); units.len()];
    let mut indegrees = Vec::with_capacity(units.len());
    for (index, unit) in units.iter().enumerate() {
        let mut direct = Vec::with_capacity(unit.rule.requires.len());
        for required in &unit.rule.requires {
            if required.lookup_method != LookupMethod::ByName {
                return Err(BuildError::UnsupportedLookup {
                    source: unit.source.to_path_buf(),
                    module: owned_name(&required.desc.logical_name),
                });
            }
            let key = identity(&required.desc, unit, cwd)?;
            let provider = *providers
                .get(&key)
                .ok_or_else(|| BuildError::MissingImport {
                    source: unit.source.to_path_buf(),
                    module: owned_name(&required.desc.logical_name),
                })?;
            direct.push(provider);
        }
        direct.sort_unstable();
        direct.dedup();
        indegrees.push(direct.len());
        for provider in &direct {
            dependents
                .get_mut(provider.0)
                .ok_or(BuildError::GraphInvariant)?
                .push(UnitIndex(index));
        }
        dependencies.push(direct);
    }
    let mut ready: Vec<_> = indegrees
        .iter()
        .enumerate()
        .filter(|&(_, degree)| *degree == 0)
        .map(|(index, _)| UnitIndex(index))
        .collect();
    let mut remaining = units.len();
    let mut layers = Vec::new();
    while !ready.is_empty() {
        remaining = remaining
            .checked_sub(ready.len())
            .ok_or(BuildError::GraphInvariant)?;
        let mut next = Vec::new();
        for index in &ready {
            for dependent in dependents.get(index.0).ok_or(BuildError::GraphInvariant)? {
                let count = indegrees
                    .get_mut(dependent.0)
                    .ok_or(BuildError::GraphInvariant)?;
                *count = count.checked_sub(1).ok_or(BuildError::GraphInvariant)?;
                if *count == 0 {
                    next.push(*dependent);
                }
            }
        }
        next.sort_unstable();
        layers.push(ready);
        ready = next;
    }
    if remaining != 0 {
        return Err(BuildError::Cycle {
            sources: units
                .iter()
                .zip(&indegrees)
                .filter(|&(_, count)| *count != 0)
                .map(|(unit, _)| unit.source.to_path_buf())
                .collect(),
            modules: units
                .iter()
                .zip(&indegrees)
                .filter(|&(_, count)| *count != 0)
                .flat_map(|(unit, _)| {
                    unit.rule
                        .provides
                        .iter()
                        .map(|module| owned_name(&module.desc.logical_name))
                })
                .collect(),
        });
    }
    Ok(Plan {
        layers,
        dependencies,
    })
}

/// Reusable scratch for one compiler's transitive import list.
#[derive(Default)]
pub struct ImportScratch
{
    /// Per-node visitation generation, avoiding an arena reset for each unit.
    marks: Vec<usize>,
    /// Current traversal generation.
    generation: usize,
    /// Explicit graph traversal stack.
    pending: Vec<UnitIndex>,
    /// Unique transitive providers for the current invocation.
    resolved: Vec<UnitIndex>,
}

impl ImportScratch
{
    /// Resolve one unit's transitive provider closure without recursive calls.
    ///
    /// # Specification
    /// - requires: `plan` is acyclic and belongs to the same unit arena.
    /// - ensures: each reachable provider appears once, in index order.
    /// - fails: inconsistent node indices or generation exhaustion.
    /// - panics: none.
    /// - intension: retains scratch storage, not all nodes' transitive
    ///   closures.
    ///
    /// # Errors
    /// Returns `GraphInvariant` for invalid indices or exhausted generations.
    ///
    /// # Adequacy
    /// - hypothesis: diamonds and repeated calls distinguish duplicate
    ///   traversal from stale membership carried between consumers.
    /// - witness: graph planning tests.
    pub fn resolve(
        &mut self,
        plan: &Plan,
        index: UnitIndex,
    ) -> Result<&[UnitIndex], BuildError>
    {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(BuildError::GraphInvariant)?;
        self.marks.resize(plan.dependencies.len(), 0);
        self.pending.clear();
        self.resolved.clear();
        let direct = plan
            .dependencies
            .get(index.0)
            .ok_or(BuildError::GraphInvariant)?;
        self.pending.extend_from_slice(direct);
        while let Some(provider) = self.pending.pop() {
            let mark = self
                .marks
                .get_mut(provider.0)
                .ok_or(BuildError::GraphInvariant)?;
            if *mark == self.generation {
                continue;
            }
            *mark = self.generation;
            self.resolved.push(provider);
            let dependencies = plan
                .dependencies
                .get(provider.0)
                .ok_or(BuildError::GraphInvariant)?;
            self.pending.extend_from_slice(dependencies);
        }
        self.resolved.sort_unstable();
        Ok(&self.resolved)
    }
}

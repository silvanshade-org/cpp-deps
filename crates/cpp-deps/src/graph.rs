use alloc::collections::BTreeMap;
use alloc::collections::btree_map::Entry;
use std::path::PathBuf;

use p1689::r5::ModuleName;

use crate::BuildError;
use crate::runner::UnitIndex;

/// One scanned translation unit and its planned object output.
pub struct Unit
{
    /// Canonical absolute source path.
    pub source: PathBuf,
    /// Linkable object path under `OUT_DIR`.
    pub object: PathBuf,
    /// Interfaces emitted by the translation unit.
    pub provides: Vec<ModuleName>,
    /// Direct P1689 imports of the translation unit.
    pub requires: Vec<ModuleName>,
}

/// Stable dependency layers and the transitive imports needed by each unit.
pub struct Plan
{
    /// Independent units within each topologically ordered layer.
    pub layers: Vec<Vec<UnitIndex>>,
    /// Source index for each logical interface name.
    pub providers: BTreeMap<ModuleName, usize>,
    /// Direct and transitive provider indices for each translation unit.
    pub imports: Vec<Vec<UnitIndex>>,
}

/// Orders translation units by their P1689 named-module requirements.
///
/// # Specification
/// - requires: all units have been scanned, and each provider names an
///   interface.
/// - ensures: every import is provided by exactly one unit, each provider
///   precedes its importers, and units in a layer have no dependency on each
///   other.
/// - errors: missing imports, duplicate providers, cycles, or an internal index
///   invariant violation are returned with relevant logical names or source
///   paths.
/// - panics: none.
///
/// # Adequacy
/// - hypothesis: L3 examples cover reversed input order, transitive partition
///   imports, independent units, duplicates, missing imports, and a partial
///   cycle.
/// - witness: `graph::tests::ordering_and_named_failures`
pub fn plan(units: &[Unit]) -> Result<Plan, BuildError>
{
    let mut providers = BTreeMap::new();
    for (index, unit) in units.iter().enumerate() {
        for name in &unit.provides {
            match providers.entry(name.clone()) {
                | Entry::Vacant(entry) => {
                    entry.insert(index);
                },
                | Entry::Occupied(entry) => {
                    let first = units.get(*entry.get()).ok_or(BuildError::GraphInvariant)?;
                    return Err(BuildError::DuplicateProvider {
                        module: name.clone(),
                        first: first.source.clone(),
                        second: unit.source.clone(),
                    });
                },
            }
        }
    }

    let mut indegree = vec![0_usize; units.len()];
    let mut dependents = vec![Vec::new(); units.len()];
    for (index, unit) in units.iter().enumerate() {
        for name in &unit.requires {
            let provider = *providers
                .get(name)
                .ok_or_else(|| BuildError::MissingImport {
                    module: name.clone(),
                    source: unit.source.clone(),
                })?;
            let followers = dependents
                .get_mut(provider)
                .ok_or(BuildError::GraphInvariant)?;
            if !followers.contains(&index) {
                let count = indegree.get_mut(index).ok_or(BuildError::GraphInvariant)?;
                *count = count.checked_add(1).ok_or(BuildError::GraphInvariant)?;
                followers.push(index);
            }
        }
    }

    let mut remaining = units.len();
    let mut layers = Vec::new();
    while remaining > 0 {
        let ready: Vec<_> = indegree
            .iter()
            .enumerate()
            .filter(|&(_, count)| *count == 0)
            .map(|(index, _)| UnitIndex(index))
            .collect();
        if ready.is_empty() {
            return Err(BuildError::Cycle {
                sources: units
                    .iter()
                    .zip(&indegree)
                    .filter(|&(_, count)| *count > 0 && *count < usize::MAX)
                    .map(|(unit, _)| unit.source.clone())
                    .collect(),
                modules: units
                    .iter()
                    .zip(&indegree)
                    .filter(|&(_, count)| *count > 0 && *count < usize::MAX)
                    .flat_map(|(unit, _)| unit.provides.iter().cloned())
                    .collect(),
            });
        }
        for &index in &ready {
            let count = indegree
                .get_mut(index.0)
                .ok_or(BuildError::GraphInvariant)?;
            *count = usize::MAX;
            remaining = remaining.checked_sub(1).ok_or(BuildError::GraphInvariant)?;
        }
        for &index in &ready {
            let followers = dependents.get(index.0).ok_or(BuildError::GraphInvariant)?;
            for &dependent in followers {
                let count = indegree
                    .get_mut(dependent)
                    .ok_or(BuildError::GraphInvariant)?;
                *count = count.checked_sub(1).ok_or(BuildError::GraphInvariant)?;
            }
        }
        layers.push(ready);
    }

    let mut imports: Vec<Vec<UnitIndex>> = vec![Vec::new(); units.len()];
    for layer in &layers {
        for &index in layer {
            let unit = units.get(index.0).ok_or(BuildError::GraphInvariant)?;
            let mut resolved = Vec::new();
            for name in &unit.requires {
                let provider = *providers.get(name).ok_or(BuildError::GraphInvariant)?;
                if !resolved.iter().any(|item: &UnitIndex| item.0 == provider) {
                    resolved.push(UnitIndex(provider));
                }
                let transitive = imports.get(provider).ok_or(BuildError::GraphInvariant)?;
                for &dependency in transitive {
                    if !resolved.iter().any(|item| item.0 == dependency.0) {
                        resolved.push(dependency);
                    }
                }
            }
            let slot = imports.get_mut(index.0).ok_or(BuildError::GraphInvariant)?;
            *slot = resolved;
        }
    }
    Ok(Plan {
        layers,
        providers,
        imports,
    })
}

#[cfg(test)]
mod tests
{
    use std::path::Path;
    use std::path::PathBuf;

    use p1689::r5::ModuleName;

    use super::Unit;
    use super::plan;
    use crate::BuildError;
    use crate::runner::UnitIndex;

    macro_rules! unit {
        ($source:literal, [$($provided:literal),*], [$($required:literal),*]) => {
            Unit {
                source: PathBuf::from($source),
                object: PathBuf::from(concat!($source, ".o")),
                provides: vec![$(ModuleName(String::from($provided))),*],
                requires: vec![$(ModuleName(String::from($required))),*],
            }
        };
    }

    #[test]
    fn ordering_and_named_failures()
    {
        let reversed = [
            unit!("consumer.cpp", [], ["api"]),
            unit!("api.cppm", ["api"], ["api:part"]),
            unit!("part.cppm", ["api:part"], []),
            unit!("other.cpp", [], []),
        ];
        let ordered = plan(&reversed).expect("valid graph");
        assert_eq!(ordered.layers, vec![
            vec![UnitIndex(2), UnitIndex(3)],
            vec![UnitIndex(1)],
            vec![UnitIndex(0)]
        ]);
        assert_eq!(
            ordered
                .imports
                .first()
                .expect("consumer")
                .iter()
                .map(|index| index.0)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        let missing = [unit!("consumer.cpp", [], ["missing"])];
        assert!(
            matches!(plan(&missing), Err(BuildError::MissingImport { module, source })
            if module.as_ref() == "missing" && source == Path::new("consumer.cpp"))
        );

        let duplicate = [
            unit!("one.cppm", ["api"], []),
            unit!("two.cppm", ["api"], []),
        ];
        assert!(
            matches!(plan(&duplicate), Err(BuildError::DuplicateProvider { module, first, second })
            if module.as_ref() == "api" && first == Path::new("one.cppm") && second == Path::new("two.cppm"))
        );

        let partial_cycle = [
            unit!("independent.cpp", [], []),
            unit!("one.cppm", ["one"], ["two"]),
            unit!("two.cppm", ["two"], ["one"]),
        ];
        assert!(
            matches!(plan(&partial_cycle), Err(BuildError::Cycle { sources, modules })
            if sources == [Path::new("one.cppm"), Path::new("two.cppm")]
                && modules.iter().map(ModuleName::as_ref).collect::<Vec<_>>() == ["one", "two"])
        );
    }
}

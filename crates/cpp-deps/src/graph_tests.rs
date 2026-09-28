//! Dependency identity, frontier and closure witnesses.
use alloc::borrow::Cow;
use std::path::Path;

use p1689::r5::DepInfo;
use p1689::r5::IsInterface;
use p1689::r5::LookupMethod;
use p1689::r5::ModuleDesc;
use p1689::r5::ModuleName;
use p1689::r5::ProvidedModuleDesc;
use p1689::r5::RequiredModuleDesc;

use super::ImportScratch;
use super::Unit;
use super::plan;
use crate::BuildError;
use crate::runner::UnitIndex;

/// Construct source-bound units without erasing dependency metadata.
macro_rules! unit {
    ($source:literal, [$($provided:literal),*], [$($required:literal),*]) => {
        Unit {
            source: Path::new($source), object: Path::new(concat!($source, ".o")),
            rule: DepInfo {
                provides: vec![$(ProvidedModuleDesc { desc: ModuleDesc {
                    logical_name: ModuleName(Cow::Borrowed($provided)), source_path: None,
                    compiled_module_path: None, unique_on_source_path: None,
                }, is_interface: IsInterface(true) }),*],
                requires: vec![$(RequiredModuleDesc { desc: ModuleDesc {
                    logical_name: ModuleName(Cow::Borrowed($required)), source_path: None,
                    compiled_module_path: None, unique_on_source_path: None,
                }, lookup_method: LookupMethod::ByName }),*],
                ..DepInfo::default()
            },
        }
    };
}

#[test]
fn frontiers_deduplicate_edges_and_closure_scratch()
{
    let units = [
        unit!("consumer.cpp", [], [b"left", b"right", b"left"]),
        unit!("left.cppm", [b"left"], [b"base"]),
        unit!("right.cppm", [b"right"], [b"base"]),
        unit!("base.cppm", [b"base"], []),
        unit!("independent.cpp", [], []),
    ];
    let plan = plan(&units, Path::new("/build")).expect("diamond DAG");
    assert_eq!(plan.layers, [
        vec![UnitIndex(3), UnitIndex(4)],
        vec![UnitIndex(1), UnitIndex(2)],
        vec![UnitIndex(0)]
    ]);
    assert_eq!(
        plan.dependencies.first(),
        Some(&vec![UnitIndex(1), UnitIndex(2)])
    );
    let mut scratch = ImportScratch::default();
    assert_eq!(
        scratch
            .resolve(&plan, UnitIndex(0))
            .expect("diamond imports"),
        [UnitIndex(1), UnitIndex(2), UnitIndex(3)]
    );
    assert_eq!(
        scratch.resolve(&plan, UnitIndex(1)).expect("left imports"),
        [UnitIndex(3)]
    );
    assert_eq!(
        scratch
            .resolve(&plan, UnitIndex(4))
            .expect("independent imports"),
        []
    );
}

#[test]
fn names_do_not_replace_source_identity()
{
    let mut units = [
        unit!("a.cppm", [b"same"], []),
        unit!("b.cppm", [b"same"], []),
        unit!("consumer.cpp", [], [b"diagnostic-name"]),
    ];
    for (unit, path) in units.iter_mut().take(2).zip(["a.cppm", "b.cppm"]) {
        let provided = unit.rule.provides.first_mut().expect("provider");
        provided.desc.source_path = Some(Cow::Borrowed(path.as_bytes()));
        provided.desc.unique_on_source_path = Some(true);
        unit.rule.work_directory = Some(Cow::Borrowed(b"/source"));
    }
    let consumer = units.get_mut(2).expect("consumer");
    let required = consumer.rule.requires.first_mut().expect("import");
    required.desc.source_path = Some(Cow::Borrowed(b"/source/b.cppm"));
    required.desc.unique_on_source_path = Some(true);
    let plan = plan(&units, Path::new("/build")).expect("source-unique providers");
    assert_eq!(plan.dependencies.get(2), Some(&vec![UnitIndex(1)]));
}

#[test]
fn named_failures_include_affected_sources()
{
    let missing = [unit!("consumer.cpp", [], [b"missing"])];
    assert!(
        matches!(plan(&missing, Path::new("/build")), Err(BuildError::MissingImport { module, source })
        if module.as_ref() == b"missing" && source == Path::new("consumer.cpp"))
    );
    let duplicate = [
        unit!("first.cppm", [b"api"], []),
        unit!("second.cppm", [b"api"], []),
    ];
    assert!(
        matches!(plan(&duplicate, Path::new("/build")), Err(BuildError::DuplicateProvider { module, first, second })
        if module.as_ref() == b"api" && first == Path::new("first.cppm") && second == Path::new("second.cppm"))
    );
    let cycle = [
        unit!("independent.cpp", [], []),
        unit!("a.cppm", [b"a"], [b"b"]),
        unit!("b.cppm", [b"b"], [b"a"]),
    ];
    assert!(
        matches!(plan(&cycle, Path::new("/build")), Err(BuildError::Cycle { sources, modules })
        if sources == [Path::new("a.cppm"), Path::new("b.cppm")]
            && modules.iter().map(ModuleName::as_ref).collect::<Vec<_>>() == [b"a", b"b"])
    );
}

#[test]
fn incomplete_identity_and_header_lookup_fail_explicitly()
{
    let mut units = [unit!("a.cppm", [b"api"], [])];
    units
        .first_mut()
        .expect("unit")
        .rule
        .provides
        .first_mut()
        .expect("provider")
        .desc
        .unique_on_source_path = Some(true);
    assert!(
        matches!(plan(&units, Path::new("/build")), Err(BuildError::InvalidRules { source }) if source == Path::new("a.cppm"))
    );
    let mut units = [unit!("consumer.cpp", [], [b"header.hpp"])];
    units
        .first_mut()
        .expect("unit")
        .rule
        .requires
        .first_mut()
        .expect("requirement")
        .lookup_method = LookupMethod::IncludeQuote;
    assert!(
        matches!(plan(&units, Path::new("/build")), Err(BuildError::UnsupportedLookup { module, source })
        if module.as_ref() == b"header.hpp" && source == Path::new("consumer.cpp"))
    );
}

#[test]
fn required_artifact_path_overrides_fallback_and_conflicts_fail()
{
    let mut units = [
        unit!("provider.cppm", [b"api"], []),
        unit!("consumer.cpp", [], [b"api"]),
    ];
    let consumer = units.get_mut(1).expect("consumer");
    consumer.rule.work_directory = Some(Cow::Borrowed(b"/build"));
    consumer
        .rule
        .requires
        .first_mut()
        .expect("requirement")
        .desc
        .compiled_module_path = Some(Cow::Borrowed(b"out/compiler-name.pcm"));
    let paths = crate::runner::interface_paths(
        &units,
        crate::runner::CompilerKind::Clang,
        Path::new("/build/out"),
        Path::new("/elsewhere"),
    )
    .expect("reported binding");
    assert_eq!(
        crate::runner::artifact_path(&paths, UnitIndex(0)).expect("provider path"),
        Path::new("/build/out/compiler-name.pcm")
    );
    let provider = units.first_mut().expect("provider");
    provider
        .rule
        .provides
        .first_mut()
        .expect("provided module")
        .desc
        .compiled_module_path = Some(Cow::Borrowed(b"/build/out/conflicting.pcm"));
    assert!(matches!(
        crate::runner::interface_paths(
            &units,
            crate::runner::CompilerKind::Clang,
            Path::new("/build/out"),
            Path::new("/elsewhere")
        ),
        Err(BuildError::Io {
            operation: "bind reported module artifact",
            ..
        })
    ));
}

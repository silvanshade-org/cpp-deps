//! Restoration witnesses for incomplete, corrupt and mismatched inventories.

use super::*;

#[test]
fn incomplete_record_never_replaces_live_outputs()
{
    let root = Stage::new(&std::env::temp_dir()).expect("temporary root");
    let out = root.0.join("out");
    fs::create_dir_all(&out).expect("output directory");
    let cache = ModuleCache::new(
        root.0.join("cache"),
        ToolchainIdentity(blake3::hash(b"fixture")),
    );
    let destinations = [
        Destination {
            role: Role::Object,
            path: out.join("unit.o"),
        },
        Destination {
            role: Role::Interface,
            path: out.join("unit.pcm"),
        },
    ];
    fs::write(&destinations[0].path, b"object revision one").expect("object");
    fs::write(&destinations[1].path, b"interface revision one").expect("interface");
    let original_time = fs::metadata(&destinations[1].path)
        .expect("metadata")
        .modified()
        .expect("mtime");
    let action = Key::default().finish();
    cache.publish(action, &destinations).expect("publish");
    let (digest, ..) = identify(&destinations[1].path).expect("identify interface");
    let blob = cache.root.join("blobs").join(digest.0.to_hex().as_str());
    let original_blob = fs::read(&blob).expect("saved blob");
    fs::write(&destinations[0].path, b"live object").expect("live object");
    fs::write(&destinations[1].path, b"live interface").expect("live interface");
    fs::remove_file(&blob).expect("remove interface blob");
    assert!(matches!(
        cache
            .restore(action, &destinations, &out)
            .expect("missing blob lookup"),
        Lookup::Corrupt
    ));
    assert_eq!(
        fs::read(&destinations[0].path).expect("unchanged object"),
        b"live object"
    );
    assert_eq!(
        fs::read(&destinations[1].path).expect("unchanged interface"),
        b"live interface"
    );
    fs::write(&blob, b"interface revision two").expect("corrupt same-size blob");
    assert!(matches!(
        cache
            .restore(action, &destinations, &out)
            .expect("corrupt blob lookup"),
        Lookup::Corrupt
    ));
    assert_eq!(
        fs::read(&destinations[0].path).expect("unchanged object"),
        b"live object"
    );
    fs::write(&blob, original_blob).expect("repair blob");
    assert!(matches!(
        cache
            .restore(action, &destinations, &out)
            .expect("complete lookup"),
        Lookup::Restored
    ));
    assert_eq!(
        fs::read(&destinations[0].path).expect("restored object"),
        b"object revision one"
    );
    assert_eq!(
        fs::read(&destinations[1].path).expect("restored interface"),
        b"interface revision one"
    );
    assert_eq!(
        fs::metadata(&destinations[1].path)
            .expect("metadata")
            .modified()
            .expect("mtime"),
        original_time
    );
}

#[test]
fn live_inventory_rejects_role_path_and_count_substitution()
{
    let root = Stage::new(&std::env::temp_dir()).expect("temporary root");
    let out = root.0.join("out");
    fs::create_dir_all(&out).expect("output directory");
    let cache = ModuleCache::new(
        root.0.join("cache"),
        ToolchainIdentity(blake3::hash(b"fixture")),
    );
    let mut destinations = [Destination {
        role: Role::Object,
        path: out.join("unit.o"),
    }];
    fs::write(&destinations[0].path, b"cached object").expect("object");
    let action = Key::default().finish();
    cache.publish(action, &destinations).expect("publish");
    fs::write(&destinations[0].path, b"live object").expect("live object");
    destinations[0].role = Role::Interface;
    assert!(matches!(
        cache
            .restore(action, &destinations, &out)
            .expect("role substitution"),
        Lookup::Corrupt
    ));
    destinations[0].role = Role::Object;
    destinations[0].path = out.join("else.o");
    assert!(matches!(
        cache
            .restore(action, &destinations, &out)
            .expect("path substitution"),
        Lookup::Corrupt
    ));
    assert!(!destinations[0].path.exists());
    assert!(matches!(
        cache
            .restore(action, &[], &out)
            .expect("count substitution"),
        Lookup::Corrupt
    ));
    assert_eq!(
        fs::read(out.join("unit.o")).expect("unchanged object"),
        b"live object"
    );
}

#[test]
fn depfile_aliases_resolve_to_one_existing_input()
{
    let root = Stage::new(&std::env::temp_dir()).expect("temporary root");
    fs::create_dir_all(root.0.join("nested")).expect("alias parent");
    let source = root.0.join("header.hpp");
    fs::write(&source, b"#define VALUE 42\n").expect("header");
    let depfile = root.0.join("unit.d");
    fs::write(
        &depfile,
        b"unit.o: header.hpp nested/../header.hpp ./header.hpp\n",
    )
    .expect("depfile");
    let paths = crate::depfile::read(&depfile, &root.0).expect("resolve aliases");
    assert_eq!(paths, vec![
        fs::canonicalize(&source).expect("canonical header")
    ]);
}

#[test]
fn interrupted_publication_and_damaged_manifests_preserve_live_outputs()
{
    let root = Stage::new(&std::env::temp_dir()).expect("temporary root");
    let out = root.0.join("out");
    fs::create_dir_all(&out).expect("output directory");
    let cache = ModuleCache::new(
        root.0.join("cache"),
        ToolchainIdentity(blake3::hash(b"fixture")),
    );
    let destinations = [
        Destination {
            role: Role::Object,
            path: out.join("unit.o"),
        },
        Destination {
            role: Role::Interface,
            path: out.join("unit.pcm"),
        },
    ];
    fs::write(&destinations[0].path, b"original object").expect("object");
    fs::write(&destinations[1].path, b"original interface").expect("interface");
    let action = Key::default().finish();
    cache
        .publish(action, &destinations)
        .expect("complete publication");
    let manifest = cache.root.join("results").join(action.0.to_hex().as_str());
    let complete = fs::read(&manifest).expect("complete manifest");
    fs::write(&destinations[0].path, b"replacement object").expect("replacement");
    fs::remove_file(&destinations[1].path).expect("missing companion");
    assert_eq!(
        cache
            .publish(action, &destinations)
            .expect_err("publication interrupted")
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(matches!(
        cache
            .restore(action, &destinations, &out)
            .expect("prior result"),
        Lookup::Restored
    ));
    assert_eq!(
        fs::read(&destinations[0].path).expect("old object"),
        b"original object"
    );
    assert_eq!(
        fs::read(&destinations[1].path).expect("old interface"),
        b"original interface"
    );
    fs::write(&destinations[0].path, b"live object").expect("live object");
    fs::write(&destinations[1].path, b"live interface").expect("live interface");
    for end in 0 .. complete.len() {
        fs::write(&manifest, &complete[.. end]).expect("truncated manifest");
        assert!(matches!(
            cache
                .restore(action, &destinations, &out)
                .expect("truncation lookup"),
            Lookup::Corrupt
        ));
        assert_eq!(
            fs::read(&destinations[0].path).expect("unchanged object"),
            b"live object"
        );
        assert_eq!(
            fs::read(&destinations[1].path).expect("unchanged interface"),
            b"live interface"
        );
    }
    let mut damaged = complete;
    *damaged.last_mut().expect("manifest checksum") ^= 1;
    fs::write(&manifest, damaged).expect("corrupt checksum");
    assert!(matches!(
        cache
            .restore(action, &destinations, &out)
            .expect("checksum lookup"),
        Lookup::Corrupt
    ));
    assert_eq!(
        fs::read(&destinations[0].path).expect("unchanged object"),
        b"live object"
    );
    assert_eq!(
        fs::read(&destinations[1].path).expect("unchanged interface"),
        b"live interface"
    );
    fs::remove_file(&manifest).expect("missing manifest");
    assert!(matches!(
        cache
            .restore(action, &destinations, &out)
            .expect("missing lookup"),
        Lookup::Absent
    ));
    assert_eq!(
        fs::read(&destinations[0].path).expect("unchanged object"),
        b"live object"
    );
    assert_eq!(
        fs::read(&destinations[1].path).expect("unchanged interface"),
        b"live interface"
    );
}

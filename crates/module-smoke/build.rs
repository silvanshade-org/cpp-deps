use std::env;
use std::fs;
use std::path::PathBuf;

/// Exercise native module linking and Clang time-trace cache restoration.
///
/// # Specification
/// - requires: the selected toolchain is immutable during this build script.
/// - ensures: removed Clang time traces return byte-for-byte and object mtimes
///   survive warm restoration before linking; other compiler lanes retain the
///   native module smoke.
/// - fails: compiler, cache, filesystem or linker failures.
/// - panics: a native artifact or cache restoration assertion fails.
///
/// # Adequacy
/// - hypothesis: an omitted persistent time trace is observable after a hit.
/// - witness: cold compilation, artifact deletion, warm restoration, then the
///   linked module-smoke test under the Clang CI lane.
fn main() -> Result<(), Box<dyn core::error::Error>>
{
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    let mut native = cc::Build::new();
    native.cpp(true).std("c++20").include("include");
    let tool = native.get_compiler();
    let time_trace = tool.is_like_clang() && !tool.is_like_msvc();
    if time_trace {
        native.flag("-ftime-trace");
    }
    let repeated_native = time_trace.then(|| native.clone());
    let cache = out_dir.join("trace-smoke-cache");
    if time_trace && cache.exists() {
        // Never reuse this fixture's attestation across build-script runs.
        fs::remove_dir_all(&cache)?;
    }
    let parallelism = core::num::NonZeroUsize::new(2).ok_or("invalid fixture parallelism")?;
    let configure = |native| {
        let mut modules = cpp_deps::ModuleBuild::new(native);
        if time_trace {
            modules.cache(cpp_deps::cache::ModuleCache::new(
                cache.clone(),
                cpp_deps::cache::ToolchainIdentity(blake3::hash(b"module-smoke invocation")),
            ));
        }
        // Reverse provider/importer order to exercise graph scheduling.
        modules
            .source("src/consumer.cpp")
            .source("src/implementation.cpp")
            .source("src/interface.cppm")
            .source("src/part.cppm")
            .source("src/detail.cppm")
            .parallelism(parallelism);
        modules
    };
    // Textual dependencies are tracked by cpp-deps. Watch the search
    // directory too, so a newly introduced header triggers rediscovery.
    println!("cargo:rerun-if-changed=include");
    let output = configure(native).compile()?;
    if let Some(native) = repeated_native {
        let artifacts = output
            .objects
            .iter()
            .map(|object| {
                let trace = object.with_extension("json");
                let bytes = fs::read(&trace)?;
                let modified = fs::metadata(object)?.modified()?;
                Ok::<_, std::io::Error>((object, trace, bytes, modified))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for &(object, ref trace, ..) in &artifacts {
            fs::remove_file(object)?;
            fs::remove_file(trace)?;
        }
        drop(configure(native).compile()?);
        for (object, trace, bytes, modified) in artifacts {
            assert_eq!(
                fs::read(trace)?,
                bytes,
                "time trace was not restored unchanged"
            );
            assert_eq!(
                fs::metadata(object)?.modified()?,
                modified,
                "object was recompiled instead of restored"
            );
        }
    }
    assert_eq!(
        output.interfaces.len(),
        3,
        "interface, interface-partition, and internal-partition BMIs"
    );
    for artifact in &output.interfaces {
        let module = &artifact.description.logical_name;
        let path = &artifact.path;
        assert!(
            matches!(
                module.as_ref(),
                b"sample" | b"sample:part" | b"sample:detail"
            ),
            "unexpected module name: {}",
            module.as_ref().escape_ascii()
        );
        assert!(
            path.is_file(),
            "{} BMI was not produced",
            module.as_ref().escape_ascii()
        );
    }
    for object in &output.objects {
        assert!(
            object.is_file(),
            "{} object was not produced",
            object.display()
        );
        assert!(object.starts_with(&out_dir), "object escaped OUT_DIR");
    }

    let mut linker = cc::Build::new();
    linker
        .cpp(true)
        .objects(&output.objects)
        .compile("module_smoke");
    Ok(())
}

use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn core::error::Error>>
{
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo did not set OUT_DIR")?);
    let mut native = cc::Build::new();
    native.cpp(true).std("c++20").include("include");
    let mut modules = cpp_deps::ModuleBuild::new(native);
    // Reverse the provider/importer order to exercise graph scheduling.
    let parallelism = core::num::NonZeroUsize::new(2).ok_or("invalid fixture parallelism")?;
    modules
        .source("src/consumer.cpp")
        .source("src/implementation.cpp")
        .source("src/interface.cppm")
        .source("src/part.cppm")
        .source("src/detail.cppm")
        .parallelism(parallelism);
    // Textual dependencies are tracked by cpp-deps. Watch the search
    // directory too, so a newly introduced header triggers rediscovery.
    println!("cargo:rerun-if-changed=include");
    let output = modules.compile()?;
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

# cpp-deps

`cpp-deps` builds C++20 named modules from a Cargo build script. This repository lives at [silvanshade-org/cpp-deps](https://github.com/silvanshade-org/cpp-deps). The `p1689` crate models revision 5 dependency files, while `cpp-deps` scans, orders, and compiles translation units using the caller's `cc::Build` settings.

## Build-script usage

Add `cc` and `cpp-deps` as build dependencies. Pass every translation unit, including implementations and non-module importers, in any order:

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut native = cc::Build::new();
    native.cpp(true).std("c++20").include("include");

    let mut modules = cpp_deps::ModuleBuild::new(native);
    modules
        .source("src/consumer.cpp")
        .source("src/interface.cppm")
        .source("src/part.cppm");
    let output = modules.compile()?;

    let mut archive = cc::Build::new();
    archive.cpp(true).objects(&output.objects).compile("my_modules");
    // output.interfaces maps logical names (including partitions) to BMI paths.
    Ok(())
}
```

`compile` uses Cargo's `OUT_DIR` unless `.out_dir(path)` overrides it. `BuildOutput.objects` preserves source order; `BuildOutput.interfaces` maps provided logical names to native PCM, GCM, or IFC paths under the output directory. A missing import, duplicate provider, cycle, unsupported lookup, or failed scanner/compiler returns a named error rather than continuing with an invalid order. `.parallelism(nonzero)` bounds simultaneous compiles of independent units; the default is the host's available parallelism.

Clang 22 scans through `clang-scan-deps -format=p1689` and builds interfaces in one pass using `-fmodule-output`. The scanner is the one the compiler reports with `-print-prog-name=clang-scan-deps`, then the one beside the compiler, then `clang-scan-deps` on `PATH`; `.scanner(path)` selects a different binary. Every scan also passes the compiler's own `-print-resource-dir`, so a shim or wrapper script as `CXX` still finds Clang's builtin headers. GCC 16 scans with `-fdeps-format=p1689r5` and uses a mapper file in `OUT_DIR`. Linux CI runs both families. The MSVC path is retained but not a Linux CI gate. Only named modules are supported; header-unit imports return `UnsupportedLookup`. Build scripts should emit their own `cargo:rerun-if-changed` markers for headers that affect the modules.

The `module-smoke` workspace crate builds a partition, interface, implementation, and importer, archives their objects, and asserts the linked C ABI values from a Rust test. To exercise both supported compiler lanes locally, run `CXX=clang++ mise exec -- cargo test --workspace` and `CXX=g++ mise exec -- cargo test --workspace` with Clang 22 (and its scanner) and GCC 16 installed.

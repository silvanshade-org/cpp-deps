# cpp-deps

`cpp-deps` builds C++20 named modules from a Cargo build script. This repository lives at [silvanshade-org/cpp-deps](https://github.com/silvanshade-org/cpp-deps). The `p1689` crate models revision 5 dependency files, while `cpp-deps` scans, orders, and compiles translation units using the caller's `cc::Build` settings.

The [module build and cache architecture](docs/architecture.md) specifies borrowed parsing, module identity, validated action keys, artifact stability and complete result restoration.

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
        .source("src/part.cppm")
        .source("src/detail.cppm");
    // Textual inputs are tracked; watch search directories for new filenames.
    println!("cargo:rerun-if-changed=include");
    let output = modules.compile()?;

    let mut archive = cc::Build::new();
    archive.cpp(true).objects(&output.objects).compile("my_modules");
    // Each output.interfaces record retains its P1689 module description
    // and the actual BMI destination in artifact.path.
    Ok(())
}
```

`compile` uses Cargo's `OUT_DIR` unless `.out_dir(path)` overrides it. `BuildOutput.objects` preserves source order. Each `BuildOutput.interfaces` record retains the complete provided module description and its native PCM, GCM, or IFC destination. Compiler-reported BMI paths take precedence; absent paths use a source-path-stable mapping under the output directory. A missing import, duplicate provider, cycle, unsupported lookup, or failed scanner/compiler returns a named error.

Pass an already acquired Cargo/GNU jobserver client through `.jobserver(client)`. The caller retains its implicit job slot; additional workers hold acquired tokens until their batch completes. Without a client, compilation is serial. `.parallelism(nonzero)` adds a caller-selected ceiling; its default imposes no ceiling beyond available jobserver capacity. Importing inherited jobserver descriptors remains the caller's responsibility; this library does not perform unsafe environment-based attachment. The jobserver helper's platform signal behavior follows the jobserver crate.

Clang 22 scans through `clang-scan-deps -format=p1689` and builds interfaces in one pass using `-fmodule-output`. The scanner is the one the compiler reports with `-print-prog-name=clang-scan-deps`, then the one beside the compiler, then `clang-scan-deps` on `PATH`; `.scanner(path)` selects a different binary. Every scan also passes the compiler's own `-print-resource-dir`, so a shim or wrapper script as `CXX` still finds Clang's builtin headers. GCC 16 scans with `-fdeps-format=p1689r5` and uses a mapper file in `OUT_DIR`. Linux CI runs both families. The MSVC path is retained but not a Linux CI gate. Only named modules are supported; header-unit imports return `UnsupportedLookup`. GCC and Clang depfiles supply Cargo rerun markers for textual inputs, including system headers, after both compilation and cache restoration. Continue watching include search directories for newly introduced filenames; MSVC callers still own header tracking.

## Local module cache

Opt in with `.cache(cpp_deps::cache::ModuleCache::new(root, identity))`. `identity` is a `ToolchainIdentity` containing a BLAKE3 digest that attests the complete immutable compiler installation, including subordinate tools, loaded libraries, plugins, resource files, specs and configuration. A compiler version or executable hash alone is insufficient. The caller must change the attestation before changing any component.

Every lookup performs fresh preprocessing and textual dependency discovery, hashes current resolved inputs and imported BMI bytes, and accounts for the effective ordered invocation, environment, working directory and compiler-expanded response arguments. Records distinguish action identities from artifact byte identities. Object, BMI, depfile, structured dependency and compiler-reported outputs form one admitted inventory. Driver-declared saved intermediates, split DWARF files, coverage notes and serialized diagnostics join that inventory; runtime coverage data does not. Destinations outside the output root or shared between different sources are rejected. All required blobs are staged and validated before restoration; restored modification times preserve compiler timestamp checks. A session lock serializes users of the same output root. Native misses remain wrapped by ccache, and missing companion artifacts fail instead of accepting an object-only hit.

The filesystem contract is cooperative input mutation with content revalidation, not adversarial snapshot isolation. Inputs that change across compilation prevent publication. Cache-enabled MSVC builds currently fail explicitly because this adapter lacks a complete textual discovery witness; uncached MSVC compilation remains available. The [architecture](docs/architecture.md) defines the complete identity and compatibility obligations.

## Native smoke

The `module-smoke` workspace crate builds an interface partition, an internal partition, an interface, an implementation, and an importer, archives their objects, and asserts the linked C ABI values from a Rust test. To exercise both supported compiler lanes locally, run `CXX=clang++ mise exec -- cargo test --workspace` and `CXX=g++ mise exec -- cargo test --workspace` with Clang 22 (and its scanner) and GCC 16 installed.

## CI status contexts

Workflow display names match the protected branch's required status checks. Job IDs remain stable for dependency edges and cache-writer selection; changing a display name independently of branch protection can leave a passing run unable to enter the merge queue.

## CI

Pull requests run light quality, format, workflow lint, and fail-open path-filter contracts. Main pushes and merge queues add both Clang 22 and GCC 16 module tests with nextest JUnit reports. The native compiler lanes invoke ccache and retain per-unit depfiles; clangd, clang-tidy, and clang-format share the reference runtime profile. Run `mise run check:ci-pins` to verify workflow/image tool pin agreement and `mise run check:ci-scripts` to exercise the filter boundaries and `mise run lint:cpp` for every module translation unit and `mise exec -- act push -j workflow-lint` for the local workflow smoke. The prebuilt image stays optional until its published GHCR tag has passed a preview run.

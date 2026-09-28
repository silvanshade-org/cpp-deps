# AGENTS.md

This repository is public. Read this file, then `docs/agents/baseline.md`, then every matching row before acting. Parent and deeper guidance apply together; nearest guidance wins on overlap.

| About to | Read | Never |
| -------- | ---- | ----- |
| write or emit anything | `docs/agents/baseline.md` | use a register or reference the baseline rules out |
| write, change, or review Rust | `docs/agents/rust.md` | weaken the lint wall to make a change pass |
| write a test or claim evidence for a specification | `docs/agents/testing-contracts.md` | count a passing test as proof of an unobserved property |
| build, format, gate, or commit | `docs/agents/source-workflow.md` | invoke a pinned binary outside mise |
| change hosted CI | `docs/agents/ci-local.md` | replace the shared CI shape with bare-runner-only jobs |

## Vendored-page bindings

`docs/agents/rust.md`, `docs/agents/testing-contracts.md`, `docs/agents/source-workflow.md`, and `docs/agents/ci-local.md` are byte-identical copies of shared guidance. `docs/agents/baseline.md` and `docs/agents/baseline.sha256` are a coupled copy checked by `mise run check:baseline-hash`. Project-specific bindings belong here; none weakens a shared rule.

| Shared site | Binding here | Reversal |
| ----------- | ------------ | -------- |
| Rust Shape: crate names, categories, and data path | Product crates are `p1689` and `cpp-deps`; `module-smoke` exercises their native build integration. Changelog tooling is externally installed, not a workspace member. The compiler pipeline is source scanning, dependency ordering, then compilation. A category prefix adds no distinction to these roles. | Another crate category needs a distinct name. |
| Rust Correctness: checker and machine | The p1689 model and module scheduler are the correctness engines. | Another engine owns either invariant. |
| Rust Representation: `Maybe<T, R>` | Build the type in the owning crate at its first reasoned absence; change callers and signatures together. | A shared crate supplies the type. |
| Rust Enforcement and Verification | `Cargo.toml` owns the lint wall; `mise run check` runs local gates. External compiler-plugin commands describe a different consumer role. | This workspace adopts that policy. |
| Source workflow: project index and release history | `mise.toml` pins Codegraph; `mise exec -- codegraph init` indexes source. `cliff.toml` keeps local rendering policy. `mise run changelog:render` installs the public `workflow-changelog` binary from reviewed Git revision `60169a4e02a4afb5e21eccd3779ab3d29a0b38ea`, then renders PR squash history or committed history. Treefmt Markdown-normalizes and compare-installs `CHANGELOG.md` with cache disabled; `treefmt:check` is the sole gate. CI fetches tags and history and grants read-only PR lookup to the format job. | The source goes private, tag namespace changes, or merge method changes. |
| Local CI: jobs and platform lanes | `.github/workflows/ci.yaml` runs Rust and module integration on GCC 16 and Clang 22; `.github/workflows/ci-image.yaml` publishes the shared image. Other platform lanes are included when the build supports them. | Platform support or measurements change. |

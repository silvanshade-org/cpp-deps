# Source workflow

> Read when: building, testing, formatting, or gating the workspace, writing a commit, or landing a PR.
> Rust conventions the gates cannot check: [rust.md](rust.md). Workflow iteration on CI itself: `ci-local.md`, which a tree adopts with its first workflow.

## toolchain-and-tasks

One toolchain for the whole workspace, pinned in `rust-toolchain.toml`; every local crate builds under it, and a Dylint library loaded from Git must use the same compiler. `mise` owns the tool pins (`mise.toml`, `mise.lock`) and the task list (`.config/mise/tasks/`); a pinned tool is reached through mise, never through a host copy that happens to be on `PATH`.

| About to | Run |
| -------- | --- |
| build everything | `cargo build --workspace --all-targets` |
| test everything | `cargo nextest run --workspace` |
| test as CI does | `cargo nextest run --profile ci --workspace` |
| format the tree | `mise run treefmt` |
| check formatting without writing | `mise run treefmt:check` |
| run every repo gate | `mise run check` |
| move a local library producer's toolchain | `mise run toolchain:bump <stable>` |
| move an external consumer's toolchain and policy | `mise run toolchain:bump --workflow-rev <full-commit>` |

`mise run check` fans out to every `check:*` task, so a new gate joins the wall by taking a `check:` name and nothing else.

A workspace consuming `gandr-lang/quenchant`'s policy loads its compiler plugin through `[[workspace.metadata.dylint.libraries]]`: the Git source, a full 40-hex `rev`, and `pattern = "crates/quenchant-dylints"`. `mise run workflow:install` reads that same revision and runs `cargo install --git --rev --locked` for `quenchant-gates`, installed under `target/workflow-tools`. Neither tool crate is a Cargo dependency or workspace member of the consumer; quenchant's library crates, such as `quenchant-shape`, are ordinary dependencies.

The consumer runs `mise run check:dylint`, which sets `CARGO_INCREMENTAL=0` and invokes `cargo dylint --lib quenchant_dylints --no-deps -- --workspace --all-targets`. `check:anodized` and `check:witnesses` depend on the pinned installer and invoke the binary with `--manifest-path Cargo.toml`. Their explicit manifest names the workspace being checked; an installed binary must never infer it from its source checkout. `check:publish` rejects the two tool packages anywhere in Cargo's resolved package graph, preserving the future publication boundary.

A library producer owns its plugin's unit/UI suites and Clippy pass: run those from the package directory where its linker configuration applies. Consumers run their entire workspace suite without a workflow-package exclusion; they do not repeat the producer's UI fixtures. Both roles require the pinned `cargo-dylint` and `dylint-link`, plus the library-compatible `rustc-dev` and `llvm-tools` components. After a compiler or policy upgrade, clear `~/.dylint_drivers` and `target/dylint`, then rerun the complete build, test, Clippy, rustdoc, formatting, and policy wall. A workspace adopting no Dylint policy runs its ordinary Clippy wall alone.

## tooling-posture

Tooling is not accumulated. A tool, a task, or a gate that has stopped being useful is removed rather than parked: a parked one keeps its dependencies, its pin, and its place in the wall, and reads as current to whoever finds it next.

Open every new repository with an empty root commit before its first content commit. That stable base lets later history rewrites use an ordinary rebase instead of `git rebase --root`.

**Project index, every language.** Every new project MUST retain a project-local Codegraph mise pin and run `mise exec -- codegraph init` from its root during setup, after initial source files exist. Confirm completion with a query returning an indexed source symbol. The shared tool template supplies the pin; a documented platform/tool failure requires an explicit exception, never silent omission. Indexing belongs to setup, not build or hardware gates.

**Rust for everything possible.** Bespoke tooling is a small binary in the workspace, never a script. A task may invoke a tool; it never becomes one — the moment a task body carries logic rather than a launch line, that logic moves into a crate, with the conventions and the tests every other crate owes. Scripts already in a tree are retired by the crates that replace them, on a burn-down the tree tracks; new ones do not open.

Two narrow exceptions, admitted per project and never standing:

- TypeScript, where a surface is genuinely unsuitable for Rust. The test is interfacing with an ecosystem that has no Rust path, never preference or familiarity.
- Mojo, which stands to Python as TypeScript stands to JavaScript: where the work must reach a Python ecosystem, it is the cheaper exception, because it does not maintain a second remapped surface.

A configuration file in another ecosystem's language is not tooling. A tool whose configuration is necessarily JavaScript or TypeScript keeps that file, and the formatter entries covering it stay; the rule above is about programs, not settings.

Each project keeps one root `CHANGELOG.md`, generated by `git-cliff` from committed Conventional Commits under checked-in `cliff.toml` only when preparing a release. Release generation fetches full history and tags, ignores pre-release tags and merge commits, and keeps version-specific surface prose in release commit bodies rather than READMEs. Crates keep no separate changelogs. Ordinary PRs never regenerate release history; treefmt only formats the tracked Markdown. No generator installation, synthetic PR-title commit, PR token, or separate changelog gate belongs in formatting.

Prefer the task to the bare binary. A task body carries the pinned tool, the environment, and the flags the tree has settled on, so the same binary run by hand carries the host's instead — and its output is not the gate's output.

## gates

A new tree's hosted CI MUST open in gandr's shape, minus lanes that do not apply, as specified in [ci-local.md §Container image](ci-local.md#container-image); NEVER start with bare-runner-only CI.

| Gate | Command | Refuses |
| ---- | ------- | ------- |
| conflict markers | `mise run check:conflict-markers` | an unresolved Git conflict marker in a tracked file |
| private paths | `mise run check:private-paths` | a private-material reference, or a tracked file under a refused directory |
| shared baseline | `mise run check:baseline-hash` | a `docs/agents/baseline.md` that is not the pinned page |
| rustdoc | `mise run check:doc` | a rustdoc lint over private items |
| CI filter | `mise run check:ci-scripts` | a regression in the changed-categories filter |
| CI pins | `mise run check:ci-pins` | a workflow tool pin drifted from its source of truth |
| external Dylint policy | `mise run check:dylint` | a compiler-plugin finding in the consumer workspace |
| specification state | `mise run check:anodized` | an unresolved or non-enforcing specification configuration in the workspace |
| adequacy witnesses | `mise run check:witnesses` | missing, ambiguous, or wrong-target runnable witnesses |
| publication surface | `mise run check:publish` | unpublished workflow tools entering the Cargo dependency graph |
| formatting, size budgets, shell lint | `mise run treefmt:check` | a file the formatter would rewrite, an over-budget page, a shellcheck finding |
| clippy wall | `cargo clippy --workspace --all-targets -- --deny warnings` | any lint the `[workspace.lints]` wall denies |

The table and hook tiers describe a Rust consumer's wall. The shared task set supplies conflict-marker, baseline-hash, rustdoc, formatting, and external-workflow tasks; a repository adopts the workflow tasks only when it consumes that policy. The shared hook floor is config validation, conflict markers, formatting, and the commit message. The private-path boundary, CI filter, pin-drift check, and push tier are each a repository's own, and the Clippy wall runs wherever a workspace carries the `[workspace.lints]` tables.

`prek` installs the local hooks (`prek install`, hook types from `prek.toml`): the conflict-marker, private-path and formatting gates before every commit, commitlint on the message, and the push-identity guard before every push. The guard resolves the credential the push would use and refuses an unexpected login. commitlint runs again in CI, over each PR's commits ([ci-local.md §Merge settings and review](ci-local.md#merge-settings-and-review)).

A task that takes arguments declares them. An undeclared value is appended to the last line of the task body, so a task read as parameterized runs unparameterized and silently: declare the usage, and the argument becomes a named value.

`prek` stashes unstaged changes before it runs, so hooks see staged content only. A partial `git add` can therefore fail on a defect the working tree has already fixed, and a change to the hook configuration itself takes effect only once staged.

A formatter cache keys on file content and does not invalidate when its own configuration changes. A configuration change is verified by running the formatter once with its cache disabled; a cached pass proves nothing about the new setting.

Content a gate cannot read is content the gate does not check. Before a new kind of embedded block enters a tree — a diagram language, a generated table, an included fragment — name what verifies it, or record that nothing does.

## commits

Conventional Commits, enforced by commitlint (`commitlint.config.mjs`) in the prek `commit-msg` hook, then in CI over each non-merge commit a PR or merge-group entry adds:

- Type and scope both required, each from the closed vocabulary in `commitlint.config.mjs`. Crate scopes name a `crates/<category>-*` category, never one crate directory, so a scope survives a crate split. Grow either list deliberately via PR; per-surface growth is the failure mode.
- Subject: no trailing period, at most 72 header characters, 50 where the subject fits.
- Body: blank line after the subject, lines at most 100 characters. Body only where the "why" is not obvious from the subject.
- Trailer block preceded by a blank line (`trailer-leading-blank`; the stock `footer-leading-blank` misfires on wrapped prose and stays disabled).
- `Assisted-by: LLM` on every commit an LLM helped write, project and upstream-bound alike. It names no model or tool, and replaces the assistant's `Co-authored-by`, which credits people only.
- Human trailers (`Fixes`, `Refs`, `Reviewed-by`, …) where meaningful.

**Merge commits only.** Squash and rebase merging are off; each PR lands as a merge commit keeping its authored, signed commits. MUST autosquash fixups (`git rebase --autosquash`) before queueing. Settings: [ci-local.md §Merge settings and review](ci-local.md#merge-settings-and-review).

**Dependent work MUST be a native stack from its first dependent PR.** Each `gh stack` PR targets its parent. `gh stack merge` queues the whole stack, which lands together, each PR its own merge commit in stack order. The queue's method overrides any method passed; auto-merge never applies to a stack. NEVER hand-rebase a child after its parent lands. `gh stack link` retargets bases that disagree with the chain: check each edge's ancestry before linking.

**An approval survives a code-unchanged rebase:** `git range-diff` shows no code change and required checks pass. Stale-review dismissal is off. The lander MUST record the range-diff in the landing note. A failing check or a code change needs review of the change; a conflict resolution is a code change, reviewed as the resolution only.

**CodeRabbit reviews on request only** (`@coderabbitai review`): new design or architecture, safety-critical code or a bug fix, performance-centric code. NEVER request it for other changes; unsure, ask the owner. The landing note MUST name the class, or the skip and its reason.

## shared-baseline

`docs/agents/baseline.md` is one shared page, byte-identical wherever it is tracked and pinned by content hash. Editing it in place breaks `check:baseline-hash`: change the page and its pinned hash in the same commit, and the copies elsewhere move with it.

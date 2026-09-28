# Module build and cache architecture

This document explains borrowed P1689 parsing, dependency planning and validated local module artifact caching. The README describes the build-script API and compiler capability boundaries.

## Why names and edges are insufficient

A dependency graph answers which compilation must precede another. It does not establish whether an existing compiled artifact is valid, where the compiler expects it, or which other files must accompany it.

A module's logical name does not uniquely identify its compilation. Identical source bytes can produce incompatible artifacts under different toolchains, targets, flags, imported BMIs, headers, environments and path contexts. One action can produce an object, a binary module interface, an ordinary depfile and structured dependency metadata. A logical name does not determine their filenames. Content-addressing identifies bytes; it does not make those bytes compiler-compatible or relocatable.

The parser, planner and cache must therefore preserve module identity, dependency evidence and artifact metadata end to end.

## Ownership and planning

The compilation session owns scanner-output buffers. Parsed records use `Cow<[u8]>`: ordinary strings borrow input, while escaped values own unescaped bytes. Buffers outlive parsed and graph views. This boundary avoids self-reference, Yoke and graph reference counting. Graph construction does not convert every field to owned storage. The scanner parser performs no UTF-8 or control-byte validation and recognizes literal schema keys directly. Unix native paths and arguments retain raw bytes; other platforms require UTF-8 when constructing native strings.

Field dispatch selects literal names by discriminating prefix bytes. Quote/backslash finders are initialized once per document and selected by target features: AVX2, SSE2 or portable word scanning. Escape handling uses lazy allocation, reusable four-byte Unicode scratch and surrogate handling. A 65,536-entry paired-hex table decodes four hex digits through two lookups, with a 256 KiB logical footprint. Changes require byte-equivalent results and measurements on ordinary and escape-heavy inputs.

Node records occupy flat arena storage. Dependency edges carry typed indices, distinct from module-identity indices. Dropping the graph releases flat allocations without following recursive ownership chains. A deterministic ready frontier and indegrees drive scheduling. Transitive import lists are computed only where compiler invocation requires them; reusable indexed membership avoids repeated linear searches. Materializing every transitive closure can itself require quadratic output space and is not a scheduling prerequisite.

## Identity layers

| Identity | Purpose | Rule |
| -------- | ------- | ---- |
| Node index | Address a translation unit in the current graph | Never persist it as compilation identity |
| Module identity | Resolve a provided or required module | Preserve decoded name, source-unique identity and lookup semantics |
| Candidate key | Optionally find potentially reusable dependency histories | Not used by the current fresh-discovery implementation; a candidate match alone is never a hit |
| Action key | Identify a compilation request and its validated inputs | Include all modeled inputs with versioned, unambiguous encoding |
| Artifact digest | Identify the actual bytes of one result or imported file | Keep separate from action identity and compatibility |
| Artifact destination | Describe the role and location needed by a consumer | Persist the mapping; do not derive it from the module name alone |

`unique-on-source-path` affects identity rather than merely decorating a name. `lookup-method` preserves by-name, quoted-include and angle-include distinctions even where the compiler adapter rejects unsupported header-unit builds. Relative paths use the rule's working directory or the captured invocation directory when the field is absent. Basename matching, lossy path conversion and speculative canonicalization must not merge distinct inputs. Symlinks and path-remapping flags can change both resolution and embedded output data.

Preserve provided and required descriptors, source paths, compiled-module paths, interface status, primary output and additional outputs. A parser's ability to read a field does not establish that a particular scanner emits it.

Generated object, scanner-evidence and BMI destinations use a full BLAKE3 digest of the canonical absolute source path in the `cpp-deps/source-path/v1` domain. Reordering the input list cannot rename artifacts or accidentally bind an old graph index to a different source. Editing a source retains its destination but changes its validated action identity. This is deliberately path-sensitive, not a claim of cross-worktree portability or artifact-byte determinism. Compiler-reported paths still take precedence.

## Compiler dependency evidence

A compiler-reported `compiled-module-path` is the preferred location for an imported BMI. For a module managed in the current graph, a validated build-plan mapping can resolve an absent path. Unresolved external imports are errors or explicitly uncacheable operations; they do not disappear from the action key.

The imported artifact contributes its actual content identity. Managed producers can pass validated result records and artifact digests to consumers. External or prebuilt BMIs require byte hashing and compiler-compatibility checks. Reuse an existing digest only while immutable storage or file-identity validation justifies it. An action key alone cannot stand in for the actual output of a nondeterministic compiler.

P1689 records module relationships and artifacts. Ordinary depfiles remain necessary for textual includes. Capture system headers as well unless an immutable toolchain boundary already covers them. Generated inputs must be available before their consumer is validated or compiled. Discovered inputs become Cargo rerun dependencies on both a compile and a cache hit.

A previous depfile lists files that were read. It does not prove that the same search will select them now. A new earlier-search-path header, changed symlink, newly available `__has_include` target, modified module map or changed search root can alter compilation without changing any listed file. Validate current dependency discovery and resolution, or obtain an equivalent complete witness from the compiler. When such evidence is unavailable, rediscover before claiming a hit. Directory timestamps alone are not a general resolution witness.

## Discovery and compilation are distinct

Cold builds need enough dependency information to order translation units before imported modules exist. Compilation can then emit richer structured metadata using the module files it actually loads. Compiler adapters must distinguish scan-time capabilities from compile-time emission capabilities.

Single-pass structured dependency emission can avoid redundant scanning when a validated graph is already available. A manifest generated after compilation cannot order an unknown cold graph before compilation. Missing optional fields are explicit capability boundaries, not permission to invent artifact locations.

## Key encoding and stable identification

Use BLAKE3 with separate, versioned domains for complete action identity, manifests, dependency evidence and artifact contents. Encode typed fields, lengths and sequences explicitly. Concatenating unframed strings is ambiguous. Preserve order where it affects compilation, especially command arguments, search paths and import bindings; sort genuine sets deterministically. Change the domain version when encoding or dependency semantics change. Any optional candidate index needs a separate lookup domain.

Compiler expansion queries run the actual driver directly, with the compiler arguments and command context reconstructed from the configured invocation. The real compile remains wrapped by ccache. Asking the wrapper for `-###` is not equivalent: cached diagnostics and path rewriting can make its trace depend on whether an object already exists rather than solely on the current compiler request.

A complete action key includes:

| Input class | Required information |
| ----------- | -------------------- |
| Toolchain | Compiler family, target, ABI and validated toolchain identity |
| Invocation | Effective ordered arguments, response-file contents and relevant environment |
| Path context | Working directory, search order, module mappings and compiler path-remapping semantics |
| Source | Actual source contents and its semantically relevant identity |
| Textual inputs | Current resolved headers and generated files with their contents |
| Imported modules | Module bindings, actual artifact digests and compatibility constraints |
| Result layout | Semantically relevant output roles and destination context |

Compiler version text or executable bytes alone do not identify a mutable toolchain. Resource headers, shared libraries, plugins, compiler specs, subordinate tools and configuration can affect results. Use a content-identified immutable toolchain boundary or validate the relevant toolchain closure. An externally supplied toolchain identity is an attestation with a documented caller obligation. It must not silently hide omitted mutable inputs.

Preserve the caller's language-standard selection; do not append a conflicting default. Do not include graph indices, process IDs or scheduler arrival order in semantic keys. Conversely, do not remove path-sensitive inputs merely to increase hit rates. Conservative misses across working directories are correct until a validated mapping establishes equivalent compiler-observable paths. Artifact identity and artifact portability are separate properties.

## Cache operation

A local cache lookup follows these steps:

1. Scan the current sources to construct the module graph. Process imported producers before validating consumers.
2. Expand the actual compiler driver plan, admit its persistent output destinations, and perform fresh preprocessing and textual dependency discovery with the resolved import bindings. Validate toolchain, effective invocation, path context, source and current textual dependency resolution.
3. Hash actual imported artifacts and current source/header contents. Generated mapper identity uses its path and bytes, not regeneration timestamps. Scanner JSON ordering is not semantic identity: resolved bindings contribute through compiler arguments and imported files.
4. Construct the complete action key and load its result manifest with bounded lengths and an explicit schema version. Check exact required roles, destinations, artifact sizes and content digests against the live inventory.
5. Stage and verify every required blob before replacing destinations. Restore the complete result set and modification times before releasing dependent nodes. Emit Cargo rerun dependencies from the restored depfiles, excluding managed build products.
6. Compile on an absent, incompatible, incomplete, corrupt or insufficiently validated result. Missing required inputs remain actionable build failures, not successful cache fallbacks.
7. Collect final dependency and output metadata. Reconcile actual inputs with the proposed action key and validate input stability before publishing reusable results.

Pre/post file metadata equality alone cannot exclude arbitrary same-size, same-time mutation. Correct reuse requires immutable input snapshots or an explicitly stated cooperative-filesystem assumption with content revalidation. An operation with unstable input evidence must not publish a reusable result. Ordinary filesystem reads do not provide adversarial snapshot isolation.

Fresh discovery precedes every lookup; no candidate-manifest index substitutes for it. An optional index could retain several dependency histories for one invocation, but its existence or recency would not establish validity. Cache eviction changes performance, not compilation semantics.

## Result records and restoration

A result record contains its action identity and artifact inventory; stored depfiles retain textual dependency evidence. Each artifact records its role, content digest, size, modification time and destination mapping. Distinguish objects, BMIs, ordinary depfiles, structured dependency files and compiler-specific additional outputs. The adapter includes compiler-reported outputs and driver-declared saved intermediates, split DWARF, coverage notes, serialized diagnostics and thin-link bitcode. Runtime coverage data is not a compilation output. Driver word parsing never executes shell text.

Metadata must not authorize writes outside admitted output locations. Compiler-generated destinations need validation just as caller-configured destinations do. Store and restore a complete operation result; an object-only hit does not satisfy a module compile that also promises a BMI and dependency metadata.

Write immutable blobs through unique staging areas and publish a checksummed complete manifest by atomic rename. Concurrent writers can share immutable content, but each manifest selects one coherent artifact set. Restoration verifies all blobs in staging before any destination replacement. A failed compile, interrupted write or partially published record never becomes a hit. Corrupt or incomplete records trigger recompilation without releasing consumers on partial results.

Multiple independent destination writes are not an atomic result. An exclusive output-root session lock serializes cooperating builds across processes. A per-session ownership registry rejects output collisions between different sources; only the coordinator's shared module mapper is exempt. Scheduling exposes completion only after every destination is restored. External readers must honor the same session boundary; arbitrary filesystem readers do not receive atomic multi-file visibility.

BMI compatibility remains tied to compiler, target, flags and path constraints. Neither equal source nor a valid blob digest permits relocation or reuse across incompatible compiler configurations.

## Cargo, ccache and parallel work

The module cache owns graph-aware action records and coherent artifact reuse. Native compilation remains wrapped by ccache and emits ordinary depfiles. A validated outer hit skips the invocation. An outer miss executes through the wrapper and verifies that all required artifacts exist before publication. Do not assume an inner object-cache hit also restores a BMI or structured dependency file. There is no need to implement a second ccache storage format or override host cache settings.

A coordinator owns graph mutation and readiness. Without an explicitly supplied jobserver client, native jobs run serially under the caller's implicit slot. With a client, a portable acquisition helper supplies additional tokens without blocking that slot. Worker batches retain their tokens through completion, including failure. An explicit parallelism ceiling can further bound capacity; no independent core-count pool oversubscribes Cargo. Scanning is serial. Workers receive indexed tasks and reusable traversal scratch rather than shared recursive graph ownership.

Reuse buffers and validated digests within an invocation instead of rereading a shared BMI for every edge. Any cross-invocation digest reuse needs its own file-identity or immutability justification. Performance optimizations must preserve the same input-validation obligations.

## Verification obligations

Correctness observations cover ordinary and escaped string equivalence, malformed escapes and surrogates, source-qualified module identity, missing and duplicate providers, partitions, cycles, deterministic scheduling and equivalent compiler import arguments. Exercise long-chain and wide graphs and flat destruction.

Cache scenarios include unchanged warm builds; direct and transitive header edits; a newly shadowing include; removed files; changed imported BMI bytes; response-file, flag, environment and toolchain changes; output-path changes; missing or corrupt artifacts; interrupted publication; and concurrent producers. Observe complete restored artifacts and linked consumer behavior rather than relying only on hit counters.

Measure parser throughput with equivalent work and timing boundaries, representative many-file inputs, escape-heavy inputs, allocation counts and uncached library build cost. Separate graph construction, transitive import production, hashing, dependency validation, scheduling, materialization, compilation and destruction. Warm no-op latency is an acceptance path, not a reason to omit dependency validation. Benchmark scope and filesystem/toolchain assumptions accompany every result.

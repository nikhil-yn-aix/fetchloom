# Decisions

## Toolchain and crate selection

Question: Which crate, at most one per job, for argument parsing and shell completion, terminal styling, progress rendering, BLAKE3, SHA-256 with hardware acceleration, raw platform syscalls, typed errors, structured events, unit and integration testing, and benchmarking, for a single static binary on six targets.

Options: For each job, the leading crates plus the option of writing no dependency at all. Full candidate list and measurements below.

Chosen:

| Job | Chosen | Version | Cost | Maintenance | Rules out |
|---|---|---|---|---|---|
| Argument parsing | `clap` derive | 4.6.6 | 574 KiB release overhead, 2 ms parse | 2026-08-06, 222M recent downloads, MSRV 1.85 | Sets the workspace MSRV floor at 1.85. Largest single contributor to binary size |
| Shell completion | `clap_complete` static generation (`aot`) | 4.6.9 | Build-time only, no runtime cost | 2026-08-06 | Dynamic completion (`env::CompleteEnv`) is still `unstable-dynamic`, so completions must be emitted by a command, which the command surface does not define |
| Terminal styling | `anstream` + `anstyle` | 1.0.0 / 1.0.14 | Already in the graph via clap's `color` feature | 2026-02-11 / 2026-03-13 | Nothing. Provides Windows console VT enabling and stripping on non-terminal streams, which a hand-rolled writer would have to reimplement |
| Progress rendering | none, written here | n/a | n/a | n/a | Rules out `indicatif`'s draw target and steady-tick thread as a second renderer |
| Terminal size | none, `rustix::termios::tcgetwinsize` and `GetConsoleScreenBufferInfo` | n/a | n/a | n/a | Removes `terminal_size` 0.4.4 from the graph; both backends are already present |
| BLAKE3 | `blake3` with `std` and `rayon`, using the `hazmat` module for tree work | 1.8.7 | 6.9 GiB/s single thread with AVX-512 on 16 KiB inputs | 2026-08-20, 40M recent downloads, by the algorithm's author | Rules out `bao` 0.13.1 and `bao-tree` 0.16.1 as the outboard implementation |
| SHA-256 | `sha2` | 0.11.0 | Runtime backend selection, no startup cost | 2026-03-25, 233M recent downloads | Rules out `ring` and `aws-lc-rs`, which need a C or assembler toolchain on all six targets |
| Raw platform syscalls | `rustix` on Unix, `windows-sys` on Windows, `libc` for two Apple calls rustix does not wrap | 1.1.4 / 0.61.2 | Header-only bindings, no runtime cost | 2026-02-22 / 2025-10-06, both above 240M recent downloads | rustix on Windows is Winsock-shaped only, so it cannot be the single answer |
| Typed errors | none, written here | n/a | n/a | n/a | Rules out `thiserror` 2.0.20 and the proc-macro build cost it carries |
| Structured events | `serde` + `serde_json` | 1 / 1.0.151 | Serialization only, no background machinery | 2026-07-20, 288M recent downloads | Rules out `tracing` and `tracing-subscriber` |
| Testing | built-in libtest + `tempfile` | 3.27.0 | Dev-only | 2026-03-11, 172M recent downloads | Rules out `assert_cmd`, `insta`, `proptest` for now |
| Benchmarking | none, an `xtask bench` regime harness written here | n/a | Dev-only | n/a | Rules out `criterion` 0.8.2 and `divan` 0.1.21 |
| Allocator | `mimalloc` on `*-unknown-linux-musl` only | 0.1.52 | Adds a `cc` build step for those two targets | 2026-05-22 | Adds a C compiler requirement to the two musl targets |

Because:

Argument parsing. The published parser benchmarks on rustc 1.94.0, Linux x86_64, report identical release parse times of 1 to 2 ms for `clap`, `clap_lex`, `argh`, `gumdrop`, `lexopt`, `pico-args`, `xflags` and `bpaf`. Parser choice therefore does not move the no-op regime; only binary size separates them, and there clap costs 574 KiB against 24 KiB for `pico-args`. The command surface has thirteen commands, eleven cache subcommands, thirteen global flags and twenty-one `get` flags, with enumerated values, repeatable flags and a required `--help` that assumes no prior knowledge. Reimplementing that on `lexopt` is a second parser and a second help renderer written here, which is the thing standards.md calls a defect. Half a megabyte is the price of not owning it.

Shell completion. `clap_complete` 4.6.9 documents two paths. Static generation through the `aot` module is stable. Dynamic completion through `env::CompleteEnv` is gated behind `unstable-dynamic` and its modules carry that marker throughout the API docs, so it cannot ship in a 0.x that promises one behavior. Static generation needs a command to write the script to stdout.

Terminal styling. clap's `color` feature already pulls `anstream`, so choosing it costs nothing new and gives the Windows console virtual-terminal enabling, the `NO_COLOR` and `CLICOLOR` handling, and the strip-when-not-a-terminal behavior that contracts.md requires. Choosing `owo-colors` or hand-rolled escapes would leave `anstream` in the graph through clap anyway, which is two ways to write a color.

Progress rendering. contracts.md requires exactly one aggregated line redrawn at a fixed rate, never one indicator per file, and it requires the `live` view to be a pure consumer of the event stream with no other input. `indicatif` 0.18.6 is built around per-item `ProgressBar` handles, a `MultiProgress` coordinator and its own draw thread, and it has no notion of being fed an event stream. Using it for the aggregated line and writing the `live` renderer by hand produces two renderers with two draw paths. One renderer that consumes events and paints either mode is one code path, and it is the smaller amount of code.

Terminal size. `rustix::termios::tcgetwinsize` exists behind the `termios` feature and `rustix` is already the Unix syscall answer; `GetConsoleScreenBufferInfo` is in `windows-sys`. `terminal_size` 0.4.4 would be a third crate wrapping calls already reachable.

BLAKE3. `blake3` is the reference implementation by the algorithm's author, with runtime SIMD detection on x86 and NEON on aarch64, so one binary runs everywhere as standards.md requires. Its `hazmat` module is public and documented as intended for "projects like Bao, which work directly with the interior hashes of BLAKE3 chunks and subtrees", exposing `HasherExt::set_input_offset`, `finalize_non_root`, `merge_subtrees_root`, `merge_subtrees_non_root` and `left_subtree_len`. That is exactly the surface the outboard tree needs. `bao` 0.13.1 was last published 2025-04 with 14.8k recent downloads and is effectively unmaintained. `bao-tree` 0.16.1 is actively maintained (2026-08-26, 331k recent downloads) and would save real work, but it imposes its own outboard encoding and chunk-group model, pulls `bytes`, `range-collections` and `positioned-io`, and contracts.md leaves the outboard format to be settled in slice 0.3. Adopting a crate's format before deciding the format is the wrong order.

SHA-256. `sha2` 0.11.0 selects its backend at runtime through `cpufeatures`: `x86-sha` for SHA-NI, `aarch64-sha2` for the ARM SHA2 extension, falling back to `soft`. No compile-time target feature is needed, so the portable-baseline rule holds. `ring` and `aws-lc-rs` are faster in places but require a C or assembler toolchain on all six targets including `aarch64-pc-windows-msvc`, which conflicts with a single static binary built from a plain cargo invocation.

Raw platform syscalls. `rustix` 1.1.4 covers the Unix surface phase 0 needs: `openat`, `renameat`, `linkat`, `symlinkat`, `unlinkat`, `mkdirat`, `statat`, `statfs`, `fstatfs`, `fsync`, `fdatasync`, `flock`, `copy_file_range`, `ioctl_ficlone`, `fallocate`, plus `termios::tcgetwinsize` and `isatty`. On Windows it exposes only Winsock-shaped APIs, so Windows file, lock, volume and console work goes through `windows-sys` 0.61.2 directly. Two crates because the platforms are two platforms, not because there are two ways to do one thing.

Typed errors. contracts.md gives every error the same seven fields plus a kind. That is one struct with a kind enum, one `Display` impl and one `std::error::Error` impl, written once. `thiserror` derives `Display` from format strings, which is the part standards.md wants written as fields rather than sentences. It buys nothing here and costs a proc-macro in the build graph.

Structured events. contracts.md fixes the event names, requires newline-delimited JSON with a monotonic sequence number, and makes the `watch` command and the `live` display consumers of that stream. That is a typed domain event, not a log record. `tracing` would model it as spans and dynamic fields and then need a subscriber to reshape it back into the contract, and standards.md already forbids a second way of emitting the same fact. `serde` and `serde_json` are needed anyway for the JSON result, the lock, the receipt, the plan and the canonical manifest form.

Testing. Contract tests spawn the binary, read stdout and stderr, and assert exit codes, parsed JSON and event sequences. `std::process::Command` plus `serde_json` does that. `assert_cmd` is convenience over the same calls; `insta` snapshots encourage asserting rendered text, which standards.md calls a bad test. `tempfile` is kept because secure per-test directories with reliable cleanup on all three platforms is not worth reimplementing. `proptest` 1.11.0 is deliberately deferred: it belongs with the canonical entry stream in slice 0.6, and adding it now would be a dependency with no test behind it.

Benchmarking. standards.md names eight regimes: no-op run, cold cache, warm cache, interrupted transfer, many small files, one large file, slow disk, constrained network. It also states that a benchmark excluding verification and extraction time is invalid, and that the no-op regime measures startup and configuration discovery. All of those are whole-process measurements against real filesystem state. `criterion` 0.8.2 measures in-process functions with statistical resampling and cannot express process startup, a cold cache, or a killed and resumed transfer. Its bootstrap confidence intervals also do not map cleanly onto the flat five percent regression gate. `divan` 0.1.21 has the same shape and was last published 2025-04-10. The harness is an `xtask` subcommand that runs the real binary N times per regime, records wall time, bytes read and written, peak resident memory and binary size as JSON, and compares against a committed baseline with a five percent gate.

Allocator. musl's `mallocng` is the documented weak point of static Rust binaries, with reported slowdowns from 2x to 20x under multithreaded allocation, and `mimalloc` measured on par with glibc where musl's default was far behind. Every hot path here is multithreaded. The evidence is blog-grade rather than a controlled study, so this is provisional: slice 0.2 measures the many-small-files and no-op regimes on `x86_64-unknown-linux-musl` with and without `mimalloc` and the numbers decide.

Targets and linking. Linux ships `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, fully static. Windows ships `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` with `-C target-feature=+crt-static`, which statically links both the VC runtime and the UCRT and removes the `vcruntime140.dll` deployment requirement. macOS ships `x86_64-apple-darwin` and `aarch64-apple-darwin` linked against `libSystem`, which cannot be static and is present on every macOS.

Costs:

clap is 574 KiB of the binary and pins MSRV at 1.85. If the no-op regime budget cannot absorb it, replacing it later means writing a parser and a help renderer, not swapping a crate.

Choosing `blake3::hazmat` over `bao-tree` means the outboard encoding, the range proof format and the verification walk are ours to write and test in slice 0.6 and phase 5. Every tree shape must be checked against `blake3::hash()` of the same bytes, including the one-chunk, exact-power-of-two and final-partial-chunk cases.

On `aarch64-pc-windows-msvc`, `cpufeatures` 0.3.1 documents aarch64 detection as "Linux, iOS, and macOS/ARM only", so `sha2` falls back to the `soft` backend there. The interop digest will be several times slower on Windows ARM than on the other five targets. It is a speed difference, not a correctness one, and it is not fixable without a compile-time target feature that would break the portable-baseline rule.

Writing the progress renderer, the error types, the event writer and the benchmark harness by hand is four pieces of code that a dependency would have supplied. Each is small and each is now maintenance.

`cargo-deny` is added as a development tool, not a dependency; see the lint and dependency policy record.

Uncertain: the 574 KiB clap figure and the 2 ms parse figures are from the `rust-cli/argparse-benchmarks-rs` suite on rustc 1.94.0 Linux x86_64 and are not measured here or on Windows. The `anstream` contribution to binary size has not been measured separately. The musl allocator numbers are third-party blog measurements. All three are replaced by measurements from the slice 0.2 harness.

Sources: crates.io API for every version and date; docs.rs for `sha2` 0.11.0, `blake3` 1.8.7 and its `hazmat` module, `rustix` 1.1.4 `fs` and `termios` indexes, `clap_complete` 4.6.9, `cpufeatures` 0.3.1; `rust-cli/argparse-benchmarks-rs` README; BLAKE3 specification benchmarks; Tweag and nickb.dev on musl allocator performance; `ChrisDenton/static_vcruntime` and the Rust `crt-static` RFC.

## Repository and crate layout

Question: What workspace layout matches the six seams in docs/roadmap.md while keeping the engine free of any knowledge of an adapter.

Options: One crate; a crate per seam with the traits duplicated; a core crate holding traits plus a separate orchestration crate; the engine crate owning the traits with adapters depending on it.

Chosen:

```
Cargo.toml                workspace, [workspace.lints], [workspace.dependencies]
rust-toolchain.toml       pinned stable
deny.toml                 cargo-deny configuration
crates/
  engine/                 seam traits, domain types, digests, errors, events, orchestration
  platform/               Platform seam: Windows, macOS, Linux
  cache/                  Store seam
  sources/                Source seam
  archive/                Archive seam
  cli/                    binary: parsing, config, Policy, Observer sinks, rendering
  faults/                 fault injection, used by cli and by tests
xtask/                    bench harness, comment check, conformance driver, completion generation
tests/                    cross-crate contract and conformance tests
```

Dependency direction. `engine` depends on no other workspace crate. `platform`, `cache`, `sources`, `archive` and `faults` depend on `engine` and on nothing else in the workspace. `cli` depends on all of them and is the only composition root. `xtask` depends on nothing in the workspace and drives the built binary as a subprocess.

Seam to crate mapping. Platform to `platform`. Store to `cache`. Source to `sources`. Archive to `archive`. Policy and Observer have no adapter crate: their traits live in `engine` and their only implementations are in `cli`, because both are decided entirely by configuration, flags and output streams. Trait definitions for all six live in `engine`, one module each.

Because: standards.md permits a crate boundary only where the code is genuinely separable and names exactly platform, sources, archive, cache, engine and cli. It also states that dependencies point inward and that adapters depend on the engine's traits, never the reverse. Putting the traits in `engine` and having adapters depend on `engine` is the only arrangement that satisfies both sentences without inventing a seventh crate. A separate `core` crate holding only traits would be a boundary standards.md does not list and would exist to make a diagram look right, which is the definition it gives of an unnecessary boundary.

`faults` is a crate rather than a test module because roadmap.md phase 0 lists the fault injection library in the Build column and standards.md states it is part of the product, not a mock in a test file. It depends on `engine` so it can implement the same traits the real adapters implement.

`xtask` is a crate rather than shell scripts because every check must run identically on Windows, macOS and Linux, and a shell script does not. It also keeps the benchmark harness, which drives the real binary, out of the shipped dependency graph.

Placing Policy and Observer implementations in `cli` keeps `engine` free of configuration precedence, terminal detection and credential store access, all of which are adapter knowledge.

Costs: `engine` holds both the seam traits and the orchestration, so an adapter compiles the orchestration code it never calls. Splitting later is a real refactor. `cli` is the only crate that can be integration-tested end to end, so contract tests concentrate there. Two builder sessions can only run in parallel when their slices land in different crates, which the phase 0 slice plan already assumes for 0.4 and 0.6.

Uncertain: whether `cache` and `engine` stay separable once leases and prune interact with orchestration. If they do not, that is a decision for phase 1, not a boundary to pre-emptively erase.

## Continuous integration for six targets

Question: How are all six targets built and tested, how are Windows and macOS runners obtained, and what is the slowest realistic job time.

Options: GitHub-hosted runners for all six; hosted runners for x86_64 with QEMU or cross-compilation-only checks for aarch64; a third-party runner provider; self-hosted hardware.

Chosen: GitHub-hosted runners, six build-and-test jobs plus one conformance stage, all on free public-repository runners.

| Target | Runner label | Arch | vCPU / RAM (public) |
|---|---|---|---|
| `x86_64-unknown-linux-musl` | `ubuntu-24.04` | x64 | 4 / 16 GB |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` | arm64 | 4 / 16 GB |
| `x86_64-pc-windows-msvc` | `windows-2025` | x64 | 4 / 16 GB |
| `aarch64-pc-windows-msvc` | `windows-11-arm` | arm64 | 4 / 16 GB |
| `x86_64-apple-darwin` | `macos-15-intel` | x64 | 4 / 14 GB |
| `aarch64-apple-darwin` | `macos-latest` | arm64 | 3 / 7 GB |

Every job is native: build, unit tests, contract tests and adversarial tests all run on the target they were built for. No emulation and no cross-compilation-only target, because capability detection, atomic publication and locking are exactly the behaviors an emulator gets wrong.

Conformance runs as a second stage. Each of the six jobs materializes the conformance corpus and uploads the tree plus its tree digest with `actions/upload-artifact`. A conformance job per platform downloads the artifacts from the other platforms, materializes each one locally and asserts an identical tree digest or the exact declared failure. Three platforms gives the six directions roadmap.md requires; the two architectures within a platform are covered by the same stage at no extra design cost.

Gate jobs, Linux only, run once: `cargo fmt --check`, `cargo clippy --all-targets` with the workspace lint table, `cargo xtask check-comments`, `cargo deny check`, and `cargo xtask bench --compare baseline` with the five percent gate.

Caching uses `Swatinem/rust-cache` with `CARGO_INCREMENTAL=0`.

Because: the GitHub-hosted runner reference lists `ubuntu-24.04-arm`, `windows-11-arm`, `macos-15-intel` and `macos-latest` (arm64) as standard runners, free and unlimited for public repositories, so all six targets are obtainable without payment or self-hosting. ARM64 Linux and Windows images moved from Arm Limited's `actions/partner-runner-images` to GitHub's own pipelines in May 2026 and are now first-party.

Costs and constraints:

`macos-15-intel` is the last x86_64 macOS image GitHub will offer and is available only until August 2027. After that date x86_64 macOS has no hosted runner and the target either moves to self-hosted hardware or is dropped. That is a dated deadline, not a risk.

`macos-latest` is the smallest runner in the matrix at 3 vCPU and 7 GB, so it bounds any test that assumes parallelism or memory headroom. Resident memory limit tests must fit in 7 GB.

Windows runners carry Defender on-access scanning, which contracts.md already requires Fetchloom to report rather than work around. That makes the Windows jobs both the slowest and the most representative, so scanning is left enabled.

The `windows-latest` and `windows-2025` labels are migrating to Visual Studio 2026 by default, so the Windows x64 job pins `windows-2025` and the toolchain is pinned in `rust-toolchain.toml` rather than floating.

Slowest realistic job: `windows-11-arm`, cold cache, estimated 10 to 14 minutes wall clock for build plus the full test suite, dropping to 4 to 6 minutes with a warm `rust-cache`. The x64 Windows job is estimated 1 to 2 minutes faster and the Linux jobs roughly half. This is an estimate from runner size, Defender scanning and a workspace whose largest dependencies are clap, blake3 and windows-sys. It is not measured. Slice 0.2 replaces these numbers with the real ones from the first green run, and if the cold Windows job exceeds fifteen minutes the matrix is reconsidered before anything depends on it.

Uncertain: whether `windows-11-arm` has a native `aarch64-pc-windows-msvc` Rust toolchain available through `rustup` without an x64-emulated fallback, and what that costs in build time. Verify first thing in slice 0.2.

Sources: GitHub Docs, GitHub-hosted runners reference; GitHub Changelog, upcoming image migrations, 2026-05-14; GitHub Changelog, new runner images in public preview, 2026-06-11; `actions/runner-images` issue 13046 on macOS 13 deprecation.

## Thread pool sizing policy

Question: How is available parallelism read on each platform, how are container CPU limits, process affinity and user-set limits honored, and how are the async runtime and the CPU pool kept separate.

Options: `num_cpus`; `std::thread::available_parallelism` alone; `available_parallelism` with per-platform correction; rayon's default global pool; tokio's blocking pool for CPU work.

Chosen: one function in `platform` returning the CPU budget, feeding one explicitly sized rayon `ThreadPool` owned by `engine`. Phase 0 ships no async runtime at all.

Detection. Start from `std::thread::available_parallelism()`, then correct per platform.

Linux. `available_parallelism` reads `/proc/self/cgroup` to determine the cgroup version, then `/sys/fs/cgroup/<path>/cpu.max` for v2 or `cpu.cfs_quota_us` and `cpu.cfs_period_us` for v1, computes `limit / period`, and takes the minimum of that quota and the popcount of the `sched_getaffinity` mask. Container limits and affinity are therefore already honored and nothing is added.

macOS. `available_parallelism` falls through to `sysconf(_SC_NPROCESSORS_ONLN)`. Nothing is added. On Apple Silicon this counts performance and efficiency cores together.

Windows. `available_parallelism` calls only `GetSystemInfo` and reads `dwNumberOfProcessors`, which ignores the process affinity mask, ignores job object CPU rate control, and reports only the current processor group so it caps at 64 on large machines. The std documentation states it "may overcount the amount of parallelism available on systems limited by process-wide affinity masks, or job object limitations". The `platform` crate therefore computes, through `windows-sys`, the minimum of the popcount of the mask from `GetProcessAffinityMask`, the count implied by `QueryInformationJobObject` with `JobObjectCpuRateControlInformation` when `JOB_OBJECT_CPU_RATE_CONTROL_ENABLE` is set, and `GetActiveProcessorCount(ALL_PROCESSOR_GROUPS)`.

User limit. The final budget is `min(detected, user_limit)` when a user limit is set, clamped to at least one. Raising the user limit above the detected value does not raise the budget; it clamps and emits `degrade` naming what was requested, what was used and why, as standards.md requires for every fallback. `explain` reports the budget and which precedence level supplied it. This needs a flag and an environment variable that contracts.md does not define; see the contract changes below. Until they exist, the budget is the detected value with no user override, and no flag is invented.

Pools. One rayon `ThreadPool` built with `ThreadPoolBuilder::num_threads(budget)`, never rayon's implicit global pool, so the size is explicit and no other crate can enlarge it. All CPU work enters it through `pool.install(...)`, which the rayon docs state makes "any attempts to use `join`, `scope`, or parallel iterators operate within that thread pool". `blake3::Hasher::update_rayon` is called inside `install`, so multithreaded hashing lands on this pool and never on the global one. The content digest and the interop digest run as two tasks on this pool so neither serializes the other, as standards.md requires.

Async runtime. Phase 0's exit criterion is a binary with no network code in it, so no async runtime is linked. When tokio arrives in phase 2 it is built with an explicit `worker_threads` count for network I/O only, its worker threads never run hashing, compression or filesystem work, and its blocking pool is bounded and used only for blocking filesystem syscalls. The rayon pool remains the only place CPU work runs. The two pools never share a thread.

Because: `available_parallelism` is the only reader that already handles cgroup v1, cgroup v2 and `sched_getaffinity` correctly on Linux, which removes `num_cpus` and any hand-written cgroup parsing from the graph. Its Windows implementation is documented and confirmed in the standard library source to be `GetSystemInfo` alone, which is a real gap on exactly the platform where job objects and affinity are most commonly used by CI systems and container runtimes, so the correction is necessary rather than defensive. An explicit rayon pool rather than the global pool is what makes the budget enforceable: a dependency calling `rayon::join` outside `install` would otherwise silently spin up a second pool sized to the raw core count.

Costs: the Windows correction is three Win32 calls in `unsafe` blocks in the `platform` crate, each needing a `SAFETY:` line and a test. `available_parallelism` explicitly does not account for `ulimit`, for NUMA regions or for an overcommitted VM host, and neither will Fetchloom. Sizing the pool once at startup means a cgroup quota changed mid-run is not observed. Running the interop digest on the same pool as the content digest means SHA-256 competes with BLAKE3 for the same threads; on `aarch64-pc-windows-msvc`, where `sha2` falls back to the soft backend, SHA-256 becomes the slower of the two and will dominate that pool.

Uncertain: whether restricting the pool to performance cores on Apple Silicon, through `sysctlbyname("hw.perflevel0.logicalcpu")`, beats using all logical cores for hashing. No measurement was found either way. Default is all logical cores; slice 0.2's one-large-file regime on `macos-latest` settles it. Also unverified: whether `QueryInformationJobObject` CPU rate control is expressible as a whole-core count in every configuration, or only as a percentage that must be rounded up.

Sources: `std::thread::available_parallelism` documentation and the standard library `sys/thread/unix.rs` and Windows thread sources; `rayon::ThreadPool::install` documentation; `blake3` 1.8.7 feature documentation.

## Lint and dependency policy

Question: What mechanically enforces the rules in docs/standards.md, specifically no comments, no unsafe without a stated invariant, no unwrap outside enforced invariants, and no new dependency without review.

Options: review discipline alone; clippy lint table; clippy plus a custom checker; clippy plus `cargo-deny` plus a custom checker.

Chosen: a `[workspace.lints]` table inherited by every crate, plus `cargo deny check` with an exhaustive allow-list, plus one `xtask` checker for the rules no lint expresses. All four run in CI as gate jobs and all four fail the build.

Lints, in the workspace `Cargo.toml` and inherited with `lints.workspace = true`. The `[lints]` table has been stable since Cargo 1.74.

```toml
[workspace.lints.rust]
unsafe_code = "deny"
unsafe_op_in_unsafe_fn = "deny"
missing_docs = "deny"
unused_crate_dependencies = "deny"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "deny", priority = -1 }
undocumented_unsafe_blocks = "deny"
unnecessary_safety_comment = "deny"
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
dbg_macro = "deny"
allow_attributes = "deny"
allow_attributes_without_reason = "deny"
missing_errors_doc = "deny"
missing_panics_doc = "deny"
```

No unsafe without a stated invariant. `unsafe_code` is denied everywhere. Only the `platform` crate lifts it, and only with `#[expect(unsafe_code, reason = "...")]` on the specific item. Inside it, `clippy::undocumented_unsafe_blocks` requires a `// SAFETY:` comment on every `unsafe` block, which is the exactly one comment form standards.md permits, and `clippy::unnecessary_safety_comment` rejects a `SAFETY:` line attached to safe code so the marker cannot be used decoratively. Both lints are in clippy's `restriction` group and are allow-by-default, so they must be named explicitly.

No unwrap outside enforced invariants. `unwrap_used` and `expect_used` are denied. An exception is written as `#[expect(clippy::unwrap_used, reason = "<the invariant, and what enforces it>")]` on the enclosing item. `allow_attributes` forces `#[expect]` over `#[allow]`, so a stale exception becomes a compile error once the exception is no longer needed instead of surviving forever. `allow_attributes_without_reason` makes the stated invariant mandatory. Test modules carry a single `#![expect(clippy::unwrap_used, reason = "test setup, failure is the assertion")]`.

No new dependency without review. `deny.toml` uses `[bans] allow = [...]` listing every crate permitted in the graph, direct and transitive, each with a reason. The cargo-deny documentation states that "if the `allow` list has one or more entries, then any crate not in that list will be denied". A new dependency, or a version bump that changes the transitive graph, fails `cargo deny check` until the list is edited, which makes the review a diff a human must approve. `multiple-versions = "deny"` and `wildcards = "deny"` are also set, and the `advisories`, `licenses` and `sources` checks run in the same invocation. `unused_crate_dependencies` catches the opposite direction, a dependency declared and no longer used.

No comments. No lint expresses this. `cargo xtask check-comments` scans every `.rs` file in the workspace with a small state machine that tracks string, char and raw-string literals so a `//` inside a literal is not a false positive, and rejects every comment except `///`, `//!` and a `// SAFETY:` line immediately preceding an `unsafe` block. `/* */` in any form is rejected. It also rejects the decorative forms standards.md names: banners, section dividers and emoji, anywhere in `.rs` and `.md`.

Budget enforcement. `cargo xtask bench --compare baseline` fails on a regression above five percent on any regime, and the no-op regime plus the recorded binary size is what mechanically bounds the cost of a dependency existing, which is the rule standards.md states but cannot express as a lint.

Because: every rule in standards.md that a tool can check is checked by a tool, and the ones no tool checks are named as such rather than left to reviewer memory. `#[expect]` with a mandatory reason is the mechanism that turns each exception into a written invariant that expires on its own, which is what the unwrap and unsafe rules actually ask for. The cargo-deny allow-list is the only mechanism found that makes an unreviewed transitive dependency fail a build rather than merely appear in a lockfile diff.

Costs: an exhaustive `[bans] allow` list must be edited on every dependency version bump that changes the graph, including bumps that change nothing else. That is deliberate friction and it will be felt on routine `cargo update` runs. `clippy::pedantic` denied at the workspace level will produce exceptions that need reasons, and each one is a small argument. `missing_docs` denied means no item can be added without a docstring, including during exploratory work. The comment checker is code written and maintained here, and a hand-written scanner over Rust source is a place bugs can hide; it needs its own tests over adversarial inputs, including raw strings containing `//` and doc comments containing `/*`.

Uncertain: whether `clippy::pedantic` at `deny` produces a tolerable number of exceptions on this codebase or a stream of noise. If it is noise, the answer is to demote it to `warn` in CI with a separate gate rather than to sprinkle allows. That call is made in slice 0.2 against real code, not now.

Sources: Cargo manifest reference, the `[lints]` section; `rust-lang/rust-clippy` `undocumented_unsafe_blocks.rs`, both lints in `restriction`; clippy `allow_attributes_without_reason` documentation; cargo-deny bans configuration reference.

## Outboard tree: chunk group, encoding, threshold, verification walk

Question: What chunk group size does the outboard tree use, what bytes does Fetchloom write for it, above what object size is one stored, and what is the shape of the range verification walk.

Options: chunk group of 1 chunk (1 KiB, bao's own shape), 16 chunks (16 KiB), 1024 chunks (1 MiB); bao's combined encoding, bao's outboard encoding, or a self-describing encoding of our own; the 64 MiB threshold already fixed in contracts.md Limits.

Chosen:

Chunk group is 1024 BLAKE3 chunks, `GROUP_LEN = 1_048_576` bytes. The outboard leaf is a group, not a chunk.

Encoding of `outboard/<content-digest>`:

```
offset 0   u64 little-endian   total object length in bytes
offset 8   parent nodes, 64 bytes each, in pre-order
```

A parent node is `left_cv` (32 bytes) followed by `right_cv` (32 bytes), each a `blake3::hazmat::ChainingValue`. Pre-order is parent, then the whole left subtree, then the whole right subtree. There is no magic number and no header beyond the length; the file's identity is its path plus the cache format fingerprint, and a second identity mechanism is what standards.md forbids.

Leaf count `L = ceil(len / GROUP_LEN)`. Parent count `L - 1`. File size `8 + 64 * (L - 1)`. At the 64 MiB threshold that is 4,032 bytes of parents; at 1 TiB it is 64 MiB, an overhead of 1/16384.

Threshold is 64 MiB, already fixed by contracts.md Limits. Below it no outboard is written and verification is a full rehash, which at the 6.9 GiB/s figure in the toolchain record is about 9 ms for a 64 MiB object.

Tree shape. Split a range recursively with `hazmat::left_subtree_len`, stopping when the range is at most `GROUP_LEN`. Leaf chaining value for group `g`:

```
let mut h = blake3::Hasher::new();
h.set_input_offset(g as u64 * GROUP_LEN);
h.update(group_bytes);
let cv = h.finalize_non_root();
```

Parent chaining value is `hazmat::merge_subtrees_non_root(&left, &right, Mode::Hash)`. Root is `hazmat::merge_subtrees_root(&left, &right, Mode::Hash)`.

Range verification walk for byte range `[start, end)`:

1. Read the 8-byte length. If it disagrees with the object's recorded size, the outboard is `cache.corrupt`: discard it and rebuild by a full rehash. This is not an integrity failure, because nothing is authenticated yet.
2. Read the root parent node at offset 8 and check `merge_subtrees_root(left, right, Hash) == content_digest`. This authenticates the two top chaining values.
3. Descend toward each group in the range. At each step the expected chaining value comes from a parent already authenticated; read that node's 64 bytes, split into `(l, r)`, and check `merge_subtrees_non_root(l, r, Hash)` against the expectation. Pre-order plus known subtree sizes gives each node's offset arithmetically, so one group's path is `O(log L)` reads of 64 bytes.
4. At the leaf, hash the group's bytes as above and compare against the chaining value its parent named. A mismatch is `integrity.range_mismatch` naming the group's byte range. Resume ladder rung 1 restarts from the start of that group, so one corrupt group costs at most 1 MiB of refetch.

Because:

The only free parameter is the group size; the threshold is already in contracts.md and the encoding follows from the group size. Group size trades outboard size against refetch granularity. At 16 KiB the overhead is 1/256, which is 4 GiB of derived data for the 1 TiB object contracts.md Limits permits. At 1 MiB it is 1/16384, 64 MiB for the same object, and the refetch penalty for one bad group is 1 MiB, a single efficient ranged request rather than a saving worth 4 GiB of disk. Verification read cost does not change with group size, because rung 1 reads the whole existing prefix either way.

Splitting with `left_subtree_len` never splits inside a group. It is documented to return "the largest power-of-two number of bytes that's strictly less than `input_len`", so for any `n > GROUP_LEN` the result is a power of two at least `GROUP_LEN`, hence a multiple of `GROUP_LEN`, because `GROUP_LEN` is itself a power of two.

`set_input_offset` is documented to panic unless the offset is a multiple of `CHUNK_LEN` and the subtree fits within `max_subtree_len(offset)`. Both hold by construction: `g * GROUP_LEN` is a multiple of 1024, and `max_subtree_len` is documented as "for a subtree starting at a 0-based chunk index N greater than zero, the maximum number of chunks in that subtree is the largest power-of-two that divides N", where `N = g * 1024`, so the bound is never below 1024 chunks.

The encoding is bao's outboard encoding with the leaf enlarged to a group: an 8-byte little-endian length "prepended to the very front", then nodes "concatenated together in pre-order (that is a parent, followed by its left subtree, followed by its right subtree)", which bao chose so that "a decoder reading the tree from beginning to end doesn't need to do any seeking". Keeping the length in the file makes the outboard self-describing without making it authoritative, because a wrong length produces a wrong root.

Checking any tree against `blake3::hash` of the same bytes, by case:

One chunk, `len <= 1024`. No outboard exists, because 1024 is far below the 64 MiB threshold. The root is `blake3::hash(bytes)` with no hazmat involved. `len == 0` is this case, and the root is `blake3::hash(b"")`.

One group, `1024 < len <= 1 MiB`. Also no outboard. `L == 1` means zero parents, and `finalize_non_root` would be wrong to use at all. A stored outboard therefore always has `L >= 65` and at least 64 parents, and the builder asserts it.

Exact power of two, `len == 2^k` for `k >= 26`. `left_subtree_len` returns `2^(k-1)`, so every split is even and the tree is perfectly balanced with `2^(k-20)` leaves.

Final partial chunk, for example `64 MiB + 1`, `64 MiB + 1023`, `100 MiB + 7`. The last group is short and its last chunk is short. Nothing special happens in our code: the leaf hasher receives fewer bytes, and `blake3` applies the chunk-end handling itself.

The conformance test builds the outboard for each of those lengths plus a multiple of `GROUP_LEN` with no partial group, walks it to the root, and asserts equality with `blake3::hash`. The adversarial test flips one byte inside a group and asserts `integrity.range_mismatch` naming that group's range, and flips one byte inside a parent node and asserts the failure is reported at that node rather than at a leaf.

Costs: 1 MiB granularity means one flipped byte forces a 1 MiB refetch, sixty-four times more than a 16 KiB group would. The encoding is ours, so every shape must be proven against `blake3::hash` in tests rather than against a second implementation. Choosing the same byte layout as bao leaves a differential test against `bao-tree` possible later, but adopting it as a dependency is not part of this decision.

Uncertain: nothing here is measured. The 1/16384 and 1/256 overheads are arithmetic. The 9 ms full-rehash figure derives from the unmeasured 6.9 GiB/s throughput cited in the toolchain record and is replaced by the slice 0.2 harness.

Sources: docs.rs `blake3` 1.8.7 `hazmat` module, `left_subtree_len`, `max_subtree_len`, `HasherExt`, `Mode`, `CHUNK_LEN`; `oconnor663/bao` `docs/spec.md`; docs.rs `bao-tree` `BlockSize`; contracts.md Limits and Resume ladder.

## Canonical entry stream for the tree digest

Question: What exact bytes are hashed to produce the tree digest, in what field order, with what length framing and what domain separation, and how are symlinks, empty directories, zero-byte files and mode reduction encoded. How is a case-fold or normalization collision on the target filesystem detected before anything is published.

Options: for separation, an ASCII prefix inside the stream versus BLAKE3 `derive_key` mode; for framing, NUL termination, newline separation, varint lengths, or fixed 64-bit lengths; for collisions, an in-process Unicode case-fold and normalization table versus letting the target filesystem decide by exclusive create.

Chosen:

Domain separation is BLAKE3 `derive_key` mode. The tree digest is `Hasher::new_derive_key("fetchloom tree digest")` updated with the stream and finalized. The manifest digest uses the context `"fetchloom manifest digest"`. The content digest stays plain `blake3::hash`, because it is the cache key, the interop-facing address, and the root the outboard tree merges to.

Entries are sorted ascending by raw path bytes compared as unsigned bytes. Paths are relative to the destination root, use `/` as the separator, and have no leading separator, no trailing separator, no empty component, and no `.` or `..` component. No normalization of any kind is applied. The destination root itself is not an entry. Every directory is an entry, not only an empty one.

Three field lists, chosen by the type tag:

```
file       0x01 | path_len:u64le | path | mode:u32le | size:u64le | content:32
directory  0x02 | path_len:u64le | path
symlink    0x03 | path_len:u64le | path | size:u64le | content:32
```

`mode` is exactly `0o644` or `0o755`. Any other mode is reduced before encoding: `0o755` if the source entry carries any execute bit, `0o644` otherwise. The mode comes from the archive entry or from the source file on a Unix filesystem, never from a stat of the destination.

For a symlink, `size` is the byte length of the target and `content` is `blake3::hash` of the target bytes, as contracts.md Materialization states. The target bytes themselves are not in the stream.

Degenerate cases carry no special case in the code. A zero-byte file is a file entry with `size = 0` and `content = blake3::hash(b"")`. An empty directory is a directory entry, identical in form to a non-empty one. Two entries with identical content produce identical `content` fields and differ only in `path`.

Collision detection. Every entry is created in staging with exclusive create: `openat` with `O_CREAT|O_EXCL` and `mkdirat` on Unix, `CreateFileW` with `CREATE_NEW` and `CreateDirectoryW` on Windows. If the raw path bytes are new to the plan but creation reports the name already exists, the target filesystem folded the name onto one already created, and that is `archive.collision`. The partner is found by comparing the 128-bit file identity of the existing entry against the identities of the entries already created in that directory, so both raw names are printed. This runs entirely in staging; nothing is published.

Because:

`derive_key` is the mechanism the BLAKE3 specification provides for keeping one application's hashes structurally incapable of colliding with another's, and contracts.md Compatibility already promises that "digest and tree algorithms are domain-separated and named at their point of use". A prefix inside the stream would leave a tree digest and a content digest in the same domain, so an attacker who controls file bytes could aim a file's content digest at a tree digest. The content digest cannot use `derive_key`, because `hazmat::Mode::Hash` is what the outboard merges under and because the content digest is the value published and compared against outside Fetchloom.

Fixed 64-bit little-endian length framing rather than a terminator removes any question of escaping a separator that can legally appear in a path, and removes varint ambiguity where two encodings of one number would give two digests for one tree. The cost is eight bytes per length field, which at the one-million-entry limit is eight megabytes of extra hashing, below a millisecond at BLAKE3 throughput.

Type-determined field lists rather than one fixed row per entry, because a directory has no byte length and no content, and a symlink has no meaningful mode: POSIX permission bits on a symlink are ignored on Linux, and Windows has none. Emitting a constant in those positions would put a field in the digest that carries no information, and emitting a real value would make the digest depend on something a platform cannot reproduce.

Mode is the one field that does not survive a round trip through Windows. Windows has no execute bit, so a tree materialized on Linux, transported, materialized on Windows and re-stated would lose it. The tree digest is therefore computed over the materialization plan's entries, whose modes come from the source, not over a re-stat of the destination. On Windows the mode is recorded and not applied, and `reconcile` there cannot detect a mode change. That is a capability the platform lacks, reported and never assumed, and it is the only way contracts.md Determinism, "the same lock produces the same tree digest on every platform", can hold.

Collision detection by exclusive create is exact by construction, because the target filesystem's own folding rules decide, and those rules differ by more than a boolean. APFS is normalization-insensitive and normalization-preserving in both its case-sensitive and case-insensitive variants, using hashes of the normalized form; HFS+ stores the normalized form instead; NTFS folds by case using an upcase table and does not normalize; ext4 folds only in a directory carrying `FS_CASEFOLD_FL`. Reimplementing that set inside Fetchloom means a Unicode case-folding table and NFC and NFD implementations, which is a new dependency and a second answer that can disagree with the filesystem's. Letting the filesystem answer is one mechanism, needs no tables, and is already the call we have to make in order to create the entry.

Costs: a tree digest cannot be checked with a stock `b3sum`, because `derive_key` mode is not what `b3sum` computes by default, so Fetchloom must be the tool that verifies a tree digest and `verify` has to be worth trusting. Collisions are found during staging rather than during planning, so `plan` cannot predict one without doing the extraction work; the plan reports the destination's measured folding behavior instead. Exclusive create means every entry costs one create call that cannot be batched away.

Uncertain: nothing here is measured. The claim that eight bytes per field is below a millisecond at one million entries follows from the unmeasured throughput figure in the toolchain record.

Sources: docs.rs `blake3` 1.8.7 `hazmat::Mode` and `hash_derive_key_context`; Apple File System Guide FAQ on APFS normalization insensitivity and preservation versus HFS+; Microsoft Learn `FILE_CASE_SENSITIVE_INFORMATION` and the WSL case sensitivity documentation; `include/uapi/linux/fs.h` `FS_CASEFOLD_FL` 0x40000000; contracts.md Materialization, Identity and Determinism.

## Atomic publication and durability tiers

Question: What is the exact call sequence per platform for publishing a file and for each of the three durability tiers, including directory durability, macOS `F_FULLFSYNC`, and the Windows choice between `ReplaceFile` and `MoveFileEx`. How is a cross-volume publish detected and refused rather than silently degraded to a copy.

Options: Windows `ReplaceFileW` versus `MoveFileExW` versus `SetFileInformationByHandle` with `FileRenameInfoEx` and POSIX semantics; cross-volume detection by errno versus by comparing volume identity before the call; directory durability on Windows by `FlushFileBuffers` on a directory handle versus `MOVEFILE_WRITE_THROUGH`.

Chosen:

Publishing one object, all platforms, in order: create in `partial/` with exclusive create; preallocate the full length; write forward at explicit offsets; flush per tier; rename into `objects/<digest>`; make the containing directory durable per tier.

Unix rename is `rustix::fs::renameat(partial_dirfd, name, objects_dirfd, digest)`. Windows rename is `MoveFileExW(partial, target, MOVEFILE_REPLACE_EXISTING)`, plus `MOVEFILE_WRITE_THROUGH` under the `strict` and `normal` tiers. `MOVEFILE_COPY_ALLOWED` is never passed.

Cross-volume publishes are refused by comparing volume identity before the call, not by interpreting a failure afterwards. The volume identifier is `st_dev` on Unix and `FILE_ID_INFO.VolumeSerialNumber` on Windows. The cache checks `objects/`, `partial/`, `staging/` and `locks/` against each other when it opens, before any transfer starts, so a cache spanning a mount point fails once with one error rather than at every publish. A destination's staging directory is created on the destination volume, so the destination publish is same-volume by construction; if staging cannot be created there, that is a hard failure and not a copy.

Durability tiers, applied at object granularity and never per entry:

| Tier | Object bytes | Containing directory |
|---|---|---|
| `strict` | Linux `rustix::fs::fsync`; macOS `File::sync_all`, which std implements as `fcntl(fd, F_FULLFSYNC)`; Windows `FlushFileBuffers` | Linux and macOS `fsync` on a directory descriptor after the rename; Windows `MOVEFILE_WRITE_THROUGH` |
| `normal` | Linux `rustix::fs::fdatasync`; macOS `fcntl(fd, F_BARRIERFSYNC)`; Windows `FlushFileBuffers` | same as `strict` |
| `fast` | none | none |

If a macOS volume does not advertise `VOL_CAP_INT_BARRIERFSYNC` (0x01000000), `normal` falls back to `File::sync_all` and emits `degrade` naming barrier fsync as requested, full fsync as used, and the volume capability as the reason.

Publishing a directory tree. If the destination does not exist, rename staging onto it. If it exists, rename the destination aside to a sibling `<destination>.fetchloom-old-<random>`, rename staging onto the destination, then remove the sibling. The destination is briefly absent between the two renames, and the previous tree is recoverable from the sibling until it is removed. It is never partially materialized.

Because:

`ReplaceFileW` is the wrong primitive twice over. It requires the replaced file to already exist, so it cannot perform a first publish, which would force a second code path. It also merges the replaced file's state into the replacement: Microsoft documents that it "preserves the following attributes of the original file: Creation time, Short file name, Object identifier, DACLs, Security resource attributes, Encryption, Compression, Named streams not already in the replacement file". contracts.md Materialization excludes ACLs, extended attributes and alternate data streams and calls their presence an error, so inheriting them from whatever was there before is the opposite of the contract. Its own write-through flag is documented as "This value is not supported."

`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` does one thing and covers both the first publish and the replacement. Omitting `MOVEFILE_COPY_ALLOWED` is what makes a cross-volume attempt fail instead of becoming a copy: the flag is documented as "if the file is to be moved to a different volume, the function simulates the move by using the CopyFile and DeleteFile functions", and contracts.md states a cross-volume rename "is an error, never a copy". `SetFileInformationByHandle` with `FileRenameInfoEx` and POSIX semantics is what Rust's standard library falls back to only on `ERROR_ACCESS_DENIED`; it solves the read-only-attribute case, which Fetchloom never creates, so adopting it would be a second way to rename.

Detecting cross-volume by comparing volume identity rather than by errno gives one mechanism on all three platforms and avoids depending on an error code the Microsoft page for `MoveFileEx` does not enumerate. `FILE_ID_INFO` is documented as "the file identifier and the volume serial number uniquely identify a file on a single computer", which is exactly the comparison needed, and `st_dev` is its Unix equivalent.

macOS needs no unsafe code for `strict`. Rust's standard library implements `File::sync_all` on Apple targets as `libc::fcntl(fd, libc::F_FULLFSYNC)`, and Apple documents why that is required: "while fsync() will flush all data from the host to the drive, the drive itself may not physically write the data to the platters for quite some time and it may be written in an out-of-order sequence", and "this is not a theoretical edge case". The standard library also maps `sync_data` to `F_FULLFSYNC` on Apple, so it cannot express a weaker tier, which is why `normal` reaches for `F_BARRIERFSYNC` directly. The tiers then mean the same thing everywhere: `strict` is the device acknowledging the write, `normal` is the operating system holding the data with write ordering preserved, `fast` is neither.

Windows has no documented way to fsync a directory. `FlushFileBuffers` is documented to require "the GENERIC_WRITE access right", which a directory handle cannot be opened with, and its only documented directory-adjacent behavior is flushing a whole volume, for which "the caller must have administrative privileges". `MOVEFILE_WRITE_THROUGH` is documented as "the function does not return until the file is actually moved on the disk", which is precisely the rename-durability guarantee a Unix directory fsync provides, so it is the Windows answer for that row.

An atomic directory swap is not portable. Linux has `RENAME_EXCHANGE`, macOS has `renameatx_np` with `RENAME_SWAP`, Windows has neither, so a swap would be three implementations of one operation and still not available everywhere. Rename-aside is one sequence on all three, and its failure mode is a recoverable sibling directory rather than a half-written tree.

Costs: `fast` performs no flush at all, which reads against the literal wording of the Cache invariant in contracts.md; the invariant has to be read as "the tier's flush". Rename-aside leaves a window in which the destination does not exist, and leaves a sibling directory behind if the process is killed between the second rename and the removal, so startup recovery has to sweep those. Comparing volume identity costs one extra handle and one extra call per publish target, though the cache pays it once at open.

Uncertain: `F_BARRIERFSYNC`'s cost relative to `F_FULLFSYNC` on APFS is not measured here and belongs to the slice 0.2 harness. Whether `MOVEFILE_WRITE_THROUGH` has any measurable cost on a same-volume rename is also unmeasured; if it is free, the `fast` tier's directory row is a distinction without a difference on Windows and should be said so after measurement.

Sources: Microsoft Learn `ReplaceFileW`, `MoveFileExW`, `FlushFileBuffers`, `FILE_ID_INFO`; Apple `fsync(2)` manual page; `rust-lang/rust` `library/std/src/sys/fs/unix.rs` and `windows.rs`; docs.rs `rustix` 1.1.4 `fs` module index; `xnu` `bsd/sys/attr.h` for `VOL_CAP_INT_BARRIERFSYNC`; contracts.md Cache, Materialization and Flags.

## Capability detection per platform

Question: How is each capability in contracts.md Platform capabilities probed on each platform without side effects and without writing to the user's destination, and what is the 128-bit file identity on each platform.

Options: infer capabilities from the filesystem name; query the platform's capability APIs; probe empirically. In practice each capability admits only some of the three.

Chosen: a query where the platform answers directly, and an empirical probe where it does not. Every empirical probe runs in a Fetchloom-owned directory on the volume being measured, which is the cache's `staging/` for a cache volume and the destination-side staging directory for a destination volume, and every probe file is removed. Nothing is ever written into the user's tree. Results are keyed by volume identity and held for the process lifetime.

| Capability | Windows | macOS | Linux |
|---|---|---|---|
| Case folding | probe | probe | probe |
| Normalization | probe | probe | probe |
| Clone | `FILE_SUPPORTS_BLOCK_REFCOUNTING` 0x08000000 | `VOL_CAP_INT_CLONE` 0x00010000 | probe: `ioctl_ficlone` on a 4 KiB file |
| Sparse | `FILE_SUPPORTS_SPARSE_FILES` 0x40 | `VOL_CAP_FMT_SPARSE_FILES` 0x40 | probe: `fallocate` punch hole |
| Symlink | probe | probe | probe |
| Hard link | `FILE_SUPPORTS_HARD_LINKS` 0x400000 | `VOL_CAP_FMT_HARDLINKS` 0x4 | probe: `linkat` |
| Max component | `lpMaximumComponentLength` | `pathconf(_PC_NAME_MAX)` | `statfs.f_namelen` |
| Max path | `\\?\` prefix, 32,767 wide characters | `PATH_MAX` 1024 | `PATH_MAX` 4096 |
| Network-backed | `\\?\UNC\` prefix, else `GetDriveTypeW == DRIVE_REMOTE` | `statfs.f_flags & MNT_LOCAL` clear | `statfs.f_type` in the network magic set |
| On-access scanner | measure, and name via `FilterFindFirst` | measure | measure |

The Windows flags come from `GetVolumeInformationByHandleW` on a handle to the volume's root; the macOS bits from `getattrlist` with `ATTR_VOL_CAPABILITIES`, each gated on the matching bit in the `valid` array before the bit in `capabilities` is believed. The Linux network magic set is NFS 0x6969, SMB 0x517b, SMB2 0xfe534d42, CIFS 0xff534d42, AFS 0x5346414f, 9P 0x01021997 and OCFS2 0x7461636f. FUSE 0x65735546 is reported as unknown backing rather than as network, because a FUSE mount may be either.

Case folding and normalization are one probe with two questions. In staging, create `fetchloom-probe-A` with exclusive create, then attempt `fetchloom-probe-a` with exclusive create; an already-exists result means the volume folds case. Then create a name containing U+00E9 in NFC (`c3 a9`) with exclusive create and attempt the same name in NFD (`65 cc 81`); an already-exists result means the volume folds normalization. Read the directory back and compare the returned bytes against the bytes written to learn whether the volume preserves the normalization it was given or stores a normalized form. The three reported outcomes are normalization-sensitive, normalization-insensitive and preserving, and normalizing.

Symlink support is one probe on all three platforms: create a symlink in staging and remove it. On Windows this covers the whole question at once, because it fails identically whether the process token lacks `SeCreateSymbolicLinkPrivilege`, Developer Mode is off so `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE` does nothing, or the volume lacks reparse points.

Hardware digest acceleration. SHA-256 acceleration is `is_x86_feature_detected!("sha")` on x86 and x86_64 and `is_aarch64_feature_detected!("sha2")` on aarch64. BLAKE3 has no dedicated instruction, so what is reported for it is the vector instruction level selected at runtime, named as such and never called hardware acceleration. On `aarch64-pc-windows-msvc` the CPU extension may be present while `sha2` still uses its soft backend, because `cpufeatures` documents aarch64 detection as Linux, iOS and macOS only; that combination is reported and emits `degrade` naming the extension as available, the soft backend as used, and the detection gap as the reason.

128-bit file identity, and the volume identifier that accompanies it in the fingerprint tuple:

| Platform | File identity, 128 bits | Volume identifier, 64 bits |
|---|---|---|
| Linux | `statx` `stx_dev_major` and `stx_dev_minor` combined as a `dev_t` in the high half, `stx_ino` in the low half | the same `dev_t` |
| macOS | `fstat` `st_dev` widened in the high half, `st_ino` in the low half | `st_dev` widened |
| Windows | `GetFileInformationByHandleEx` with `FileIdInfo`, the 16-byte `FILE_ID_INFO.FileId` verbatim | `FILE_ID_INFO.VolumeSerialNumber` |

The legacy 64-bit `nFileIndex` from `BY_HANDLE_FILE_INFORMATION` is not used, because it is not unique on ReFS. `FILE_ID_INFO` is available from Windows 8 and Windows Server 2012, below the floor Fetchloom targets.

Because:

Clone support cannot be inferred from a filesystem name on Linux. XFS supports reflink only when formatted with it enabled, Btrfs supports it unless the file carries `FS_NOCOW_FL`, NFS 4.2 supports it over the wire, and overlayfs depends on its lower layer. There is no query, so the only honest answer is an attempt, and an attempt in Fetchloom's own staging directory has no side effect the user can observe. macOS and Windows both publish a direct bit, and Microsoft's is unambiguous: `FILE_SUPPORTS_BLOCK_REFCOUNTING` "indicates that FSCTL_DUPLICATE_EXTENTS_TO_FILE is a supported operation."

Case folding and normalization behave the same way. Windows exposes `FILE_CASE_SENSITIVE_SEARCH` at the volume level, but NTFS reports it clear while individual directories can carry `FILE_CS_FLAG_CASE_SENSITIVE_DIR` since Windows 10 version 1803, so the volume answer is wrong for the directory that matters. Linux exposes `FS_CASEFOLD_FL` per inode on ext4 and f2fs and nothing at all for a FUSE or network mount that folds. Normalization has no query on any of the three. One probe answers all of it, on the exact directory being written to, using the filesystem's own rules, and it is the same exclusive-create mechanism the collision check already uses.

The on-access scanner is required by contracts.md to be reported with "the measured cost", so a measurement has to happen regardless of what any enumeration API says. Writing many small files in staging and comparing against writing the same bytes to one file is that measurement, and it is the same shape as the many-small-files benchmark regime. `FilterFindFirst` adds only the product's name, which `doctor` needs in order to say which exclusion the user may configure; Microsoft's altitude allocation puts anti-virus minifilters in 320000 to 329998 and activity monitors, where endpoint detection products sit, in 360000 to 389999. The call has no documented privilege requirement and no side effects.

Costs: the empirical probes create and delete a handful of files per volume per process, which the no-op benchmark regime has to absorb, so they must be lazy and a command that never touches a volume must never probe it. A probe answers for the staging directory, and on Windows per-directory case sensitivity means the destination directory could in principle differ; the collision check catches that anyway, because it runs on the real paths. Probing means a plan produced without staging cannot report folding behavior, so `--offline` plans mark it unknown.

Uncertain, and thin enough to say so rather than choose:

The macOS `statfs` `MNT_LOCAL` bit and the Windows `GetDriveTypeW` `DRIVE_REMOTE` value were not verified against primary documentation in this session. Both are widely used for this purpose, but the network-backed row for those two platforms should be confirmed before it is implemented.

Whether `is_aarch64_feature_detected!` returns a useful answer on `aarch64-pc-windows-msvc` was not verified; `std_detect`'s aarch64 backend has historically been Linux and Apple only. If it does not, that row reports the extension as undetectable rather than as present and unused.

The threshold ratio at which a scanner is called present is not chosen here. It comes from the slice 0.2 harness measuring one machine with the scanner enabled and disabled.

Sources: Microsoft Learn `GetVolumeInformationByHandleW`, `FILE_ID_INFO`, Block Cloning, `FilterFindFirst`, `CreateSymbolicLinkW`, allocated filter altitudes, `FILE_CASE_SENSITIVE_INFORMATION`; `xnu` `bsd/sys/attr.h`; Apple `getattrlist(2)`; `statfs(2)` manual page; `include/uapi/linux/fs.h`; docs.rs `cpufeatures` 0.3.1; `std::arch` feature detection macros.

## Advisory locking, liveness, and which filesystems are refused

Question: What primitive holds the single-writer lock on each platform, what is in the liveness token and where does each field come from, how is a stale lock distinguished from a live one, how does it behave across users in a shared cache, and which filesystems cannot express it and must be refused.

Options: `flock` and `LockFileEx` through `rustix` and `windows-sys`; POSIX `fcntl` byte-range locks; open-file-description locks on Linux; `std::fs::File::lock`, stabilized in Rust 1.89; a lock directory created with exclusive create.

Chosen: `std::fs::File::try_lock`, `lock` and `unlock` from the standard library, which uses `flock` on Unix and `LockFileEx` on Windows. One primitive, one code path, no unsafe, no platform module. This raises the workspace MSRV floor from 1.85 to 1.89.

Two files per digest under `<cache>/locks/`:

`<digest>.lock` is zero length and is the only file ever locked. It is never read and never written.

`<digest>.owner` holds the liveness token as canonical JSON. It is never locked, is readable by every user of the cache, and is rewritten by the holder immediately after the lock is acquired.

Two files because Windows locks are mandatory. Microsoft documents that "locking a portion of a file for exclusive access denies all other processes both read and write access to the specified region", and the standard library locks the whole file, so a waiter could not read a token stored inside it.

The token's fields and their sources:

| Field | Linux | macOS | Windows |
|---|---|---|---|
| `machine` | `/etc/machine-id` | `IOPlatformUUID` | `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid` |
| `boot` | `/proc/sys/kernel/random/boot_id` | `sysctl` `KERN_BOOTTIME` | boot instant derived from uptime |
| `pid` | `getpid` | `getpid` | `GetCurrentProcessId` |
| `start` | field 22 of `/proc/<pid>/stat` | `sysctl` `KERN_PROC_PID`, `kp_proc.p_starttime` | `GetProcessTimes` creation time |

Liveness is decided in this order, and never from a file modification time:

1. `machine` differs from ours. The holder is another machine, which can only happen on a shared or network cache. Not stale, and not inspectable. Wait, then fail with `cache.locked` naming the machine.
2. `boot` differs. The record is from a previous boot of this machine. Stale.
3. No process with that `pid` exists. Stale.
4. A process with that `pid` exists but its start time differs. The pid was recycled. Stale.
5. Otherwise live.
6. Any field that cannot be read makes the holder live. A lock is never stolen on missing evidence, and the missing field is reported.

The lock, not the token, is the authority. `try_lock` succeeding proves there is no live holder, because both `flock` and `LockFileEx` are released by the kernel when the last descriptor closes or the process dies. The token exists so a waiter can say who holds the lock, and so recovery can reason about a volume where the lock itself is not trustworthy. Lock files are never deleted by a waiter, because deleting a file another process holds open races on every platform; they are zero length, and `cache prune` removes one only while holding its lock.

Cross-user behavior, which is the only behavior. On Unix `locks/` is mode 0777 with the sticky bit, and the two lock files are created 0666 with the umask cleared for them specifically, so any user of the cache can take the lock while the sticky bit stops one user removing another's file. `<digest>.lock` is opened read-write, because `flock(2)` documents that over NFS "in order to place an exclusive lock, the file must be opened for writing". On Windows `locks/` inherits its parent's access control entries and Fetchloom sets none of its own, so a lock file is reachable by exactly the users the directory's creator allowed, and the handle is opened with `GENERIC_READ` and `GENERIC_WRITE`, which `LockFileEx` requires.

Filesystems refused, because a cache on one cannot be shared safely and sharing is not something a user opts into:

Any volume detected as network-backed. `flock(2)` documents that "up to Linux 2.6.11, flock() does not lock files over NFS", and that since 2.6.12 the kernel emulates it with whole-file `fcntl` byte-range locks. Mounting with `-o nolock` or a `local_lock=` option makes those locks node-local and returns no error, so a silent single-writer violation cannot be detected from a return code. contracts.md already requires refusing rather than using unsafely.

Any volume where a probe `try_lock` on a scratch file fails with `ENOLCK`, `EOPNOTSUPP`, `ENOSYS` or `EINVAL`, which is how a FUSE filesystem without lock support answers.

There is no exception for a cache only one machine writes to, because nothing declares that and the next process to open the directory may be on another host. A cache on a network-backed volume is refused with `cache.locking_unsupported`, and the run continues in no-cache behavior with a `degrade` event naming the volume, which is what contracts.md requires of a cache that cannot be used.

Because: the standard library reached this exact primitive in 1.89 and documents the mapping to `flock` and `LockFileEx` explicitly, which removes an entire platform module and every unsafe block that would have been in it. `fcntl` byte-range locks are the wrong shape twice: they are per-process rather than per-descriptor, so a second open in the same process silently releases them, and they are what NFS emulation is built on rather than an improvement over it. Open-file-description locks fix the first problem but exist only on Linux. A lock directory created with exclusive create needs no filesystem lock support at all, but it has no liveness at all either: a killed process leaves a directory that nothing can prove is dead, which is precisely what contracts.md forbids deciding by modification time.

The pair of pid and process start time is what makes pid reuse detectable, and boot identity is what makes a whole generation of records recognizable at once after a crash, which is what contracts.md startup recovery of orphaned `partial/` and `staging/` entries depends on. Machine identity is not in contracts.md and is added here, because the moment a cache directory is shared or network-mounted, a token whose pid refers to a different machine's process table would otherwise be read as live or stale by accident.

Costs: the MSRV floor moves to 1.89, and any future need for a shared lock plus a readable token in one file is foreclosed by the two-file layout. Lock files accumulate, one per digest ever written, until a prune removes them. A holder that is alive but wedged is indistinguishable from a holder that is working, so the only remedy is the wait timeout, not a liveness check. Refusing every network volume refuses some that would in fact lock correctly, because the mount options that break locking cannot be distinguished from the ones that do not.

Uncertain: the macOS `IOPlatformUUID` and `KERN_BOOTTIME` sources and the Windows `MachineGuid` registry path were not verified against primary documentation in this session. The Windows boot instant has no single obvious source, and the choice between deriving it from uptime and reading a performance counter is unsettled. All four are report-only inputs to the liveness ladder and must be confirmed before implementation. The error kind for refusing a volume that cannot express locking does not exist: `cache.locked` means contended, not incapable.

Sources: `std::fs::File` `lock`, `try_lock`, `lock_shared`, `try_lock_shared`, `unlock` and `TryLockError`, stable since 1.89; `flock(2)` manual page NOTES and ERRORS; Microsoft Learn `LockFileEx`; contracts.md Cache and Cache modes.

## The rustix gaps and the unsafe blocks that fill them

Question: What is the exact approach for `fcntl_fullfsync`, `clonefileat`, and every other call `rustix` 1.1.4 does not wrap, and what invariant does each unsafe block state.

Options: wait for `rustix` to add them; vendor a small binding crate; call through `libc` directly with one unsafe block each.

Chosen: `libc` directly, one unsafe block each, each carrying the single `SAFETY:` line standards.md permits. The `rustix` 1.1.4 `fs` module index confirms that `fsync`, `fdatasync`, `fallocate`, `ioctl_ficlone`, `copy_file_range`, `flock`, `fcntl_lock`, `renameat`, `renameat_with`, `statfs`, `fstatfs`, `statx`, `openat`, `mkdirat`, `linkat`, `symlinkat`, `readlinkat`, `unlinkat`, `ftruncate` and `fchmod` are present, and that `fcntl_fullfsync`, `clonefileat`, `fclonefileat`, `copyfile`, `fcntl_rdadvise`, `fcntl_nocache` and `ioctl_ficlonerange` are absent.

`F_FULLFSYNC` needs no gap filled at all. Rust's standard library implements `File::sync_all` on Apple targets as `libc::fcntl(fd, libc::F_FULLFSYNC)`, so the `strict` tier on macOS is `File::sync_all()` and contains no unsafe code of ours.

`ioctl_ficlonerange` needs no gap filled either. Fetchloom clones whole objects, and whole-file `ioctl_ficlone` is present.

The gaps that remain, all macOS:

`fcntl(fd, F_BARRIERFSYNC)`, for the `normal` durability tier, which the standard library cannot express because it maps `sync_data` to `F_FULLFSYNC` as well.
`// SAFETY: fd is a valid open descriptor borrowed from an owned File for the call, and F_BARRIERFSYNC takes no variadic argument.`

`clonefileat(src_dirfd, src, dst_dirfd, dst, flags)`, for copy-on-write materialization, with `CLONE_NOFOLLOW` and a destination that must not exist. The `libc` signature is `unsafe extern "C" fn clonefileat(c_int, *const c_char, c_int, *const c_char, u32) -> c_int`.
`// SAFETY: both directory descriptors are open and borrowed for the call, and both path pointers are NUL-terminated CStrs that outlive it.`

`getattrlist` with `ATTR_VOL_CAPABILITIES`, for capability detection.
`// SAFETY: attrlist is a fully initialized struct attrlist and buf is at least buf_len bytes, so the kernel cannot write past it.`

Parsing that call's reply, which is a leading `u32` length followed by a `vol_capabilities_attr_t` at an offset the kernel chose.
`// SAFETY: the kernel reported a reply length covering this offset plus the size of vol_capabilities_attr_t, and the read is unaligned.`

`fcntl(fd, F_PREALLOCATE, &fstore_t)`, for preallocation.
`// SAFETY: fd is a valid open descriptor borrowed for the call, and the fstore_t behind the pointer is fully initialized and outlives it.`

`pathconf(path, _PC_NAME_MAX)`, for the maximum component length.
`// SAFETY: path is a NUL-terminated CStr valid for the duration of the call.`

Windows is unsafe by construction, because `windows-sys` is a raw binding crate. The calls are `CreateFileW`, `MoveFileExW`, `FlushFileBuffers`, `GetVolumeInformationByHandleW`, `GetFileInformationByHandleEx` with `FileIdInfo` and `FileCaseSensitiveInfo`, `SetFileInformationByHandle` with `FileAllocationInfo` and `FileEndOfFileInfo`, `DeviceIoControl` with `FSCTL_DUPLICATE_EXTENTS_TO_FILE`, `CreateSymbolicLinkW`, `GetDriveTypeW`, `FilterFindFirst`, `FilterFindNext`, `FilterFindClose`, `GetCurrentProcessId` and `GetProcessTimes`. Each takes the same shape of invariant, stated per call site rather than once:
`// SAFETY: the handle is owned and open for the call, and the buffer is at least the size passed, so the kernel cannot write past it.`

Because: waiting for `rustix` blocks phase 0 on someone else's release. A vendored binding crate would be a seventh workspace crate that standards.md does not list and that exists only to hide six calls. `libc` is already in the graph, as the toolchain record states, and six unsafe blocks each with one invariant is the smallest honest surface. Each invariant is about pointer validity and buffer size, which is what the compiler cannot check and what a reviewer needs told.

Costs: six unsafe blocks that a future `rustix` release would make redundant, and they will not be removed automatically, because nothing fails when the safe wrapper appears. The Windows list is long enough that the platform crate's Windows module is mostly unsafe, and the `SAFETY:` line is the only comment standards.md allows anywhere, so it carries all the reviewable meaning.

Uncertain: the `fstore_t` field names and the `F_ALLOCATEALL` and `F_PEOFPOSMODE` constants were not verified against `libc`'s Apple module in this session, only the shape of the API. `F_BARRIERFSYNC`'s presence in current `libc` for Apple targets is supported by search results rather than by a fetched docs.rs page. Both must be confirmed against `libc` before the builder writes them.

Sources: docs.rs `rustix` 1.1.4 `fs` module index; docs.rs `libc` Apple targets `clonefileat`; `rust-lang/rust` `library/std/src/sys/fs/unix.rs`; `xnu` `bsd/sys/attr.h`; standards.md on unsafe and comments.

## Preallocation and sparse files, and why SetFileValidData is not used

Question: How is the full length preallocated on each platform, how are sparse files handled, and why is `SetFileValidData` excluded.

Options: `fallocate` versus `posix_fallocate` versus `ftruncate` on Linux; `F_PREALLOCATE` with or without `F_ALLOCATECONTIG` on macOS; `FileAllocationInfo` versus `SetEndOfFile` versus `SetFileValidData` on Windows.

Chosen:

Linux: `rustix::fs::fallocate(fd, FallocateFlags::empty(), 0, len)`, which both reserves blocks and sets the size. On `EOPNOTSUPP`, fall back to `ftruncate(len)` and emit `degrade` naming block reservation as requested, size-only as used, and the filesystem as the reason.

macOS: `fcntl(fd, F_PREALLOCATE, &fstore_t { fst_flags: F_ALLOCATEALL, fst_posmode: F_PEOFPOSMODE, fst_offset: 0, fst_length: len })`, then `ftruncate(len)`, because `F_PREALLOCATE` grows the allocation and not the file size. Gated on `VOL_CAP_INT_ALLOCATE` (0x40); when the volume does not advertise it, `ftruncate` alone plus `degrade`. `F_ALLOCATECONTIG` is not requested, because contiguity is not needed and asking for it makes the call fail on a fragmented volume.

Windows: `SetFileInformationByHandle` with `FileAllocationInfo` and `FILE_ALLOCATION_INFO { AllocationSize: len }`, then `FileEndOfFileInfo` with the same length. Microsoft documents that "the end-of-file (EOF) position for a file must always be less than or equal to the file allocation size", which is why the allocation is set first.

Sparse files are reported as a capability and never used in phase 0. A cache object is written densely forward from byte zero in a single pass, so no hole can exist. A partial transfer would develop holes only if ranges were written out of order, and resume ladder rung 1 resumes "from the first bad or missing chunk", which is sequential. On Windows a file is not sparse until `FSCTL_SET_SPARSE` is issued, and Fetchloom never issues it, so a Windows partial file is dense by construction. Sparse archive members are a phase 3 question, because the archive formats are not chosen yet.

`SetFileValidData` is not used, for three independent reasons, each of them sufficient.

It requires a privilege Fetchloom must never request. Microsoft states that "a caller must have the SE_MANAGE_VOLUME_NAME privilege enabled when opening a file initially", which an ordinary user account does not hold.

It exposes other users' deleted data. Microsoft states that "reading from the file will return whatever the allocated clusters contain, potentially content from other users", and describes the exact crash window Fetchloom would create: "if the system stops responding before the caller finishes writing up the ValidDataLength supplied in the call, then, on a reboot, such a nonprivileged user can open the file and read exposed content". Fetchloom does write every byte it allocates, but a killed transfer is a normal event here, not an exceptional one, and contracts.md requires that a killed process leave no invalid state.

Its documented restrictions exclude the volumes Fetchloom runs on: "the file cannot be a network file, or be compressed, sparse, or transacted."

Because: preallocating the full length before writing is a standards.md rule, and the reason is fragmentation and the cost of extending a file per write, not a durability property. `fallocate` is chosen over `posix_fallocate` because the latter's glibc fallback writes zeros through userspace when the syscall is unavailable, which would double the write volume silently, and a silent fallback is what standards.md calls a defect of the same severity as data corruption. `ftruncate` alone sets the size without reserving anything, which is why it is the degraded path and emits an event rather than being the default.

Costs: three implementations of one operation, one per platform, none of which can be shared. The macOS path costs two syscalls where the others cost one. `FileAllocationInfo` reserves clusters that a failed transfer leaves reserved until the partial file is removed, so startup recovery matters for disk accounting as well as for correctness. Reporting sparse support while never using it means the capability appears in plans and results with no behavior behind it in phase 0.

Uncertain: the `fstore_t` field names and the `F_ALLOCATEALL` and `F_PEOFPOSMODE` constants were not verified against `libc`'s Apple module, as the rustix gaps record also notes. Whether preallocation measurably helps on any of the eight benchmark regimes is unmeasured; standards.md requires it, and the slice 0.2 harness should still report what it is worth.

Sources: Microsoft Learn `SetFileValidData`, `FILE_ALLOCATION_INFO`, `FSCTL_SET_SPARSE`; docs.rs `rustix` 1.1.4 `fs::fallocate`; Apple `fcntl(2)`; `xnu` `bsd/sys/attr.h` for `VOL_CAP_INT_ALLOCATE`; standards.md Disk; contracts.md Resume ladder.

## Slice B0 measurements

Question: The four questions earlier records left for the foundation slice to answer with measurement.

Options: Measure on this machine, measure in continuous integration, or report the question as still open.

Chosen:

Native toolchain on Windows on ARM. Answered. `aarch64-pc-windows-msvc` is a Tier 1 target with host tools, so `rustup` installs a native toolchain on `windows-11-arm` and nothing runs under x64 emulation. What it costs in build time is not answered, because this session has no Windows on ARM machine and did not run continuous integration.

Clippy pedantic at deny. Answered, and it stays at deny. The whole workspace compiles under the lint table in the lint policy record with exactly two exceptions, each carrying a written reason: `clippy::struct_excessive_bools` on `VolumeCapabilities`, whose four booleans are four capabilities the contract requires reported separately, and `clippy::cast_precision_loss` on the binary size the benchmark harness records. Two exceptions across every crate is not a stream of noise, so no separate warn gate is created.

Cold and warm continuous integration times. Not answered. The workflow exists and no run has happened, because this session has no remote and was told not to push. The estimates in the continuous integration record stand as estimates and the first green run replaces them.

Mimalloc on the two musl targets. Not answered. This machine is `x86_64-pc-windows-msvc` with no musl target installed, no cross linker, and no container runtime, so neither musl target can be built or run here. The allocator is wired behind a `mimalloc` feature on the `cli` crate, gated on `cfg(target_env = "musl")`, so the comparison is one benchmark run with the feature and one without on each musl runner.

Because: two of the four are settled by evidence available here, and two require hardware this session does not have. Reporting them as measured would be a defect of the same kind the standards call out.

Costs: phase 0 cannot close until a continuous integration run produces the two remaining numbers, and until a baseline exists for each of the six targets the benchmark gate cannot run anywhere but the machine that recorded one.

Uncertain: everything named as not answered above.

Sources: the Rust platform support page, the Tier 1 with Host Tools table; the real output of `cargo clippy --workspace --all-targets` in this session.

## Redaction rule for a location string

Question: Which parts of a location string are replaced with the redacted text at construction.

Options: A named list of signed and presigned parameters per provider; every query parameter value; the userinfo component only.

Chosen: the userinfo component in full, and the value of every query parameter. The parameter name is kept and its value becomes the fixed redacted text. A parameter with no value is kept as written.

Because: the contract names signed query strings and presigned URL parameters among the things never written anywhere, and it names no list of them. A per-provider list is a table Fetchloom would have to keep correct as providers change theirs, and a stale entry in it is a leaked credential. Redacting every value needs no table and cannot go stale. Redaction is idempotent, so reading a receipt or a plan back and writing it again does not change it, which is what makes the redacted location type deserializable through the same constructor.

Costs: a harmless query parameter is redacted too, so a location with a meaningful query reads less well in an error message and in `explain`. Nothing distinguishes a signed parameter from a page number in the output.

Uncertain: nothing.

Sources: contracts.md Credentials; the redaction contract test in `crates/engine/tests/contracts.rs`.

## What the benchmark harness can measure at phase 0

Question: The toolchain record says the harness records wall time, bytes read and written, peak resident memory, and binary size. Which of those can it record.

Options: Depend on the `platform` crate for the process counters; call the platform directly from `xtask`; record only what needs no platform call.

Chosen: wall time and binary size now. Peak resident memory and the byte counters are added when the `platform` crate exists and exposes them.

Because: the layout record states that `xtask` depends on nothing in the workspace, and peak resident memory has no portable reader in the standard library. Adding a platform call to `xtask` would be a second implementation of a query the `platform` crate is being written to own, which the standards forbid. Adding a dependency for it is not in any decision record.

Costs: the resident memory limit in contracts.md Limits has no benchmark behind it until the `platform` crate lands, so nothing mechanically catches a regression in it during phase 0.

Uncertain: whether `xtask` should be allowed to depend on `platform` alone, which would settle this cleanly and would weaken the layout rule. That is a decision for the session that adds the counters.

Sources: the toolchain record; the layout record; `xtask/src/bench.rs`.

## Crates that exist at the end of the foundation slice

Question: The layout record lists seven crates plus a cross-crate test directory. Which of them are created by the foundation slice.

Options: Create all of them so the tree matches the record; create only the ones with code in them.

Chosen: `engine`, `faults`, `cli`, and `xtask`. The `cache`, `sources`, and `archive` crates are created by the phases that implement them, phases one, two, and three. The `platform` crate is created by the slice that owns it. The cross-crate `tests` directory is created by the slice that writes the conformance corpus.

Because: an empty crate is a placeholder, which the standards forbid, and a crate whose only content is a module declaration is worse than no crate because it looks finished. Every seam trait those crates will implement already exists in `engine`, which is what the parallel slices actually compile against.

Costs: the tree does not match the layout record until phase three, so the record has to be read as the destination rather than the current state.

Uncertain: nothing.

Sources: the layout record; standards.md on placeholders; build-protocol.md on the foundation session.

## Exit code for a failure, derived rather than chosen

Question: Which exit code each error kind produces.

Options: A table from kind to code; a table from layer to code; a code field on each error.

Chosen: a mapping from layer to code, with the layer already fixed per kind by contracts.md.

Because: the two contract tables already determine it. Every layer has exactly one exit code whose stated meaning is that layer's failure: resolve to ten, transfer to twenty, verify to thirty, extract to seventy, materialize to sixty, cache to eighty, policy to forty, resource to fifty. Writing a third table from kind to code would be a second way to state a fact the first two already state, and it could drift from them.

Costs: an error kind that ever needs a code its layer does not produce cannot express it without changing the contract, which is the intended friction.

Uncertain: nothing.

Sources: contracts.md Errors and Exit codes; `crates/engine/src/outcome.rs`.

## Event payload fields

Question: What fields each event carries, given that contracts.md names the events and almost never names their fields.

Options: Give every event a free-form field map; give every event only the fields some document states; invent a field list per event.

Chosen: every event carries a sequence number, a timestamp, and where applicable the dataset and the artifact. Beyond those, an event carries only fields that contracts.md or standards.md states. Where a document states nothing, the event carries nothing and the slice that needs a field adds it with a contract change.

Because: the standards forbid inventing a field, and the observability section of the standards does state some of them: start and end events carry byte counts and durations. The contract states the rest that exist. The degrade event names what was requested, what was used, and why. The source selection event names the source and the reason. The resume rung is always reported. The extraction rejection names the rejection. The reconcile event names the outcome. A listing counts what it ignored.

Costs: several events carry less than a renderer will eventually want, and each addition is a contract change rather than a code change. The live display cannot show a fact no event carries, which is exactly what the contract requires of it.

Uncertain: whether the failover event naming both ends is a field the contract states or an extrapolation from the event's name. It is the latter, and it is the one place this record went past what is written down.

Sources: contracts.md Events, Source selection, Resume ladder; standards.md Observability; `crates/engine/src/event.rs`.

## What the comment checker calls a banner in Markdown

Question: The lint policy record says the checker rejects banners and section dividers in Markdown as well as in Rust. Markdown uses some of those characters as syntax.

Options: Reject every run of repeated punctuation; reject a fixed set of characters; reject nothing in Markdown.

Chosen: a Markdown line is a banner when, trimmed, it is four or more repeats of one character drawn from the set of equals, asterisk, hash, tilde, underscore, and plus. A hyphen is excluded, because a run of hyphens is a horizontal rule and a table separator, both of which are Markdown syntax rather than decoration. A heading is not a banner, because a heading has text after its hashes.

Because: the existing documents use horizontal rules and table separators throughout, and a checker that rejects them would be wrong about the repository it is checking rather than right about the rule. Every remaining character in the set has no syntactic meaning as a full line.

Costs: a line of hyphens used decoratively is not caught.

Uncertain: nothing.

Sources: standards.md on banners and dividers; the checker's own tests in `xtask/src/comments.rs`.

## Denying an unused crate dependency and the development dependencies

Question: The unused crate dependency lint at deny fires on a development dependency that a given target does not use, which is every crate with a test needing a library the library itself does not.

Options: Demote the lint to warn; drop the development dependency; write the documented suppression.

Chosen: the documented suppression, an underscore import of the crate, placed in the crate root behind a test configuration attribute or at the top of the test that needs it.

Because: the lint's own help text names that form as the remedy. Demoting it to warn loses the opposite direction the lint policy record wants it for, which is catching a dependency that is declared and no longer used.

Costs: one line per crate that has a development dependency its library target does not use, and the line reads like noise to someone who does not know the lint.

Uncertain: nothing.

Sources: the real compiler output in this session; the lint policy record.

## Pinned toolchain and the minimum supported version

Question: Which toolchain the repository pins, given that the locking record raised the minimum supported version to 1.89.

Options: Pin the minimum; pin the newest stable; float.

Chosen: `rust-toolchain.toml` pins 1.98.0, the newest stable at the time of writing, with clippy and rustfmt. The workspace minimum stays at 1.89, which is the floor the locking record set.

Because: the continuous integration record requires the toolchain to be pinned rather than floating, so a runner image change cannot move it. Pinning the newest stable rather than the floor means the lint table is evaluated by the compiler that will actually run in continuous integration, while the recorded minimum keeps stating what the code actually needs. The 2024 edition needs 1.85 and is therefore already covered by the 1.89 floor.

Costs: the pin has to be raised deliberately, and a new lint in a newer compiler will not be seen until it is.

Uncertain: nothing.

Sources: the continuous integration record; the advisory locking record on the 1.89 floor; the toolchain record on the 1.85 floor from the argument parser.

## Configuration file format and discovery

Question: What format a configuration file is written in, where the project and user files are found, and what the config flags do to that search. contracts.md fixed the flags, the two file levels, and a relative cache directory, but named no format and no path.

Options: JSON, because serde_json is already in the graph for machine output; TOML, adding the toml crate; or shipping phase 0 with only the command line, the environment, and the built-in defaults, leaving the two file levels unimplemented.

Chosen: TOML. Project configuration is `fetchloom.toml`, found by searching the working directory and then each parent until one is found or the filesystem root is reached. User configuration is `config.toml` in the platform's configuration location. That is the configuration location and it is never the cache location; the two are separate directories with separate defaults. Naming one file disables the search, and disabling configuration disables both levels. Unknown keys are an error and the reserved prefix is rejected. The toml crate at 1.1 is added to the workspace dependencies and to the cargo-deny allow list, along with toml_datetime, toml_parser, serde_spanned, and winnow.

Because: a configuration file holds proxy, transport security, and limit settings that a user needs to comment out and annotate while working out why a run behaves as it does, and JSON has no comments. That serde_json is already in the graph is not an argument, because it is there for machine output, locks, and receipts, none of which a person edits by hand. Manifests accept three surface syntaxes because strangers write them; configuration accepts one because the user in front of us writes it, and one format is one parser and one set of error messages. Deferring the two file levels was rejected because the settings report has to name which of the five precedence levels supplied each value, two of those levels are files, and building the precedence mechanism without them means retrofitting later into code that never had them.

Costs: toml and four transitive crates enter the graph and the allow list, and every one of them is binary size the no-op regime has to absorb. Version 0.9 was rejected on the way through because it resolves two versions of winnow, which the duplicate version rule correctly refuses.

Uncertain: what the five crates cost in binary size and in the no-op regime is unmeasured until the harness runs against a build that actually parses a configuration file.

Sources: contracts.md Configuration files, Flags, and Environment; the real output of cargo deny check in this session.

## Windows boot identity is read, never derived from uptime

Question: Where the boot field of the lock owner token comes from on Windows. The advisory locking record left this explicitly unsettled and named the choice between deriving it from uptime and reading a performance counter.

Options: derive the boot instant by subtracting the tick count from the current time; read the boot time through the undocumented system information call; read the boot counter the session manager keeps in the registry.

Chosen: the boot counter the session manager keeps in the registry, under the prefetch parameters of the memory management key. When it cannot be read, the boot field is unavailable and rung six of the liveness ladder applies: the holder is treated as live and the missing field is reported.

Because: a derived boot instant is not stable between two readers. Two live processes in the same boot subtract two independently drifting clocks, get two different values, and each concludes the other's record is from a previous boot and therefore stale. That is the one failure mode that steals a lock from a process that is still running, and it is worse than every failure mode the alternatives have. The registry value is exactly equal for every reader in a boot, and both of its failure modes degrade safely: if it is missing the ladder reports the field unreadable and never steals, and if it were frozen across boots the ladder falls through to the process identifier and start time checks, which detect a dead holder on their own. The undocumented system information call is not in the documented platform surface and was rejected for that reason.

Costs: the value is not documented by the platform vendor, and it is maintained by the prefetcher, so a system with the prefetcher disabled may not update it. Neither case can produce a stolen lock, but the second leaves a stale lock to be waited out rather than reclaimed.

Uncertain: the behavior of the value when the prefetcher is disabled was not tested, only reasoned about from the ladder. It was read on the development machine and returned a plausible counter.

Sources: the real registry read in this session; contracts.md Cache; the advisory locking record.

## Receipts and locks are not written in phase 0

Question: Where a receipt is written, given that verification is contracted to compare against one and no document says where one lives.

Options: invent a location beside the destination; put it inside the destination; put it in the cache; or establish that phase 0 does not write one.

Chosen: phase 0 writes neither a lock nor a receipt, and verifying a path recomputes its tree digest and reports it rather than comparing against a receipt. The comparison arrives with receipts.

Because: roadmap.md phase 4 lists lock writing and receipts in its Build column, and phase 0's Build column lists neither. Phase 0's own exit criterion is that a directory materialized on one platform reproduces an identical tree digest on the other two, which recomputation alone proves. Inventing a receipt location now would fix a portable surface path before the phase that designs the portable surface, and a receipt placed inside the destination would additionally appear as a foreign entry to the first reconcile that ran over it.

Costs: verification in phase 0 is a strict subset of its contracted behavior. That is stated rather than hidden, and it is not a placeholder, because the part that exists does real work.

Uncertain: nothing.

Sources: roadmap.md phase 0 and phase 4 Build columns; contracts.md Command surface, Receipt, and Reconcile.

## Where the on-access scanner threshold comes from

Question: The capability detection record left the ratio at which a scanner is called present unchosen, to be measured. Windows can name a filter driver directly; macOS and Linux cannot.

Options: measure it now; pick a value and mark it provisional; report every volume as having no scanner until a measurement exists.

Chosen: on Windows presence is decided by enumerating filter drivers and needs no ratio at all. On macOS and Linux presence is decided by the measured ratio exceeding two, and that two is provisional, lives in one named constant, and is replaced by the slice that measures it.

Because: the development machine for this phase is Windows, where the ratio does not decide anything, so the macOS and Linux threshold cannot be measured here at all. Reporting every volume as unscanned would be a silent wrong answer on exactly the platforms that cannot name the product, and contracts.md requires the scanner be reported with its measured cost. The repository already has precedent for a provisional value carried with its evidence quality stated, in the allocator row of the toolchain record.

Costs: a value with no measurement behind it decides a reported capability on two of three platforms until it is measured.

Uncertain: the value itself. It is a guess labelled as a guess and it must not survive the slice that can measure it.

Sources: contracts.md Platform capabilities; the capability detection record; the toolchain record's treatment of the allocator.

## The tree digest is hashed entry by entry rather than from one buffer

Question: Whether the canonical entry stream is materialized whole before it is hashed.

Options: build the whole stream into one buffer and hash it once; encode one entry at a time into a reused buffer and update the hasher per entry.

Chosen: the second. One buffer is allocated, cleared per entry, and fed to the hasher as each entry is encoded. The function that returns the whole stream stays, because the framing tests assert against the bytes themselves.

Because: standards.md requires that nothing is read whole into memory and that buffers are allocated once and reused. At the one million entry limit the whole stream is on the order of a hundred megabytes against a one gibibyte resident memory limit, spent for no reason. The digest is identical either way, which the pinned conformance value proves: it did not change when the encoding was moved from one buffer to per entry updates.

Costs: two paths exist over the same encoding, one returning bytes for tests and one hashing directly. They share the per entry encoder, so the encoding itself is still written once.

Uncertain: nothing.

Sources: standards.md Memory; the pinned portable core digest test.

## Only what performs is in the binary

Question: contracts.md names twelve commands and this build performs four of them. What the other eight do when a user types them, and what the same question means for flags and for configuration keys.

Options: define them and fail; define them and exit zero; leave them out until they work.

Chosen: leave them out. The parser holds `get`, `verify`, `completions`, and `explain` and nothing else. The eight that perform nothing are absent, so the parser rejects them the way it rejects any word it does not know. The same rule is applied to flags and to configuration keys: a flag exists only when setting it changes what the run does, and a configuration key exists only when the binary acts on it. That removed `--cache-dir`, `--verbose`, `--color`, `--no-hints`, and `--yes` from the global flags, everything except `--output` from the flags for `get`, and every configuration key except `offline`, `threads`, and `display`. Help and the completion scripts generate from what is present, so they describe this build exactly.

An earlier version of this record chose the opposite: define everything and exit two. That was wrong on three counts and is recorded here rather than deleted, because the reasoning is the point. Exit two means usage error, and a user who typed a contracted command correctly made no usage error, so the code was a false statement about the failure. A command present and unable to act is a placeholder, which standards.md forbids outright. And the same input would have produced exit two today and exit zero once the phase that builds it landed, which is drift in the observable surface.

Because: contracts.md Command surface now states it directly. Before 1.0 a command, flag, or value exists in the binary only once it performs what is written there, because a caller cannot distinguish a present-but-inert thing from a usage error. The stability argument that motivated the earlier choice does not apply before 1.0, when the surface is not frozen and nothing may yet depend on its shape.

Costs: the surface grows phase by phase, so a user who reads contracts.md sees commands this build does not offer. That is the honest direction to be wrong in, because the alternative is a binary that accepts a command and does nothing with it. Each later phase adds its own commands, flags, and keys, and the completion scripts change shape with them until 1.0 freezes them.

Uncertain: nothing. This replaces the earlier decision entirely.

Sources: contracts.md Command surface, the paragraph on what exists before 1.0; standards.md on placeholders; roadmap.md, which assigns each absent command to a later phase.

## Direct filesystem calls until the Platform seam exists

Question: how the local materialization path does its file work while no implementation of the Platform seam exists.

Options: write a platform implementation inside the command line crate; wrap the fault injection library around an implementation that does not exist; call the standard library directly and report what is missing.

Chosen: one module calls the standard library directly, and every capability that the Platform seam would have supplied is reported as a degradation rather than assumed. Atomic publication, capability detection, and advisory locking are all absent, and a run says so through a degrade event. The module's own docstring states that it calls the seam when the seam exists and that the direct calls are then deleted.

Because: writing a platform implementation in the command line crate would be the second implementation of the seam that the platform crate is being written to own, which the standards forbid outright, and it would be the harder one to delete. The fault injection library wraps an inner platform and cannot stand in for one that does not exist. Calling the standard library from one named module, with the gap reported at runtime rather than hidden, is the only option that leaves nothing to unpick beyond deleting the module's body.

Costs: publication is a plain rename with no volume check ahead of it, so a cross volume destination fails on the rename's own error rather than being refused before the call, which is what the atomic publication record requires. That is a real gap in the contract and it closes when the seam lands.

Uncertain: nothing. The gap is named in the code, in the event stream, and here.

## The progress line is suppressed rather than left plain

Question: contracts.md says progress is written only when the standard error stream is a terminal, and separately that the display mode is forced to plain or none when that stream is not a terminal. Which of the two the mode becomes.

Options: force the mode to plain and have the renderer stay silent; force the mode to none.

Chosen: none. When the standard error stream is not a terminal there is no progress output at all.

Because: the two sentences have to agree, and the only reading that satisfies both is that the mode which writes no progress is the one selected. The plain view's only progress artifact is the aggregated line, so a plain view that never draws it is the none view under another name, which is the second way of doing one thing that the standards forbid. The final result is unaffected, because it goes to standard output and never through the renderer.

Costs: a run whose output is piped reports its result and nothing else, so a long run in a pipeline shows no sign of life. That is what the contract asks for.

Uncertain: nothing.

Sources: contracts.md Output streams and Display modes.

## The live view falls back to plain and says so

Question: what happens when the live view is asked for, given that roadmap.md builds it in phase nine.

Options: reject the value; accept it and render nothing; accept it and render the plain view.

Chosen: the value is accepted, the plain view is rendered, and a degrade event names the live view as requested, the plain view as used, and this build as the reason.

Because: the flag and its values are the surface and are fixed now. A fallback that says what it did is exactly the rule the standards state for every fallback, and it needs no invented flag value and no invented error.

Costs: a user asking for the live view gets the plain one, and only the event stream and the degrade event say why.

Uncertain: nothing.

Sources: contracts.md Display modes; standards.md on fallbacks; roadmap.md phase nine.

## The help text no longer wraps to the terminal width

Question: the argument parser's help wrapping feature pulls a crate the toolchain record ruled out.

Options: keep the feature and allow the crate; drop the feature.

Chosen: drop the feature. The parser is configured with argument derivation and color only.

Because: the toolchain record chose to read the terminal size through calls that are already in the graph and states in as many words that this removes that crate from the graph. Enabling the wrapping feature put it straight back, which the dependency allow list caught. The record is the decision and the feature is the accident.

Costs: help text wraps at the parser's fixed default width rather than at the width of the window it is printed into. When the progress renderer needs the terminal width it reads it through the calls the toolchain record named, and the help text can be wrapped by the same reader at that point.

Uncertain: nothing.

Sources: the toolchain record; the real output of cargo deny check in this session.

## The no-op regime runs a real command

Question: the benchmark harness ran the binary with no arguments, which exited zero when no command surface existed and is a usage error now that one does.

Options: keep measuring the bare binary and accept the usage error; measure a command that does no work.

Chosen: the regime runs the settings report, which starts the process, discovers configuration, resolves every level, and moves no bytes.

Because: the standards say the no-op regime measures startup and configuration discovery, and the settings report is precisely those two things and nothing else. Measuring a process that exits on a usage error would measure the parser rejecting input, which is not what the regime is for. Reconciliation is named in the standards as part of this regime and joins it when a locked run against an unchanged destination exists to measure, which needs the lock that phase four writes.

Costs: the regime is not yet the whole of what the standards describe, and the number is not comparable to one taken after reconciliation joins it.

Uncertain: nothing.

Sources: standards.md Measure; the real output of the harness in this session.

## What this machine could not verify, and continuous integration must

Question: the development machine for phase 0 is a single Windows host. What that leaves unverified, so a judge session reads a list rather than rediscovering it.

Options: leave it implied by the absence of test runs; write it down.

Chosen: write it down, here, as the standing list. Every entry is something no run on this machine can settle. An entry leaves this list when a continuous integration run covers it, not when someone believes it works.

Resolved on this machine, so no longer on the list. Every workspace crate compiles for `aarch64-apple-darwin`. That check appeared to be impossible earlier, because `blake3` builds NEON through a C toolchain this machine does not have, and the failure was in the build script rather than in any Fetchloom code. Running the same check with the `pure` feature of `blake3` enabled temporarily, and reverting immediately, compiles the whole workspace clean for that target. The shipped feature set is unchanged and still builds NEON. What remains unverified for Apple targets is therefore the NEON build itself and every run, not whether the code compiles.

Compilation:

`x86_64-apple-darwin` has never been compile checked here at all, because only the `aarch64` Apple target is installed. No Fetchloom crate contains architecture-specific code, so the risk is low, and it is still unchecked.

The two musl targets compile clean here. Neither has ever been linked or run, because there is no musl linker and no container runtime on this machine.

Execution, all of it:

Nothing on macOS or Linux has been executed. Every test result reported for phase 0 comes from `x86_64-pc-windows-msvc`. That covers the digest work, the canonical entry stream, the command surface, configuration precedence, and the local materialization path, all of which are portable code, and none of which has been observed running anywhere else.

The cross-platform conformance corpus has never been transported. `conformance.rs` is pure data and its portable core has a pinned tree digest asserted on this machine only. All six directions named in roadmap.md remain unrun, and the pinned digest is the value one Windows host computed. If another platform disagrees with it, the pinned value is the thing to question first, not the platform.

The declared-failure set of the corpus has never been exercised against a real filesystem, because materializing it needs the Platform seam.

Capability probes, none of which exist yet:

Every row of the capability table is unverified on every platform, because Track A produced no code that survives. When it lands, each of the following is a probe this machine cannot exercise even for Windows in a meaningful spread: case folding and Unicode normalization on APFS in both its case-sensitive and case-insensitive forms, on HFS+, on ext4 with and without the case-folding flag, and on NTFS; clone support on Btrfs, XFS with reflink, APFS, ReFS, and NTFS, where NTFS is expected to report no support and fall back to a copy; sparse file support per filesystem; symbolic link permission on Windows both with and without Developer Mode and the privilege; hard link support; maximum component and path length per volume; network-backed detection against a real NFS, SMB, and network share, and against a FUSE mount, which must report unknown backing rather than network.

The one filesystem this machine can speak for is NTFS on a local volume, and even that is unverified, because no capability detection code exists to run against it.

Advisory locking is unverified everywhere. The cross-user case, the network filesystem refusal, and the `ENOLCK` and `EOPNOTSUPP` family that must produce `cache.locking_unsupported` all need filesystems and users this machine does not have.

The liveness ladder is unverified on macOS and Linux. The three sources of machine identity, the three of boot identity, and the three of process start time were chosen from documentation and, for the two Windows registry values, from a real read on this machine. The macOS `sysctl` names and the Linux `/proc` fields have been read by nobody.

Durability and publication:

No durability tier has been observed doing anything, on any platform. `F_BARRIERFSYNC` and its fallback, `F_FULLFSYNC`, `fdatasync`, and `MOVEFILE_WRITE_THROUGH` are all unexercised.

Cross-volume refusal is unexercised. This machine has two volumes and the test was written for Track A, which did not land, so even the Windows case is unproven.

Kill and restart during publication, which roadmap.md phase 0 names in its Prove column, has not been attempted at all.

Measurement:

No timing baseline exists for any runner. The local one recorded before the metric split was invalid, because it came from a machine running other work, and it was removed rather than re-recorded. Each of the six runners records its own on its first green run.

The on-access scanner threshold of two is a provisional constant with no measurement behind it, and it decides a reported capability on macOS and Linux. Windows names the filter driver instead and does not use it.

Whether the musl allocator choice helps is unmeasured, as the toolchain record already states, and needs one run with the feature and one without on each musl runner.

Because: a list of what was not verified is worth more than a claim about what was, and a judge session that has to derive this list will derive a shorter one. Every item here is a place where a green run on this machine means nothing.

Costs: the list is long, and it stays long until continuous integration runs. That is the accurate picture of a phase built on one host.

Uncertain: the list itself is only as complete as this session's view of it. It is a floor, not a ceiling.

Sources: roadmap.md phase 0 Prove and Done when; the capability detection, advisory locking, atomic publication, and preallocation records; the real command output in this session.

## The platform seam is one behavior behind three sets of syscalls

Question: how the Platform seam is split between what is shared and what is per platform, given that a previous attempt produced two and a half thousand lines with the decisions scattered through the platform modules.

Options: one module per platform, each implementing the whole trait; a shared trait implementation calling a small per-platform surface.

Chosen: the second, and the split is drawn at exactly the line between a decision and a syscall. The crate root holds every behavioral rule and is written once: comparing volume identity before a publish and refusing a cross-volume one, the rename-aside sequence that publishes a tree, the fallback from cloning to copying, the advisory lock, and the whole liveness ladder. Each platform module exposes sixteen functions and no more, and each of those is a call rather than a policy. A platform module cannot decide anything, because there is nothing left in it to decide.

Because: the decisions are the part that must be identical on three platforms, and the only way to guarantee that is to write them once. The previous attempt put the ladder and the cross-volume check inside the platform modules, which meant three copies of a rule that has to agree, on platforms that cannot all be tested from one machine. Sixteen named functions also gave the two builders a signature list to fill rather than a behavior to interpret.

Costs: the seam's own trait takes a concrete lock type, so the crate root owns the lock and a platform cannot choose its own primitive. That is intended: the locking record already settled on one primitive for all three.

Uncertain: nothing.

## Liveness answers three ways, not two

Question: what a platform reports when a process identifier exists but cannot be inspected.

Options: report it as gone, which the ladder reads as stale; report it as live; report it as neither.

Chosen: neither. The per-platform call returns started, gone, or unreadable, and the ladder maps unreadable to the undecidable rung that names the missing field. Only the platform saying the process does not exist produces stale.

Because: the first draft of this returned an option, where nothing meant gone. That is the one bug that costs a running process its lock: a process that exists but cannot be opened would have been reported as gone, read as stale, and its lock taken while it was still working. The contract says liveness is decided from the recorded values and a lock is never stolen on missing evidence, and an option cannot express the difference between no evidence and evidence of absence. On Windows the two are separated by the error the platform returns when the process cannot be opened; on Linux by whether the process's own directory is absent rather than unreadable.

Costs: a third state every platform must answer for, and a platform that cannot tell the two apart has to say so rather than guess.

Uncertain: whether Windows returns exactly one error for a process that does not exist, or several. The implementation treats one as gone and everything else as unreadable, which fails safe: an unrecognized error leaves the lock alone.

## Publication is refused before the call, never diagnosed after it

Question: how a cross-volume publish is detected.

Options: attempt the rename and interpret the failure; compare volume identity first.

Chosen: compare first, in the crate root, before any platform call. The comparison is on the containing directories, because the target of a publish does not exist yet and cannot be asked what volume it is on.

Because: the atomic publication record requires it, and interpreting a failure afterwards would need a different error code on each of three platforms, none of which the vendor documentation enumerates for this case. Comparing identity is one mechanism everywhere. It is also the only way to be sure the refusal is a refusal: a rename that failed for some other reason would otherwise be reported as a cross-volume attempt.

Costs: one extra identity query per publish, on both sides.

Uncertain: nothing. This is the one path a second volume on the development machine could exercise for real, and it is covered by a test that asserts both the error kind and that the target does not exist afterwards.

## Killing a run is tested at every stage rather than at one

Question: how to test that a killed publication leaves no partial object.

Options: kill a child after it publishes; kill it at a random moment; kill it at each named stage.

Chosen: the child aborts at a stage named through the environment, and the parent runs it once per stage: after the file is created, after it is written, after it is flushed, and after the rename. At every stage the parent asserts that anything in the object directory is the whole object, that no stage left more than one, and that at least one stage reached publication at all.

Because: the first version of this test killed the child only after the publish completed, which tested nothing about being killed during one. It also passed vacuously when the directory was empty, so it would have stayed green with the publish removed entirely. The last assertion is what closes that: a test that proves nothing is worse than no test. Naming the stages rather than timing them keeps the test deterministic and needs no sleep, which the standards forbid.

Costs: the stages are the ones this implementation has, so a publication that grows a step needs a stage added here.

Uncertain: nothing on this platform. The same test runs unchanged on the other two.

## What a capability probe is asserted against

Question: how to test a probe whose correct answer differs per machine.

Options: assert the value this machine produces; assert the probe agrees with the filesystem.

Chosen: the second, everywhere. The case folding test creates a name and then attempts its other case, and asserts the probe said what the filesystem just did. The normalization test does the same with two spellings. The clone test performs a copy and asserts the mechanism reported matches what the probe predicted. The symbolic link test attempts one and compares.

Because: a probe asserted against a hardcoded value is a test of one machine, and it either fails on every other machine or is written so loosely it cannot fail at all. Asserting agreement with the filesystem is the same assertion on every platform and every volume, which is what lets one suite run unchanged in all six continuous integration jobs. It also tests the thing that actually matters: not what the answer is, but whether the probe is right.

Costs: a probe and a filesystem that are wrong in the same way agree, and the test passes. Nothing here can catch that.

Uncertain: nothing.

## What the platform seam leaves for continuous integration

Question: the earlier coverage record listed every capability probe as unverified because no platform crate existed. It exists now and its suite runs. What is still unverified.

Options: revise the earlier record in place; add what the platform work settled and what it did not.

Chosen: add it here. The earlier record stands for everything outside this crate.

Settled on this machine, and no longer open. The seam's suite runs forty-five tests on `x86_64-pc-windows-msvc` with fourteen named skips. What that proves, for real, on NTFS: file and volume identity including that two volumes differ; fingerprints changing with content; exclusive creation refusing an existing name rather than truncating it; publication by rename under all three tiers, including replacing an existing object and publishing a tree over an existing tree with no sibling left behind; preallocation setting the whole length; the copy fallback producing byte-identical output and reporting itself as a degradation; the whole liveness ladder including that a live holder is never stale and a holder on another machine is never stale even when its process is long gone; a lock refused to a second holder in this process and to a second process, and released when its holder exits; and a killed publication leaving no torn object at any of four stages.

Two things this machine could exercise that the earlier record listed as impossible. The cross-volume refusal is proven for real, because this machine has two NTFS volumes, and the test asserts both the error kind and that the target does not exist afterwards. Symbolic link creation is proven unprivileged, because Developer Mode is on here, so that probe is not merely returning false.

Still unverified, and now specific to this crate:

Every line of the Linux and macOS modules. Both compile clean for their targets and neither has ever run. That is roughly half the platform crate. The Apple module in particular carries every unsafe block the syscall wrapper does not cover, and no assertion has ever been made about any of them at runtime.

The Apple constants were confirmed against the C library's own source in this session rather than against vendor documentation: the preallocation record's field names and its two flags, the barrier flush command, the mount flag that marks a volume local, and the clone call's signature. One constant is not in the C library at all and is named from the platform header: the flag telling a clone not to follow a link. That value is unverified by anything.

The Apple process start time does not use the record the earlier records assumed. That type is not in the C library, so the implementation reads the process's own information record instead, which carries the start time directly. Nothing has run it.

The Linux process start time parses the field after the last bracket of the status line, which is the part that is easy to get wrong and impossible to check here.

Block cloning has never succeeded anywhere. This machine has no ReFS volume, so every clone attempt here fails and exercises only the fallback. The Windows clone path, the Linux one, and the macOS one are all unrun. A runner with Btrfs, reflink XFS, APFS, or ReFS is what first proves any of them.

The scanner measurement runs here and reports, but no scanner was ever detected, so the filter driver enumeration has returned an empty list every time. Whether it names a real product is unproven. On macOS and Linux presence is decided by the provisional ratio alone, which remains a guess.

The volume capability query on macOS reads a reply at an offset the kernel chooses and trusts the valid array before believing a capability. Neither the parse nor the gating has run.

Every ignored test names the filesystem it needs. Nine are in the capability suite: a block-cloning volume, a reflink volume, APFS, case-insensitive APFS, HFS+ normalization, a case-folded ext4 directory, a network share, a FUSE mount reported as unknown rather than network, and a volume without sparse support. Four are in the locking suite: locking across users, a network volume refused for a shared cache, and a filesystem whose locking fails in the way that must produce the unsupported error. Those names are the list a runner has to satisfy.

Because: the earlier record could only say that everything was unverified because nothing existed. Naming what a Windows machine did prove, and what it structurally cannot, is what tells a judge session where to look.

Costs: the list is shorter than it was and still long. Half of a crate that cannot run here is the honest description of building a platform layer on one platform.

Uncertain: the same caveat as the earlier record. This is a floor.

## Phase 0 gate

Question: Does phase 0 meet its exit criteria, and what remains unproven.

Options: Close it; hold it open until every filesystem in the roadmap's Prove
list has been exercised.

Chosen: Closed, with the filesystem gap carried into phase 1 as a named debt.

Because: the exit criterion is a tree reproducing an identical digest across
platforms with no network code in the binary. All six targets independently
produce the committed tree digest for the portable corpus, which is a stronger
statement than pairwise agreement because the reference is fixed. All eleven
continuous integration jobs are green, 163 tests pass on every target, and no
async runtime or transport is linked.

The first three runs found five real defects, every one of them in code that had
compiled on Windows and never been linted or test-compiled elsewhere: three
clippy findings in the Unix probe, four test files that did not name libc and
rustix as used on Unix, and a benchmark gate that failed a runner measuring
itself for the first time rather than recording a baseline. This is the evidence
that the platform code needed a machine, not a review.

Costs: fourteen named skips remain, and they are the honest gap. Block cloning
has never succeeded on any machine, because no runner offers ReFS, btrfs, or
XFS. A network-backed volume, a case-sensitive APFS volume, and tmpfs are
likewise unexercised. Phase 1 exercises clone and lock paths continuously, so a
runner with those filesystems belongs there rather than in a phase that has
nothing to clone.

Uncertain: the portable corpus is constructed by the same code on every target
rather than transported between them, so the agreement proves the canonical
encoding is platform-independent, not that a materialized tree survives a
physical move. Phase 4 moves a bundle between machines and settles that.

## A lease is a shared advisory lock, not a record on disk

Question: what "leased by a running process" means on disk, so that prune can
tell a lease held by a living reader from one left behind by a process that was
killed.

Options: a lease file carrying an owner token and an expiry, swept by the same
liveness ladder the write lock uses; a shared advisory lock on the digest's
existing lock file, held for as long as the reader has the object open.

Chosen: the second. A reader takes `lock_shared` on `locks/<digest>.lock`
before it looks for `objects/<digest>`, and holds it until it is finished with
the object. A writer takes the exclusive lock on the same file. Prune attempts
the exclusive lock without waiting, and an object whose lock is held by anyone
survives that sweep.

This adds one method pair to the Platform seam, `try_lock_shared` and
`lock_shared`, alongside the exclusive pair already there. The seam's shape is
unchanged: the same concrete lock type, released by dropping it.

Because: a lease record has to answer whether the holder is alive, and the only
answers available are the liveness ladder, which is evidence about a process
identifier rather than about a lease, and an expiry, which contracts.md Cache
forbids by requiring liveness to be decided from recorded identity and never
from a modification time. A kernel-held lock answers it directly: both `flock`
and `LockFileEx` are released when the last descriptor closes or the process
dies, so a killed reader releases its lease with no record to sweep and no way
for prune to be wrong about it. It is also the primitive already chosen for the
writer, so leases and writes are one mechanism rather than two.

Ordering matters and is part of the contract this record fixes: a reader locks
first and looks second. Prune deletes only while holding the exclusive lock, so
a reader that holds the shared lock cannot have the object removed underneath
it, and a reader that finds the object present has already made it unremovable.

Costs: a reader holds a lock file open for as long as it reads, so a cache with
many concurrent readers keeps many descriptors open, bounded by the objects in
flight. A wedged reader keeps an object alive indefinitely, which is the same
tradeoff the write lock already carries.

Uncertain: nothing. The shared and exclusive pairs are the standard library's,
stable since 1.89, and their platform mapping is documented.

Sources: `std::fs::File` `lock_shared`, `try_lock_shared`, `TryLockError`;
contracts.md Cache; the advisory locking record above.

## A lock is valid only while its file is still the file that was locked

Question: prune removes a lock file while holding its lock, and a name that has
been unlinked can be recreated by another process. What stops two processes
each believing they hold the same digest's lock.

Options: never remove lock files, so the race cannot happen and the directory
grows by two files for every digest the cache ever held; remove them and verify
after acquisition that the locked handle is still the file the name refers to.

Chosen: the second. After the lock is taken, the identity of the locked handle
is compared against the identity of the path. They differ when the file was
unlinked and recreated between the open and the lock, and the acquisition then
starts again from the open. This adds `file_id_of`, taking an open file, to the
Platform seam, and the existing path-taking query is written in terms of it.

Because: the race is real on all three platforms and was measured here rather
than assumed. On Windows, `std::fs::remove_file` succeeded against a file this
process held open and exclusively locked, and an immediate reopen of the same
name created a new file rather than failing with a sharing violation or a
pending deletion. Unix has always behaved this way. Without the identity check,
prune removing a lock file is a single-writer violation; with it, the loser of
the race notices and retries, which is what makes prune able to reclaim
`locks/` at all.

Costs: one identity query per acquisition, and a retry loop that is bounded by
attempts rather than by time.

Uncertain: whether every Windows build behaves as this one did. The check is
correct whether the platform unlinks immediately or defers, so the behavior it
guards against does not have to be predicted.

Sources: a probe run on `x86_64-pc-windows-msvc` in this session, printing
`remove while holding open+locked: ok` and `reopen after remove: ok`;
`std::fs::File` locking documentation; the advisory locking record above.

## A waiter learns the outcome by taking the lock and looking again

Question: how a process that waited on another process's transfer learns
whether that writer finished, failed, or died.

Options: the writer records an outcome the waiter reads; the waiter watches for
the object to appear; the waiter takes the lock the writer held and asks the
cache the same question it asked before it waited.

Chosen: the third, and there is no other channel. The sequence is: take the
shared lock, look for the object, and use it if it is there. Otherwise take the
exclusive lock, which blocks until the writer releases it, and look again. The
object being present means the writer finished, because nothing enters
`objects/` except a completed verification and rename. The object being absent
means the writer failed or died, and the process now holding the exclusive lock
is the writer.

Because: an outcome record is a second source of truth about whether an object
exists, and it can disagree with the directory. It also cannot be written by a
process that was killed, so the failed case and the died case would need
different handling for no gain: both mean the object is absent and someone has
to transfer it. Waiting for the object to appear needs a timer or a watch, and
contracts.md forbids deciding anything from a clock. Taking the lock is the
wait, the kernel releases it however the writer ended, and the directory is the
answer.

The `cache.wait` event fires when the exclusive lock blocks, and it names the
holder from the owner record, which is what the owner record is for.

Costs: a waiter that loses a race to a third process waits a second time. The
loop is bounded by the wait timeout, not by attempts.

Uncertain: nothing.

Sources: contracts.md Cache, a second process wanting an object being written
waits and reuses the result; contracts.md Events `cache.wait`.

## Startup recovery is one boot generation sweep, run once per boot

Question: which partial and staging entries are orphans, when the sweep runs,
and what it costs a run that has nothing to recover.

Options: sweep whenever the cache is opened; sweep entries whose writer is not
live; sweep entries recorded in a previous boot, once per boot.

Chosen: the third, exactly as contracts.md words it. Every entry in `partial/`
and `staging/` is created together with an owner record naming the machine, the
boot, the process and its start time, which is the token the locking record
already defines. Recovery removes an entry whose recorded machine is this
machine and whose recorded boot is not this boot. An entry from another machine
is left alone, because a directory another machine also writes to is not this
machine's to recover. An entry from this boot is left alone whether or not its writer is still running:
a partial from this boot is resume material, and its writer's lock is what
keeps a second writer off it.

The sweep runs at most once per boot per cache. `meta/recovered` holds the boot
identity of the last sweep, and a cache whose file already names this boot skips
the sweep after one read. The file is written after the sweep, under the lock
that guards it, so two processes racing to recover perform one sweep between
them.

Because: contracts.md says orphaned staging and partial entries from a previous
boot are removed at startup, and the boot generation is the whole rule. It also
says the binary does no work at startup that a command does not need, so a sweep
on every cache open would put a directory enumeration in front of the no-op
regime; one file read is what remains after the first run of a boot. Removing
entries by liveness rather than by boot would delete the partial belonging to a
run the user just killed, which is the transfer the resume ladder exists to
continue.

Staging directories are never resumable, so a staging entry from a previous boot
is removed outright. A partial entry from a previous boot is also removed,
because contracts.md names it as an orphan; resume across a boot would need the
partial's recorded source identity to be revalidated, which is phase 2's work
and not a reason to keep bytes nothing points at.

Costs: one owner record written beside every partial and staging entry, and a
file read per cache open. A machine whose boot identity cannot be read never
recovers, which the liveness ladder already treats as undecidable, and the sweep
reports that it did not run rather than guessing.

Uncertain: whether a cache shared between machines wants a per-machine recovery
marker rather than one file. One file per machine identity under `meta/` is the
obvious answer if it does, and it is not needed until a second machine writes.

Sources: contracts.md Cache; the advisory locking record's owner token; the
atomic publication record on the sibling directory a killed tree publish leaves.

## The cache format fingerprint is an unordered hash of named format facts

Question: what exactly is hashed to produce the value in `format`, given that
standards.md requires an unordered hash of the format definition that nothing
can branch on.

Options: hash the source of the cache crate; hash a hand-written version string;
hash a set of statements describing the format, combined so that order cannot
matter.

Chosen: the third. The format definition is a set of short ASCII statements,
each naming one fact about the on-disk format: the name of every directory, the
naming rule for every file in it, the digest algorithm and its text encoding,
the field list of the owner record, the field list of a pin record, the outboard
group size and threshold, and the canonical form the records are written in.
Each statement is hashed with BLAKE3 `derive_key` under the context
`fetchloom cache format`, the resulting 32-byte values are combined by exclusive
or, and the fingerprint is the derived-key hash of that accumulator.

Because: exclusive or of hashes is the standard unordered combination, so the
value cannot depend on the order the statements are written in, which is what
standards.md means by an unordered hash and what stops the list from acquiring
an implicit sequence that looks like a version. Hashing the crate's source
would change the fingerprint when a comment or a test changed, which would
force a cache clear for a change that cannot affect any byte on disk. A version
string is a version field wearing a different hat.

A statement is added or edited only when the on-disk format changes, and that
change makes every existing cache fail with `cache.format_mismatch` and the
instruction to run `cache clear`. Nothing reads the fingerprint for anything
but equality, and the type carries no ordering that a branch could use.

The test that keeps this honest asserts two things: changing any statement
changes the fingerprint, and permuting the statements does not.

Costs: the statement list is written by hand, so a format change that nobody
records in it produces a fingerprint that does not move and a cache that is
silently wrong. That is the one failure this construction cannot detect, and
the review gate for any change under the cache crate is to ask whether a
statement belongs with it.

Uncertain: nothing.

Sources: standards.md One thing; contracts.md Cache and Errors
`cache.format_mismatch`; docs.rs `blake3` `Hasher::new_derive_key`.

## Ownership is the filesystem's answer, not a recorded one

Question: how prune tells which objects the invoking user created, and which
filesystems cannot express the ownership rule contracts.md states, given that
sharing is not a mode and every cache is treated as one several users may reach.

Options: record the creating user in a file beside each object; ask the
filesystem who owns the object.

Chosen: ask the filesystem. The Platform seam gains one query returning the
owner of a path and one returning this process's own owner identity, both as an
opaque identity that is only ever compared for equality. On Unix that identity
is the numeric user identifier from `stat`, and this process's is `geteuid`. On
Windows it is the owner security identifier read with `GetSecurityInfo` for
`OWNER_SECURITY_INFORMATION`, and this process's is the user in its own access
token, compared as bytes.

Permissions, on Unix, and they are the same whether or not anyone else ever
opens the directory: `objects/`, `outboard/`, `partial/`, `staging/`, `meta/`,
`locks/` and `pins/` are mode 0777 with the sticky bit, so every user who can
reach the cache may add entries and only an entry's owner may remove it; objects
are published mode 0444, because an object is immutable once it is in `objects/`
and no user has cause to write one; the two lock files stay mode 0666 as the
locking record already fixed. Whether anyone else can reach the cache at all is
decided by the directory the user put it in, which is the user's decision and
not Fetchloom's: the default location sits under a home directory that is
already private.

On Windows Fetchloom sets no access control entry of its own. A directory
inherits its parent's, so a cache under the per-user default is private and a
cache an administrator created for several users carries the entry that
administrator chose. There is no configured group, because there is no mode to
configure and inheritance already expresses exactly the intent the person who
made the directory had. The sticky bit has no Windows equivalent, so removal of
another user's object is prevented by prune's ownership check rather than by the
filesystem.

Filesystems that cannot express it, and are refused: any volume with no
ownership model at all, which is FAT and exFAT; any volume mounted so that every
file reports one identity regardless of who wrote it, which is what an SMB or
CIFS mount with a fixed user option does and what a network mount of a Windows
share looks like from Unix; and every volume already refused by the locking
record, because a cache without cross-user locking is refused with
`cache.locking_unsupported` before ownership is even asked about. The probe is
empirical and matches the capability record's shape: create a file in the
cache's own staging directory, ask who owns it, and refuse the volume when the
answer is not this process's own identity.

Because: a recorded owner is a claim by whoever wrote the record, and the other
users of a cache directory are exactly the parties the record would be
protecting against. The filesystem's answer is the only one that is not
self-asserted. Comparing opaque identities for equality keeps one rule on three
platforms and never needs a user name, which would be a lookup that can fail and
a string that can collide.

Costs: one ownership query per object prune considers, which is a stat the sweep
was already making on most of them. World-writable directory modes look alarming
in isolation and are safe only because the sticky bit is set and the parent
directory is what decides reachability. On Windows the removal rule is enforced
by Fetchloom rather than by the filesystem, so a user with permission to delete
another user's object could do so with any other tool.

Uncertain: the exact Windows call sequence for reading an owner from an open
handle and for reading the token user was chosen from the documented API surface
and has not been compiled or run in this session. Whether a given SMB mount
reports a fixed identity is discovered by the probe rather than predicted, which
is why the probe exists.

Sources: contracts.md Cache Modes, sharing is not a mode; the advisory locking
record on `locks/` permissions; Microsoft Learn `GetSecurityInfo`,
`GetTokenInformation`; `stat(2)`; `geteuid(2)`.
## A cache that cannot be used is a degradation, never a failure

Question: what a run does when the cache directory is missing, read-only, or
out of space, and how each of the three is detected.

Options: check the cache up front with a free-space query and a permission
probe; discover each condition at the point of the operation that fails.

Chosen: discover at the point of failure, then degrade once. The cache is
opened lazily by the first operation that needs it. A missing directory is
created, and a failure to create it is the missing case. A read-only cache is
the failure to create the format file or an entry, reported by the platform as
a permission or read-only-filesystem error. A full cache is the failure of the
preallocation or the write, reported as no space left. Each of the three emits
one `degrade` event naming the cache as requested, no-cache behavior as used,
and the platform's own reason, and the run continues exactly as `--no-cache`
would: the partial transfer lives beside the destination and is discarded on
success.

The degradation is decided once per run and latched. A cache that failed is not
retried later in the same run, so one run cannot emit the same degradation for
every object.

Reading is separate from writing. A cache that can be read but not written is
still used for hits; only the writing side degrades. That is the read-only case
and it is the common one, a cache directory shared read-only or on a read-only
mount.

Because: a free-space query up front is a guess with a race behind it, since
another process may consume the space between the check and the write, so the
write has to handle exhaustion anyway and the check adds a second code path
that can be wrong. contracts.md Disk accounting requires a space check before a
transfer begins, and that check is about the destination, which is the real
copy and must fail rather than degrade. The cache is an optimization, so the
same condition has a different answer there, and the two are not one mechanism.

Costs: the first failing operation is the one that discovers the condition, so a
run degrades partway rather than at its start, and the degrade event may arrive
after some objects were already cached. Latching means a cache that becomes
writable again mid-run stays unused for the rest of it.

Uncertain: which error the platform reports for a full volume during
preallocation as opposed to during a write differs per filesystem, so the
detection matches on the error kind the standard library reports rather than on
a code, and a volume that reports something else degrades through the generic
path with its own message rather than being misreported.

Sources: contracts.md Cache Modes, a cache that is missing, read-only, or out of
space does not stop a run; contracts.md Disk accounting; standards.md on
fallbacks and `degrade`.

## Prune sweeps under the lock, and the clock only sets the pace

Question: what the grace period in mark, grace, sweep protects, how long it is,
and where a mark lives.

Options: grace as a retention window, so recently used objects survive; grace as
the interval between marking an object unreferenced and removing it.

Chosen: the second. A mark is a record under `meta/prune/` naming a digest and
the instant it was marked. A sweep removes an object only when it is still
unpinned, its exclusive lock can be taken without waiting, and its mark is older
than the grace period. Taking the lock is what makes the removal safe; the grace
period only stops a prune from removing an object a run is about to lease.

The default grace is sixty seconds. It is a race window, not a retention policy:
the window between a process deciding it will use an object and taking its
shared lock is bounded by a resolution step, not by anything a user does.

Because: correctness cannot come from the clock, and contracts.md Determinism
says so directly. The lock is the authority in both directions: an object under
any lock is never swept, and an object being swept is under the sweeper's
exclusive lock, so no reader can be part-way through opening it. If the grace
period were zero the invariants would still hold, and the only thing lost would
be objects a starting run was about to want. Sixty seconds is chosen to exceed
that window by a wide margin while being far below any interval at which a user
would notice prune deferring work to a second run.

A mark that is younger than the grace survives, and the object is swept by a
later prune. A mark for an object that has since been leased or pinned is
removed along with the mark, because the object is now referenced and marking it
again later is one directory read.

Costs: reclaiming space takes two prune runs when the first one marks. A crash
between mark and sweep leaves marks behind, which the next prune reads and acts
on, so they are not orphans.

Uncertain, and raised rather than invented: contracts.md Limits does not list a
grace period and Flags names no way to set one, so this build compiles the
default in and offers no flag, which is what the rule about a flag existing only
when it performs requires. Whether the grace belongs in Limits as a configurable
default is a contract question.

Also uncertain and named here rather than worked around: contracts.md requires
that an object referenced by a lock in the working directory survives prune. No
build writes a lock yet, so in this phase nothing is referenced by one and the
rule has no subject. It is implemented when locks are, in phase 4, and the test
for it is written then.

Sources: contracts.md Cache, Determinism, Limits, Flags; the lease record above.

## Filesystems come from images the workflow builds

Question: how btrfs, XFS with reflink, ReFS, APFS in both case forms, HFS+,
tmpfs and a network-backed volume are made to exist on hosted runners, so that
cloning, locking and the capability probes are exercised rather than skipped.

Options: pay for runners with those filesystems attached; build each filesystem
on the runner from a loopback or virtual disk image.

Chosen: build them on the runner. Linux creates a file, formats it, and mounts
it: `mkfs.btrfs` for btrfs and `mkfs.xfs -m reflink=1` for XFS, both from
packages the workflow installs, plus `/dev/shm` for tmpfs and a loopback NFS
export mounted from localhost for the network-backed volume. macOS creates
sparse disk images with `hdiutil create -type SPARSE -fs APFS`, the same with
`Case-sensitive APFS`, and the same with `HFS+`, each attached at a named mount
point. Windows creates a virtual disk with `New-VHD`, mounts it, and formats it
with `Format-Volume -DevDrive`, which is ReFS with block cloning, supported on
the `windows-2025` image.

Each job exports the mount points it created through environment variables named
for the property they carry rather than for the filesystem, and every test that
is skipped today reads the variable that names what it needs. A variable that is
not set leaves the test skipped, so a runner that cannot build an image reports
a skip rather than a failure, and the test itself is unchanged.

Because: the phase 0 gate closed with block cloning never having succeeded on
any machine and fourteen named skips, and this phase clones and locks
constantly. A runner that cannot clone would let every clone path stay unproven
until the phase ended. Building the images costs seconds per job and turns the
skips into runs on the same suite.

Costs: three more setup steps per platform, each of which can fail for reasons
that have nothing to do with Fetchloom, and a Windows step that depends on the
virtual disk cmdlets being present on the image. The network-backed case is
covered on Linux only; a macOS or Windows network volume is not built here and
stays a named skip.

Uncertain: whether the arm Linux and arm Windows images carry the same tools as
their x86 counterparts, and whether a hosted runner permits the loopback mounts
and the virtual disk attach at all. Every one of those is answered by the first
run rather than by a claim here.

Sources: `mkfs.btrfs(8)`; `mkfs.xfs(8)` on the reflink option; `hdiutil(1)`;
Microsoft Learn on Dev Drive and `Format-Volume`; the phase 0 gate record.

## Asking whether an object is present takes no lock

Question: whether the question "does the cache hold this object" takes the
shared lock that a read takes.

Options: take the shared lock, so the answer cannot go stale; take nothing, and
make the answer advisory.

Chosen: take nothing. The lock is taken by opening the object, which looks
again once it is held. The presence question is only ever a reason to open.

Because: the first version took the shared lock and deadlocked, which the tests
found by hanging rather than failing. A writer holds a digest exclusively and
then asks whether the object is already there, which is the whole of the
wait-and-reuse sequence, and a shared acquisition from the same process blocks
against the exclusive one the same process is holding. The lock is per open file
description on Unix and per handle on Windows, so a process cannot be told that
it is itself the holder.

Making the answer advisory loses nothing, because a caller that acts on a hit
opens the object, and opening takes the lease before it looks. Prune removes
only under the exclusive lock, so an object that was present a moment ago and is
gone by the time it is opened produces an absent error rather than a torn read.

Costs: a caller that trusts the answer without opening the object is wrong, and
nothing in the type system says so. The docstring does.

Uncertain: nothing.

Sources: contracts.md Cache; the lease record above; a hung test run in this
session.

## Bytes whose digest is not known until they are read

Question: how a local source enters the cache, given that a lease is taken on a
digest and a local file's digest is not known until it has been read.

Options: hash the source, then transfer it, which reads every byte twice; write
to the cache under a name of this process's own and rename it to the digest once
the digest is known.

Chosen: the second. The bytes are read once, hashed as they are written to
`partial/<pid>-<start>.ingest`, and renamed to `partial/<hex>` and then into
`objects/` once the digest is known and the lease on it is held. A digest the
cache already holds discards the scratch file and reports a hit.

Because: standards.md says hashing runs on the bytes as they arrive and there is
never a second read to compute a digest. A manifest transfer knows its digest
before it starts and takes the lease first, which is the path phase 2 uses; a
local source and phase 8's inference do not, and this is the one operation that
covers them. Renaming into `partial/<hex>` before publishing keeps the format
statement about what a partial is named true, and costs one same-volume rename.

Nothing appears under a digest it does not hash to, because the name is given
after the hash is known and the publication into `objects/` is the same rename
every other object goes through.

Costs: a scratch name in `partial/` that recovery treats like any other entry,
and one extra rename per ingest.

Uncertain: nothing.

Sources: standards.md Optimization, CPU; contracts.md Cache; the format record
above.

## Ownership is one question, not two values

Question: whether the platform reports who owns a file and who this process is,
or answers directly whether a file belongs to this process.

Options: two queries returning an opaque identity, compared by the caller; one
query answering the question.

Chosen: one query, `owns`. Unix compares the file's user against the effective
user. Windows compares the file's owner against both the token's user and the
token's owner.

Because: the two-value form was wrong on Windows and continuous integration
proved it. A process running with an elevated token creates files owned by the
administrators group rather than by the user, so prune compared the object it
had just written against the token user, found them different, and skipped every
object in the cache as another user's. Both Windows runners failed and no other
platform did. Windows has two identities that both mean this process, and only
the platform can say so, which is exactly what a seam is for.

Costs: a caller that wants to report who owns a file cannot, and would need a
second query added when a command needs to print it.

Uncertain: whether a service account or an impersonating token has a third
identity that means the same process. The question is answered by the platform
rather than by the caller, so a third one is added where the other two are.

Sources: a continuous integration run in this session failing
`a_pinned_object_survives_a_prune_that_removes_everything_else` on both Windows
targets; Microsoft Learn `TokenOwner`, which documents it as the identifier
applied to objects the process creates.

## A volume with no room is a resource failure wherever it is found

Question: what error kind a cache operation produces when the volume is full,
given that the same call can fail for a permission, a missing directory, or no
space.

Options: report the kind the operation belongs to; recognize the platform's
no-space report and produce a resource failure instead.

Chosen: the second, in one helper each in the platform crate and the cache
crate, so every filesystem failure passes through one place that asks the
question.

Because: a macOS runner ran out of room while creating a lock file and reported
`cache.locked`, which says contended and means something a user would act on
differently. The condition is the volume, not the lock. Matching on the standard
library's own storage-full kind rather than on a platform error number keeps one
rule on three platforms.

Costs: a filesystem that reports something else for exhaustion is reported as
whatever kind the operation belongs to, and reads as a corruption rather than a
full volume.

Uncertain: whether every filesystem in use reports exhaustion as the standard
library's storage-full kind. The small-volume regime in continuous integration
is what answers that per platform.

Sources: a continuous integration run in this session failing
`a_volume_with_no_room_left_fails_the_transfer_rather_than_the_cache` on both
Apple targets with `No space left on device` inside a `cache.locked`.

## A record is written beside its name and renamed onto it

Question: how a record another process may be reading at the same moment is
written.

Options: write in place, which truncates and then fills; write beside it and
rename onto it.

Chosen: the second, for every record the cache writes.

Because: eight processes racing for one digest found this immediately. A waiter
read the owner record of the lock it was waiting on while the holder was part
way through writing it, and parsed an empty file, which failed the run over a
file that was correct a moment later and correct a moment after. Writing beside
and renaming means a reader sees the previous record or the next one, never
half of either, which is the same rule publication already follows.

The strict parse stays. A record that does not parse is now evidence of
corruption rather than of a race, which is what makes failing on it correct.

Costs: one extra file and one extra rename per record write, and a scratch name
in the directory if a process is killed between the two.

Uncertain: nothing.

Sources: a failing race in this session reporting `EOF while parsing a value at
line 1 column 0` on a lock's owner record.

## The lock probe carries the identity of the process that makes it

Question: what a cache opening writes to learn whether its volume can express
advisory locking.

Options: one shared probe name; a probe named for this process.

Chosen: a probe named for this process, removed as soon as it is answered.

Because: a shared name is a file every process creates, locks, and removes under
the others. With eight processes opening one cache at once, the acquisition's
identity check saw the name replaced under it on attempt after attempt and gave
up, failing a run for a reason that had nothing to do with the digest it wanted.
A probe is a question about the volume, so nothing is gained by sharing it.

Costs: a killed process leaves a zero-length probe file behind, which nothing
reads and `cache clear` removes.

Uncertain: nothing.

Sources: the same failing race in this session.

## A verification that quarantined something exits eighty

Question: what `cache verify` exits with when it moved an object to quarantine.

Options: zero, because the command did what it was asked; the cache code,
because the cache turned out to hold something that had failed verification.

Chosen: the cache code, eighty. Nothing quarantined exits zero.

Because: contracts.md maps `cache.corrupt` to the cache layer and the layer to
the code, and verify reports each mismatch as `cache.corrupt`. A script that
verifies a cache and checks the exit code is asking whether the cache is sound,
and a run that found and quarantined damage should not answer yes.

Costs: a run that quarantines one object out of a million exits non-zero, which
reads as a failure of the command rather than a finding about the cache. The
result names the count either way.

Uncertain: nothing.

Sources: contracts.md Errors, Exit codes, and the Cache paragraph on verify.

## Phase 1 gate

Question: Does phase 1 meet its exit criteria, and what remains unproven.

Options: Close it; hold it open until continuous integration can run again.

Chosen: Closed on its exit criterion, with the verification gap named exactly
rather than described as a whole. The store is built, its invariants are tested,
and the criterion has been met on five of the six targets. Continuous
integration then stopped for a reason outside the repository, and the last three
commits have run nowhere but one Windows machine.

Because: the roadmap's criterion is that the store survives a thousand random
kills under concurrent load with zero invalid objects and zero orphans after
recovery. That test runs eight writers at a time, kills each at a named point in
its work, checks after every batch that every object hashes to the name it is
stored under, and finishes by rewriting the boot generation and asserting that
recovery leaves nothing in `partial/` or `staging/`. It passed on
`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`
and `aarch64-apple-darwin` in the run at `ca62927`, and passes locally on
`x86_64-pc-windows-msvc`.

What the run at `ca62927` proved, which is the last run that executed: nine of
eleven jobs green, including every Apple and Linux target, the lint, format,
comment and dependency gates, and the benchmark gate. The two Windows targets
failed on one test, case folding on a case-sensitive directory, which was a real
defect: the capability answer was memoized per volume while case sensitivity is
a property of a directory on NTFS and on ext4, so whichever directory a process
probed first decided the answer every other directory on that volume received.
It is fixed in `90a6334` and the test passes on this machine.

What the filesystem runners proved, which is the phase 0 debt this phase was
asked to pay. Block cloning succeeded for the first time on any machine: on ReFS
through a Dev Drive on both Windows targets, and on btrfs and reflink XFS on both
Linux targets. A network-backed volume is exercised against a real loopback NFS
mount, a volume with no ownership and no sparse support against a FAT image, a
volume with no room against a sixteen mebibyte image, a read-only cache against
a read-only mount, and tmpfs through `/dev/shm`. macOS exercises APFS,
case-sensitive APFS and HFS+ through sparse disk images. Of the fourteen named
skips phase 0 carried, four remain, and two of those four were never skips: they
are the child processes the race and the kill loop spawn.

What the benchmarks say. A warm cache is measurably faster than a cold one on
every platform that has run: on this machine 307 ms against 1995 ms for sixteen
mebibytes in sixty-four files. The deterministic metric is stronger than the
timing one and is the same statement as never starting a second transfer: a cold
run grows the cache by exactly the corpus and a warm run grows it by exactly
zero.

Costs, and they are the honest gap:

Continuous integration stopped after `9efb4a4` and every job since has failed in
two seconds without starting, with the message that the account's recent
payments have failed or its spending limit needs raising. Three commits have
therefore run on no machine but this one: `90a6334`, which fixes the per-volume
memo, makes a zero length reservation a success rather than a resource failure,
and stops a test comparing a timestamp; `9efb4a4`, which deletes a skip; and
`32dd639`, which lints each platform's own code and removes what that found.

What was done instead of running them. Every target whose toolchain this machine
has was compile-checked and linted: `x86_64-pc-windows-msvc` builds and runs its
whole suite, and `x86_64-unknown-linux-musl` and `aarch64-apple-darwin` are
checked and linted clean with all targets. That is what caught the last set of
findings, because the lint job had only ever run for the host target, so code
behind another platform's configuration was compiled by its build job and never
linted. Linting the Apple target found five denied casts in code phase 0 wrote
and three constants nothing reads.

What this cannot substitute for, and must be run when continuous integration
returns:

The thousand kills, the eight-process race, and the whole cache suite on Linux
and macOS at the current commit rather than at `ca62927`. They passed there and
nothing since changes the store, but that is an argument rather than a run.

The two Windows targets at the current commit. The case folding defect they
found is fixed and verified here; no Windows runner has confirmed it.

Every filesystem the runners build, at the current commit. The per-volume memo
fix changes what a probe answers for a directory, which is precisely what those
runners exist to check.

The timing baselines, which are per runner and per target and have never been
recorded on any of the six.

What remains unproven regardless of continuous integration:

Locking across two users still needs a second account, and a volume whose
locking fails with `ENOLCK` or `EOPNOTSUPP` still needs a filesystem no runner
builds. Both remain named skips.

A FUSE mount is reported as unknown backing rather than as network, which no
runner builds.

The on-access scanner threshold is still the provisional ratio of two, and a
loopback ext4 image measured a ratio near a thousand with no scanner present,
which means the reported capability is wrong on that volume. The measurement
cannot distinguish a scanner from a slow filesystem, and deciding what it should
report instead is a contract question rather than an implementation one.

An object referenced by a lock in the working directory is contracted to survive
prune. No build writes a lock, so the rule has no subject until phase 4 and its
test is written there.

Uncertain: whether the case folding fix is complete. It makes folding and
normalization per directory and leaves the rest of the answer memoized per
volume, which is right for clone, sparse, hard links and backing. If any other
row turns out to be per directory on some filesystem, the same defect returns
for that row.

Sources: the runs at `ca62927` and `9efb4a4`; the local suite at the current
commit; `cargo clippy --workspace --all-targets` for
`x86_64-pc-windows-msvc`, `x86_64-unknown-linux-musl` and
`aarch64-apple-darwin`; `cargo xtask bench --compare` on this machine.

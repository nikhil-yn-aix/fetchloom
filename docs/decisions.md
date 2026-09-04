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

## The verification matrix runs on one machine, and names what it cannot reach

Question: All six targets were built and tested by GitHub Actions. The account
cannot run it and it is not coming back. What verifies the six targets now.

Options: another hosted service; self-hosted hardware; a local matrix built from
what one Windows machine plus a container runtime can reach; accept that only
the host target is verified.

Chosen: a local matrix, `cargo xtask verify`, and an explicit statement in its
own output of every target it did not prove. The workflow file is deleted. The
three volume scripts move to `verify/`, because they provision filesystems and
were never workflow steps.

| Lane | Targets | What runs |
|---|---|---|
| Native | `x86_64-pc-windows-msvc` | fmt, clippy over the three lint targets, build, the whole suite against the filesystems `verify/volumes-windows.ps1` builds, check-comments, `bench --compare` |
| Container | `x86_64-unknown-linux-musl`, `x86_64-unknown-linux-gnu` | clippy, build, the whole suite against the loopback images `verify/volumes-linux.sh` builds inside a privileged container |
| Emulated | `aarch64-unknown-linux-musl`, `aarch64-unknown-linux-gnu` | The container lane again under qemu binfmt, behind `--arm`, because it is slow |
| Compiled only | `aarch64-apple-darwin` | clippy and build, and a degrade-shaped line in the output saying macOS was not run |

Because: the two properties that decide correctness here are what a real
filesystem does and what a real kernel does, and both survive a container and
neither survives a cross-compilation check. A privileged container can build and
mount btrfs, XFS, FAT, ext4 and a read-only image over loopback, which is every
Linux filesystem the deleted workflow built, so the Linux half of the matrix
loses nothing but the runner. macOS has no such answer on this machine and
inventing one would be worse than saying so, which is why the tool says so in
every run rather than in a document nobody reads during a change.

The target directory inside the container is a named volume rather than the
bind-mounted one, because a Linux `target/` and a Windows `target/` sharing a
directory invalidate each other on every alternation and turn a warm build into
a cold one.

The Linux lane adds `x86_64-unknown-linux-gnu`, which the workflow never built.
It costs one extra build in a lane that is already warm and it is the target
every Linux user actually runs.

NFS is lost, and it is named rather than quietly dropped. The volume script
exported a loopback NFS mount through `nfs-kernel-server`, which needs systemd
to start `rpcbind`, `nfsd` and `mountd`. Those daemons can be started directly
in a container, but only with kernel modules loaded from the host and a running
`rpcbind`, and the result would be a network-backed volume proven on a machine
whose kernel is the container host's, which is a weaker statement than the same
mount on a runner. What the network-backed row needs is a volume that reports a
network magic number, and no such volume exists in this matrix. The row is
therefore unproven from here, and the same is true of the conservative locking
path a network volume selects.

The hook. `cargo xtask verify --install-hook` writes a `pre-push` hook running
fmt, clippy over the three lint targets, the native suite and check-comments. The
full matrix is minutes; the hook is the part that is fast enough to be run every
time, and it exists because nothing else now stands between a broken change and
`main`.

Timing baselines. The standards say a timing gate runs only on continuous
integration, against a baseline from that runner. There is no runner, so the
gate arms when `cargo xtask verify` sets `FETCHLOOM_VERIFY`, which names the one
context in which this machine is measuring rather than being used. A baseline
stays per target, and it is now also per machine, which is what it always was in
substance.

Costs: five of six targets lose their native runner. Two of them, the Apple
pair, lose everything but a compile. Every timing number in the repository is
now a number from one desktop, and comparing it to anything else is invalid.

Uncertain: whether a privileged container on Docker Desktop can create loopback
devices and mount btrfs and XFS. That is the first thing step 1 has to prove and
the reason step 2 does not start until it has.

Sources: the deleted workflow at `04c3c14`; `verify/volumes-linux.sh`;
standards.md Measure; the phase 1 gate record for what stopped running.

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
| On-access scanner | enumerate via `FilterFindFirst`, and measure the cost | measure the cost only | measure the cost only |

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

The on-access scanner is required by contracts.md to be reported with "the measured cost", so a measurement has to happen regardless of what any enumeration API says. Writing many small files in staging and comparing against writing the same bytes to one file is that measurement, and it is the same shape as the many-small-files benchmark regime. `FilterFindFirst` adds the enumeration and the product name, which `doctor` needs in order to say which exclusion the user may configure; Microsoft's altitude allocation puts anti-virus minifilters in 320000 to 329998 and activity monitors, where endpoint detection products sit, in 360000 to 389999. The call has no side effects and requires elevation, so an ordinary run enumerates nothing.

Costs: the empirical probes create and delete a handful of files per volume per process, which the no-op benchmark regime has to absorb, so they must be lazy and a command that never touches a volume must never probe it. A probe answers for the staging directory, and on Windows per-directory case sensitivity means the destination directory could in principle differ; the collision check catches that anyway, because it runs on the real paths. Probing means a plan produced without staging cannot report folding behavior, so `--offline` plans mark it unknown.

Uncertain, and thin enough to say so rather than choose:

The macOS `statfs` `MNT_LOCAL` bit and the Windows `GetDriveTypeW` `DRIVE_REMOTE` value were not verified against primary documentation in this session. Both are widely used for this purpose, but the network-backed row for those two platforms should be confirmed before it is implemented.

Whether `is_aarch64_feature_detected!` returns a useful answer on `aarch64-pc-windows-msvc` was not verified; `std_detect`'s aarch64 backend has historically been Linux and Apple only. If it does not, that row reports the extension as undetectable rather than as present and unused.

The cost ratio above which small writes are called expensive is not chosen here. What it may be used to conclude is settled separately, and it concludes nothing about whether a scanner is present.

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

## What a ratio can and cannot say about an on-access scanner

Question: The capability detection record decided a scanner is present on macOS
and Linux when writing many small files costs more than twice what writing the
same bytes to one file costs. Is that measurement evidence of a scanner.

Options: keep the ratio as a detection threshold and measure a better value;
report every volume as unscanned where nothing can be enumerated; separate the
cost from the cause and give three answers instead of two.

Chosen: three answers. Present carries a product name and the measured ratio and
is reported only where the platform enumerated what inspects a write. Absent
carries nothing. Unknown carries the measured ratio and is reported where the
platform cannot enumerate and the ratio is above the cost threshold, with a
`degrade` naming the ratio, the answer left unknown, and the reason. The
threshold stays at two, stays in one named constant, and is a cost threshold. No
later slice turns it into a truth value, because no value of it could be one.

Because: a loopback ext4 image on a continuous integration runner measured a
ratio near a thousand with no scanner loaded. The measurement cannot separate a
scanner from a filesystem that is slow at small writes, so a present answer
derived from it is a false statement about the machine, which is the kind of
defect this project treats as equal to corruption. The contract requires the
cost be reported, and the cost is what the measurement actually produces, so the
cost is what it now reports.

What Windows can enumerate, and what it costs. `FilterFindFirst` lists every
registered filter driver with its name and altitude, which is exactly the
contract's question: not whether an anti-virus product is installed, but whether
something inspects a write. It fails with `HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)`
in a process that is not elevated, measured on this machine at `0x80070005`, so
an ordinary run enumerates nothing and reports unknown. An elevated run reports
present or absent. The enumeration is also corrected here to grow its buffer on
`ERROR_INSUFFICIENT_BUFFER` and to read `ERROR_NO_MORE_ITEMS` as an empty list
rather than as a failure, because the previous code returned an empty list for
every failure and therefore reported absent on every unelevated Windows machine.

Why not the Security Center. `IWSCProductList` enumerates registered anti-virus
products without elevation and gives each product's display name. It is a COM
interface. `windows-sys` ships the `WSCProductList` class identifier and the
provider constants but no interface vtable, so using it means either the
`windows` crate, which is a far larger dependency than this one answer justifies
against deny.toml's allow list, or hand-written COM vtables and unsafe calls for
a capability report. It also answers a different question: a registered product
is not the same fact as a filter inspecting this write, and the Security Center
service does not exist on Windows Server, where the answer would silently become
absent. Windows therefore reports unknown when it cannot enumerate, and says so.

Costs: an ordinary unelevated Windows run now reports unknown where it used to
report absent, so the honest answer is less useful than the wrong one was.
`doctor` states the cost and names the exclusion regardless, because the cost is
measured in every case.

Uncertain: nothing about the ratio, which is no longer load-bearing. Whether an
elevated Windows run finds Defender at an altitude inside the allocated scanner
ranges has not been observed on this machine, because enumeration has only been
run unelevated here.

Sources: `FilterFindFirst` on Microsoft Learn for the documented return values;
`IWSCProductList` and `IWSCProductList::Initialize` on Microsoft Learn;
`windows-sys` 0.61.2 `Win32::System::SecurityCenter`, which contains the class
identifier and `WscGetSecurityProviderHealth` and no interface; a direct call to
`FilterFindFirst` from an unelevated process on this machine returning
`0x80070005`; the phase 1 gate record for the ext4 measurement.

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

## What this machine could not verify, and where it is verified now

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

Where each of these is verified now, since there are no runners. The verification matrix record names the lanes. The two musl targets, `x86_64-unknown-linux-gnu`, and every Linux execution entry above are covered by the container lane, which links and runs them. The aarch64 Linux pair is covered by the same lane under emulation behind `--arm`, which proves the code and not the machine. Every filesystem entry that names ext4, btrfs, XFS, FAT or tmpfs is covered by the images `verify/volumes-linux.sh` builds inside that container.

Permanently unproven from this machine, and stated as such rather than deferred. Nothing on macOS is executed: `aarch64-apple-darwin` is compiled and linted only, `x86_64-apple-darwin` is not installed and is not checked, the NEON build is never run, APFS in either form and HFS+ are never mounted, and every Apple constant confirmed against the C library's source stays confirmed only there. A network-backed volume is not built, so the network magic set, the conservative locking path and the FUSE row stay unproven. Locking across two users stays unproven. The two Windows on ARM targets have no machine here at all.

The scanner entry is settled rather than deferred: the ratio is no longer a detection threshold, and what it may conclude is in its own record.

The timing baseline entry is settled rather than deferred: there are no runners to record one per, so the baseline is per target and per machine and the gate arms only under `cargo xtask verify`.

Because: a list of what was not verified is worth more than a claim about what was, and a judge session that has to derive this list will derive a shorter one. Every item here is a place where a green run on this machine means nothing.

Costs: the list is shorter than it was, and the part that remains no longer has a date on it. Some of it is now permanent rather than pending.

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

## What the platform seam leaves for the verification matrix

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

The scanner measurement runs here and reports, and the filter driver enumeration has never succeeded, because it needs elevation. Whether it names a real product is unproven, and only an elevated run can prove it.

The volume capability query on macOS reads a reply at an offset the kernel chooses and trusts the valid array before believing a capability. Neither the parse nor the gating has run.

Every ignored test names the filesystem it needs. Nine are in the capability suite: a block-cloning volume, a reflink volume, APFS, case-insensitive APFS, HFS+ normalization, a case-folded ext4 directory, a network share, a FUSE mount reported as unknown rather than network, and a volume without sparse support. Four are in the locking suite: locking across users, a network volume refused for a shared cache, and a filesystem whose locking fails in the way that must produce the unsupported error. Those names are the list a runner has to satisfy.

Because: the earlier record could only say that everything was unverified because nothing existed. Naming what a Windows machine did prove, and what it structurally cannot, is what tells a judge session where to look.

Where each of these is verified now. The container lane runs every line of the Linux module and the loopback images prove the Linux clone path, ext4 case folding, the sparse and no-sparse rows and the small-volume row. The Apple module, its constants, its process start time record and its volume capability parse are compiled and linted only, and are permanently unproven from this machine. Block cloning is proven on btrfs and reflink XFS by the container lane; ReFS and APFS cloning remain unproven, because this machine has no ReFS volume and no Mac. The four locking skips and the network share skip have no volume in this matrix and are permanently unproven from here.

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

What this record's green run means now. The eleven jobs it names ran and their
result stands as recorded; nothing re-runs them. What re-establishes the same
statement at a later commit is `cargo xtask verify`, which covers `x86_64`
Windows and the four Linux targets and covers no Apple target beyond a compile.
The two Windows on ARM targets and both Apple targets are green at `ca62927`
and at no commit since.

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

What this cannot substitute for. Continuous integration is not returning, so
each of these is answered by the verification matrix or by nothing:

The thousand kills, the eight-process race, and the whole cache suite on Linux
at the current commit. The container lane runs them. On macOS they are proven
nowhere but `ca62927` and stay that way.

`x86_64-pc-windows-msvc` at the current commit, which the native lane runs. The
case folding defect is fixed and verified there. `aarch64-pc-windows-msvc` has
no machine here and stays proven only at `ca62927`.

Every filesystem the scripts build, at the current commit. The Linux images and
the Windows Dev Drive are built by the two running lanes. The macOS disk images
are built by nothing.

The timing baselines, which are now per target and per machine and are recorded
by this matrix for the first time.

What remains unproven regardless of any of it:

Locking across two users still needs a second account, and a volume whose
locking fails with `ENOLCK` or `EOPNOTSUPP` still needs a filesystem this matrix
does not build. Both remain named skips, now permanently.

A FUSE mount is reported as unknown backing rather than as network, and nothing
here builds one. So is a network-backed volume of any kind, because the NFS
export did not survive the move into a container.

The on-access scanner answer is settled. The ratio is a cost and never a
detection, an unknown answer exists and degrades, and the record naming that is
its own.

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

## A probe name is unique per probe, because the name is the question

Question: Every capability probe used a fixed name. The first Linux run of the
suite failed with `fetchloom-probe-many: File exists`. What is the right name.

Options: keep fixed names and serialize probes behind a lock; give each probe a
name no other probe uses.

Chosen: a name no other probe uses. Every probe name now carries this process's
identifier and a counter that never repeats within it.

Because: the failure is the visible half of a correctness defect. A probe asks
the filesystem a question by creating a name and then attempting a second name
that differs only in case or in normalization, and it reads an already-exists
result as the filesystem's answer. If another probe holds that name, the
already-exists comes from the other probe rather than from the filesystem, and a
case-sensitive volume is reported as case-folding. Two Fetchloom processes
sharing a cache probe the same staging directory, so this is not confined to a
test binary running its tests in parallel. Serializing behind a lock would fix
the collision inside one process and leave the cross-process case, which is the
one that matters.

Costs: nothing measurable. The names are longer and are still removed.

Uncertain: nothing. The uniqueness is per process identifier and per counter,
and two processes with the same identifier on one machine at one time do not
exist.

Sources: the first `x86_64-unknown-linux-musl` run of the platform suite in the
container lane; `crates/platform/tests/capability.rs` lines 312 and 412, the two
tests that probe a volume root at the same moment.

## A volume that refuses the probe name reports normalization as unknown

Question: The normalization probe creates a name containing U+00E9. A FAT volume
refuses it with `EINVAL`, so `volume_capabilities` failed and Fetchloom could
not use the volume at all. contracts.md gave normalization three values and was
silent on this.

Options: report sensitive with a degrade; fail the volume; add a fourth value.

Chosen: a fourth value, unknown, carrying no claim, with a `degrade` naming that
the volume refused the probe name and the reason it gave.

Because: the other two answers are both false statements. Sensitive says two
spellings are two names, which is a measurement that did not happen. Failing the
volume says a FAT volume cannot hold a destination, which is not true and which
contradicts the suite that uses a FAT image for the no-ownership and no-sparse
rows. This is the same shape as the scanner answer decided in this phase: where
the platform cannot answer, the report says so rather than picking the
comfortable value.

Costs: a fourth value every consumer of the capability must handle, and a
destination on such a volume whose collision behavior for two spellings is not
known in advance. The collision check runs on the real names at extraction time
and catches it there.

Uncertain: whether any volume other than FAT refuses the name. The answer is
measured per volume, so it does not need to be enumerated.

Sources: the container lane running `sparse_support_is_reported_where_it_is_absent`
against `/mnt/fetchloom/fat`; contracts.md Platform capabilities.

## The HTTP client is ureq, and the reason is the seam rather than taste

Question: Which blocking HTTPS client. The Source seam's `Body` is
`std::io::Read` and phase 0 chose rayon as the one explicit pool, so no async
runtime may enter this workspace, transitively or at runtime.

Options: `ureq`, `attohttpc`, `minreq`, `curl`, `isahc`, `reqwest` with its
`blocking` feature.

Chosen: `ureq` 3.4.0, with `rustls` and `rustls-platform-verifier`.

Because, against the requirements one at a time:

No runtime. `reqwest`'s `blocking` client is disqualified outright: `tokio` is a
mandatory dependency regardless of the feature, and the blocking client spawns
an operating system thread named `reqwest-internal-sync-runtime` running a
current-thread tokio runtime, then blocks the caller on a channel to it. That is
an async runtime inside the process, which is exactly what phase 0 forbade.
`isahc` is disqualified for the same reason in a different shape: it is
async-first over libcurl and carries `async-channel`, `futures-lite`, `polling`,
`waker-fn` and `event-listener` as required dependencies, driving its own
executor per blocking call.

Timeouts. Contracts need a per-request timeout separate from a connect timeout,
because a stalled body and an unreachable host are different failures with
different retry answers. `minreq` has one `with_timeout` covering everything and
is disqualified on that alone. `ureq` splits the question further than the
contract needs: `timeout_connect`, `timeout_resolve`, `timeout_send_request`,
`timeout_send_body`, `timeout_recv_response`, `timeout_recv_body`,
`timeout_per_call` and `timeout_global`.

Redirects. The contract follows ten and drops credentials across hosts, so a
client that only counts hops is not enough. `ureq` takes `max_redirects(0)` to
follow none, and `save_redirect_history` records every hop, which is what lets
the drop be decided and reported rather than assumed. `minreq` follows redirects
recursively and warns in its own documentation that a high limit risks a stack
overflow, which is a second reason to refuse it.

Connection reuse. The standards require one pool per host with keep-alive and
ban opening a connection per request. `ureq`'s `Agent` holds the pool and is
shared. `attohttpc` is refused here: nothing in its documentation states that it
pools connections at all, and a client that might open a connection per request
cannot satisfy a rule written to forbid exactly that.

Streaming. `ureq` bodies read as `std::io::Read`, which is the seam's type
already, so nothing wraps or buffers.

Header access. `ureq` returns `http::Response`, so `ETag`, `Last-Modified`,
`Retry-After`, `Content-Range` and `Accept-Ranges` are read directly.

Plain HTTP. `ureq` speaks `http://`, which means the adversarial server does not
need TLS and the transfer logic is tested over plain HTTP with TLS tested
separately and narrowly.

Why not `curl`. It is the only remaining candidate that clears the runtime bar,
and it is refused for build reasons rather than API reasons. It is a C library
reached through FFI, so every target needs a C toolchain for it. This machine
has none for `aarch64-apple-darwin`, which is exactly the lane that is already
reduced to a lint, and a statically linked musl build would have to vendor and
build libcurl inside the container lane as well. Its TLS backend is also decided
by how the linked libcurl was built rather than by this workspace, which makes
the trust store answer below unstatable. Its API is otherwise the most capable
of the group and this is not a judgement about the library.

Costs: `ureq` adds `base64`, `log`, `percent-encoding`, `ureq-proto`,
`utf8-zero`, `http`, `rustls`, `rustls-pki-types`, `rustls-platform-verifier`
and their own dependencies. Every one of them is added to deny.toml with its
reason, and `multiple-versions` stays denied, so a duplicate pulled in later is
a build failure rather than a surprise.

Uncertain: whether `ureq` reads the Windows registry or the macOS system
preferences for proxy configuration. The evidence found says it reads
`HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY` and nothing more. See the proxy
record.

Sources: crates.io for every version and publish date checked in this session;
`docs.rs/ureq/3.4.0` for the configuration builder; `reqwest`'s
`src/blocking/client.rs` on GitHub for the spawned runtime;
`crates.io/api/v1/crates/isahc/2.0.1/dependencies`;
`crates.io/api/v1/crates/minreq/3.0.0/dependencies`; `docs.rs/minreq/3.0.0`;
`docs.rs/curl/0.4.50/curl/easy/struct.Easy.html`.

## Trust comes from the platform, and bundled roots are the fallback that says so

Question: Whether TLS trust is a bundled root set compiled into the binary or
the platform's own trust store. Contracts name "how platform trust stores and
proxies are read" as a requirement, so bundled-only has to be argued rather than
assumed.

Options: `webpki-roots`, a compiled-in mirror of Mozilla's root program;
`rustls-native-certs`, which loads the platform's roots once at startup;
`rustls-platform-verifier`, which hands each certificate to the platform to
verify.

Chosen: `rustls-platform-verifier` 0.7.0. There is no bundled root set in the
binary and no fallback to one.

Because: a bundled root set is wrong for this program in a way that is not a
preference. It contains public roots only, so a machine behind a corporate
inspecting proxy fails every transfer while every other tool on that machine
works, and the user has no action available. It cannot see a root an
administrator installed. It carries no revocation, because revocation is a
platform service and a compiled-in list is a snapshot. And it goes stale with
the binary rather than with the machine.

`rustls-native-certs` is refused as a half measure: it copies the platform's
roots into a list at startup and then verifies locally, so it inherits the
snapshot problem, sees no revocation, and its own maintainers now point at the
platform verifier instead.

`rustls-platform-verifier` delegates verification itself: the certificate
verification interface on Windows, `Security.framework` on macOS, and on Linux
the system bundle read through `rustls-native-certs` and `openssl-probe`,
because Linux has no verification service to delegate to. That last case is a
degradation in kind rather than in configuration and is stated here rather than
reported at runtime, because it is a property of the platform and not of the
run.

Costs: the platform verifier's dependencies are platform specific,
`core-foundation` on macOS and `windows-sys` on Windows, which is a crate the
workspace already carries. A verification failure now depends on machine
configuration, so the same run can succeed on one machine and fail on another,
and the error must say which trust store refused rather than saying the
certificate is invalid.

Uncertain: nothing about the choice. The Linux path's exact behavior when no
system bundle exists is unverified and is a question for the first container run
that makes a TLS connection.

Sources: `github.com/rustls/rustls-platform-verifier`;
`github.com/rustls/rustls-native-certs`, including its own recommendation;
`crates.io/crates/webpki-roots` and the root program it mirrors; contracts.md
Platform capabilities and the phase 2 Decide column.

## One pool per host, kept alive, and never a connection per request

Question: What the connection pool holds and when a connection is closed.

Options: a pool per process; a pool per host; a connection per request.

Chosen: one `ureq` agent per host, holding that host's pool, with keep-alive on
and no idle connection kept past sixty seconds. A connection is never opened per
request. The pool holds at most as many connections to one host as the
politeness ceiling permits, because a pooled connection above that ceiling is a
connection the ceiling said not to make.

Because: the standards already state it. What is decided here is only the shape:
per host rather than per process, because the ceiling, the measured throughput
and the rate-limit response are all per host, and a pool that spans hosts cannot
answer any of them. Sixty seconds of idle is the same number as the retry
ceiling, and it is chosen so that a resumed transfer after the longest permitted
wait still finds its connection rather than paying a handshake.

Costs: a run touching many hosts holds many pools. The bound is the number of
hosts in the plan, which is small, and each pool is bounded by the ceiling.

Uncertain: nothing that is not measured in phase 6, which replaces the fixed
ceiling with a discovered one.

Sources: standards.md Network; contracts.md Source selection.

## Which failures are transient, and what a server asking for an hour means

Question: Which failures are retried, which are terminal, how server retry
guidance is honored, and what the backoff is.

Options: retry on a list of status codes; retry on everything that is not a
client error; retry only what the specification says is safe.

Chosen, and every request Fetchloom makes is `GET` or `HEAD`, which are
idempotent, so the specification's automatic-retry rule permits retrying any of
them after a communication failure:

| Failure | Answer |
|---|---|
| Connection refused, reset, or closed before any response | Transient |
| Connection closed part way through a body | Transient, and resumed by range rather than restarted |
| Name resolution failure | Transient |
| Read or write timeout | Transient |
| Certificate rejected by the trust store | Terminal |
| 408, 429 | Transient |
| 500, 502, 503, 504 | Transient |
| 501 | Terminal |
| Every other 4xx, including 400, 401, 403, 404, 410 | Terminal |
| 416 | Terminal for the range asked for; the length is reread from the response and the request is remade once |

Backoff is exponential with full jitter: the wait before attempt number `n` is a
random duration between zero and `min(2^(n-1) seconds, 60 seconds)`. Attempts
stop at five, both of which are already in contracts.

Server guidance wins where it exists. `Retry-After` is honored on 429, on 503
and on any 3xx, in both its forms, and it is a floor on the wait rather than a
replacement for it: the wait is the longer of the computed backoff and the wait
the source asked for, so a source asking for less than the run's own policy is
not obeyed into being hammered. A `Retry-After` longer than the sixty second ceiling is not
waited out: the source is left, the next source in order is tried, and if none
remains the run fails with `network.status` reporting the wait that was asked
for. Waiting an hour is a decision for a person, not for a program that was told
its ceiling is a minute.

Because: full jitter rather than a fixed multiple is what stops a rate-limit
storm from becoming a synchronized second storm, which is the failure the
adversarial suite reproduces. `501` is separated from the rest of the 5xx family
because it says the server does not implement the thing, which no amount of
waiting changes. `416` is separated because the correct response is not to wait
but to reread the length the server reported and ask again, once, which is the
one case where a terminal status leads to another request. A certificate
rejection is terminal because retrying it is how a program teaches a user to
ignore it.

Costs: five attempts with a ceiling of sixty seconds means a worst case of
roughly two minutes of waiting per source before failover, and a run against
five dead sources spends ten. The alternative is a smaller ceiling, which fails
a source that asked for a legitimate minute.

Uncertain: whether the `RateLimit` header fields should be read. They are an
Internet-Draft, at revision eleven as of May 2026, not an RFC, and a draft that
can be renamed is not something a contract may promise. They are not read. Phase
6 revisits it if it is published.

Sources: RFC 9110 sections 9.2.2, 15.5.9, 15.5.17, 15.6.4 and 10.2.3; RFC 6585
section 4 for 429; `draft-ietf-httpapi-ratelimit-headers-11`, 2026-05-23;
contracts.md Limits.

## Cross-host means a different origin, and the Public Suffix List is not used

Question: Contracts drop a credential on a redirect to a different host. What
counts as different.

Options: a different hostname; a different registrable domain, which needs the
Public Suffix List; a different origin, meaning scheme, host or port.

Chosen: a different origin. A credential is dropped when the scheme, the host or
the port of the redirect target differs from the request the credential was
resolved for. The Public Suffix List is not a dependency and is never consulted.

Because: this is the rule the web platform's own fetch algorithm applies, and it
is the strictest of the three. The registrable-domain answer requires the Public
Suffix List, whose own specification says it "cannot be relied-upon to provide a
hard security boundary, as the public suffix list will diverge from client to
client", and whose private section is self-reported. A security decision made
from a list that differs between two builds of the same program is not a
decision. The hostname answer is what curl does, and it has a specific hole this
project cannot accept: a redirect from `https://host/a` to `http://host/b` is
the same host, so the credential survives, and is then sent in clear text. Port
is included for the same reason: a different port is a different service.

Costs: stricter than curl, so a deployment redirecting between two ports or two
schemes of one host loses its credential and gets `policy.credential_missing`
with the drop reported. That is a visible failure with a stated cause rather
than a silent leak.

Uncertain: nothing. The rule is mechanical and testable, and the test asserts on
the bytes of stdout, stderr and the event stream rather than on a redaction
function.

Sources: the Fetch Standard, HTTP-redirect fetch, and its CORS non-wildcard
request-header name set, which contains exactly `Authorization`; the HTML
Standard's same origin definition; the URL Standard on registrable domain and
its own caveat; `curl.se/libcurl/c/CURLOPT_FOLLOWLOCATION.html`; RFC 9110
section 15.4; contracts.md Credentials.

## What a partial file records about where its bytes came from

Question: Contracts say resume never appends to a partial whose recorded source
identity differs from the current response. What is recorded, and where.

Options: a header inside the partial file; a record beside it; a row in cache
metadata.

Chosen: a record beside it, at `partial/<name>.source`, written when the partial
is opened and removed with it. This is the same shape the partial store already
uses for its owner record, so the recovery sweep that removes an orphaned
partial removes this with it and nothing new has to know about it.

The record holds the redacted location, the host, the length the source stated,
what the source said identifies the bytes, the entity tag and the last modified
value as they were received, whether the source accepted ranges, and the rung
the transfer was on. Nothing in it is a secret, because the location is redacted
at construction, which is where redaction happens everywhere else.

Resume reads the record, probes the source, and compares. The ladder then reads:

| Rung | Condition | Request |
|---|---|---|
| 1 | An outboard tree is known for the expected digest | Verify what is on disk by range, then range from the first bad or missing chunk |
| 2 | The recorded identity is a content address or an immutable version, and it is unchanged | Range from the end of the partial |
| 3 | The recorded entity tag is strong and unchanged | Range from the end of the partial, sent with `If-Range` carrying that tag |
| 4 | Only a weak validator or a last modified value, unchanged | Range from the end of the partial, sent without `If-Range`, full verify at completion, quarantine on mismatch |
| 5 | Nothing identifies the bytes, or anything recorded has changed | Discard the partial and start from zero, reporting why |

`If-Range` carries the strong entity tag only. The specification forbids a weak
entity tag in `If-Range` outright, and permits a date there only when the date
can be shown to be a strong validator, which needs a `Date` at least a second
later from a clock the client has reason to trust. That is a condition this
program cannot establish, so rung four sends no `If-Range` at all and pays for it
with a full verification at the end, which contracts already require.

A response to a range request that arrives as 200 rather than 206 means the
server ignored the range. The partial is discarded and the transfer restarts,
reporting the rung it fell to, because the specification gives a client no other
signal that this happened and the bytes arriving are the whole object.

Because: the record has to survive a crash and a reboot and has to be readable
without the object, which rules out a header inside the partial. Cache metadata
would work and would put a per-transfer fact in a store the contract describes as
per host and per resolution.

Costs: one more small file per in-flight transfer, and one more file the recovery
sweep removes.

Uncertain: nothing about the shape. Whether a real source's weak validator is
stable enough for rung four to be worth having over rung five is a measurement
phase 6 can make and this phase cannot.

Sources: RFC 9110 sections 8.8.1, 8.8.2.2, 8.8.3, 13.1.5 and 14.2; RFC 9111
section 3.4 on combining partial content only under a shared strong validator;
contracts.md Resume ladder and Cache.

## Fixed defaults, chosen honestly, because nothing has been measured

Question: Politeness ceilings and default concurrency, before any measurement
exists.

Options: pick generous defaults and let phase 6 lower them; pick conservative
defaults and let phase 6 raise them; refuse to pick.

Chosen, all per host, all replaced by measurement in phase 6:

| Setting | Default |
|---|---|
| Connections to one host | 4 |
| Objects transferred at once from one host | 4 |
| Objects transferred at once across all hosts | the thread budget, whichever is smaller |
| Connect timeout | 10 s |
| Response header timeout | 30 s |
| Idle timeout inside a body | 30 s |
| Idle connection kept in the pool | 60 s |

Because: four is the number a client can hold against one host without being the
reason a server slows down, it is the ceiling browsers settled on for the same
reason, and it is low enough that being wrong costs speed rather than goodwill.
The alternative, a generous default lowered later, is wrong in the direction
that damages someone else's service. The thirty second idle timeout inside a
body is what makes a stalled connection a failure rather than a hang, and it is
the number the adversarial suite's stall test is written against.

These are guesses. They are labelled as guesses, they live in one place, and
phase 6 replaces every one of them with a measurement.

Costs: a fast, tolerant host is transferred from more slowly than it could be,
until phase 6.

Uncertain: all seven values. None of them has a measurement behind it and none
of them can have one before there is a client to measure with.

Sources: standards.md Network; contracts.md Limits; roadmap.md phase 6.

## An index is recognized before it is parsed, and never guessed at

Question: Which directory index formats are recognized, and how an unrecognized
one fails.

Options: try each parser until one succeeds; recognize a format by a signature
and refuse everything else.

Chosen: recognize by signature, and fail with `reference.unresolved` when no
signature matches. A parser is never tried speculatively, because a parser that
succeeds on the wrong input produces a listing that is wrong rather than an
error that is right.

| Format | Signature that must match before parsing |
|---|---|
| Object store listing | An XML root element `ListBucketResult` in the namespace `http://s3.amazonaws.com/doc/2006-03-01/` |
| WebDAV | Status 207 and an XML root element `multistatus` in the namespace `DAV:` |
| Generated HTML index | A `text/html` body containing an `h1` whose text begins `Index of ` |

Nothing else is recognized in this phase. FTP is not implemented here, and when
it is, `MLSD` is what is parsed, decided by asking `FEAT` first, because the
classic `LIST` output has no grammar at all and the two common shapes can only
be matched by heuristic.

Because: the first two have specifications and namespace-qualified root
elements, so recognition is exact and cheap. The third has none: a generated
HTML index is the output of a server's own template and is not a wire format, so
what is recognized is deliberately the narrowest thing that is stable across the
two common generators, and it is documented as a heuristic rather than a format.
An HTML body that does not carry that heading fails as unresolved even if it
plainly contains links, because guessing at links is crawling, which contracts
forbid.

Costs: a server whose generated index is themed differently is unresolvable, and
the user is told so rather than given a wrong listing.

Uncertain: whether the `Index of ` heading survives across the versions and
themes of the two generators. It is a heuristic and is named as one in the
error.

Sources: the object store list API reference for the root element and
namespace; RFC 4918 section 9.1 for `PROPFIND`, the 207 status and the
`DAV:multistatus` root; RFC 3659 for `MLSD` and for the fact that `LIST` has no
standard format; the two generators' own module documentation; contracts.md
Directory listing.

## Proxies are read from the environment, and the system configuration is not

Question: Contracts name how proxies are read as a requirement. What is read.

Options: the environment only; the environment plus the platform's own proxy
configuration; nothing.

Chosen: `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY`, which the client reads
already, and nothing else. The Windows registry and the macOS system
preferences are not read in this phase.

Because: reading a platform's proxy configuration is not one question. On
Windows it is a registry value and, more often in a managed network, a proxy
auto-configuration script, which means fetching and evaluating JavaScript. On
macOS it is a system configuration dynamic store with the same script problem.
Neither belongs in this phase, and an incomplete version of either is worse than
none, because a program that reads half of a proxy configuration fails in a way
the user cannot connect to their own settings.

What makes this honest rather than silent: `doctor` states that only the
environment is read, so a user on a managed network is told where the setting
Fetchloom obeys lives. There is no `degrade` for it, because a degrade names a
fallback taken during a run, and this is a capability the build does not have.

Costs: a user behind a system-configured proxy with no environment variables
gets connection failures until they set one. That is the same behavior as most
command line tools and it is stated rather than discovered.

Uncertain: whether the chosen client reads anything beyond those three
variables. The evidence found says it does not.

Sources: `docs.rs/ureq/3.4.0`; `curl.se/libcurl/c/libcurl-env.html`, which
documents the same three variables and states that libcurl has no support for
detecting a system proxy either; contracts.md phase 2 Decide.

## What a source is scored on, and when a transfer leaves one

Question: Contracts fix the scoring inputs and their priority and set the probe
budget at four. What is not yet mechanical is how each input is compared and
what makes a transfer switch.

Options: leave it to the implementation; state the comparison.

Chosen: state it. The inputs are compared in the order contracts fixes them, and
the first that differs decides. Nothing later is consulted.

| Input | How it is compared |
|---|---|
| Reachable | A probe that returned any response beats one that did not |
| Supports ranges | `Accept-Ranges` naming `bytes` beats its absence or `none` |
| Exposes immutable identity | A content address beats an immutable version, which beats a strong tag, which beats a weak one, which beats nothing |
| Recorded throughput for that host | Higher beats lower; a host with no record loses to a host with one |
| Time to first byte | The probe's own measurement, lower wins |
| Egress cost | Lower wins; unknown loses to known-free and beats known-paid |
| Remaining politeness headroom | More free connections under the ceiling wins |
| Manifest order | Earlier wins, so equal measurements are deterministic |

At most four candidates are probed, in manifest order, in parallel. A fifth
source is never probed unless one of the four failed to answer at all.

A transfer leaves its source when the body is idle past the idle timeout, when
its retries are spent, when it returns a terminal status, or when its measured
throughput stays below one quarter of what the probe measured for a continuous
thirty seconds. Verified bytes are kept, the identity record is rewritten for
the new source, and the rung is recomputed against it, which will usually be
rung five unless the new source offers the same content address.

Because: a quarter for thirty seconds is chosen so that a transient dip does not
cost a failover, which is itself expensive, and so that a source that has quietly
died at ten percent of its measured rate is left rather than waited on. Both
numbers are guesses of the same kind as the politeness ceilings and phase 6
replaces them.

Costs: a switch throws away the connection and the pool it warmed.

Uncertain: the quarter and the thirty seconds. Neither is measured.

Sources: contracts.md Source selection and Limits; standards.md Network.


## What the C in the cryptography costs the compile-only lanes

Question: The client's TLS is performed by `ring`, which is C. The host lane
cross-compiled `x86_64-unknown-linux-musl` and `aarch64-apple-darwin` as a lint
and a compile check. Both now fail in `ring`'s build script, because this
machine cross-compiles no C at all.

Options: drop the two checks; find a cross C toolchain; narrow each check to
what can still be compiled and say what was dropped.

Chosen: narrow both, and say it in the tool's own output.

The host lints its own target with the whole workspace. Linux is no longer
linted on the host, because the container lane lints and builds both Linux
targets with a real C compiler, which is a stronger check than the one it
replaces.

The Apple compile check covers `fetchloom-engine`, `fetchloom-platform`,
`fetchloom-cache` and `fetchloom-faults`, and no longer covers
`fetchloom-sources` or `fetchloom-cli`. That keeps the check where it earns its
place: the Apple platform module is the code with Apple-specific syscalls and no
machine to run them, and it is still type checked the moment it is written. The
two crates it drops are the two that link the client.

Because: the alternative is to claim a compile check that does not run. Linux
loses nothing. Apple loses the type check on two crates, and neither of them
contains Apple-specific code, so what is lost is the chance to catch a Rust
error in portable code, which the host target already catches.

Costs: an Apple-only compile failure inside the source adapter or the binary is
found by nobody, on any machine, ever. That is a real hole and it is stated
rather than papered over.

Uncertain: whether a cross C toolchain for `aarch64-apple-darwin` could be
assembled here at all. It needs Apple's own software development kit headers,
which are not redistributable, so the answer is very likely no.

Sources: `ring` 0.17.14's build script failing for both targets on this machine
in this session; the verification matrix record; the client record for why
`ring` is in the graph.

## A reference naming one object materializes a destination holding it

Question: `get` on a reference naming a single file failed with
`reference.unresolved` and an error blaming readability, because the resolved
path was handed to a directory walk. contracts.md says the grammar accepts a
local file or a directory, so this was a hole in a stated contract. What does a
single-object reference materialize into.

Options: materialize the object as the destination itself, so `--output out`
produces a file named `out`; materialize a destination directory holding the
object under its own name.

Chosen: the second. A destination is always a directory, whether the reference
named one object or a thousand.

Because: a destination that is sometimes a file and sometimes a directory makes
every later rule conditional. Reconcile compares a destination against a tree,
`verify` walks a destination and reports its tree digest, and a lock records a
tree. Each of those has one shape to handle rather than two, and the phase that
adds the second shape pays for it everywhere. The cost is one directory level a
user did not ask for, which is visible and predictable, against a shape
difference that would be invisible until something else broke on it.

The walk now returns the root it took its relative paths from. A reference
naming a container walks that container; a reference naming one object walks its
parent and takes only that object. That is one code path with one input that
differs, rather than a second walk.

The error that blamed readability is separated: a name with nothing at it is
reported as nothing being there, and a name that exists and cannot be read keeps
the readability wording. Sending a user to fix a permission that is already
correct is the failure this project treats as worse than no message.

Costs: `get` on one file writes `out/a.txt` rather than `out`. A user who wanted
the file at an exact path renames it.

Uncertain: nothing. The same decision shapes what an HTTPS reference naming one
object does, which is why it was made here rather than in the phase that only
had local files.

Sources: contracts.md Reference grammar; the report of `get file://…/a.txt`
failing with `os error 267`; `crates/cli/src/materialize.rs` walk.

## The binary doubled when it learned to speak TLS, and the baseline says so

Question: The deterministic benchmark gate failed. The release binary grew from
1,839,104 bytes to 3,885,056 bytes, a hundred and eleven percent against a gate
that fails above five.

Options: silence the gate; carry the old baseline and let every later run fail;
record the new baseline with the cause and the number stated.

Chosen: record it, here and in the baseline file, with the number.

Because: the gate did exactly what it exists for. The growth is not a regression
in code that was already there; it is the cost of a capability the phase added.
`rustls`, `ring`, `rustls-platform-verifier` and `ureq` are two megabytes of
cryptography and protocol, and there is no version of an HTTPS client that does
not carry them. The standards say a deterministic metric that moves is
investigated rather than silenced. It was investigated, the cause is the one
above, and re-recording is the honest end of that investigation rather than a
way around it.

What this buys, so the trade is visible: verification delegated to the platform
trust store on every target, with no bundled root set in the binary.

Costs: two megabytes on every download of the tool, on every platform. Any
later growth is now measured against the larger number, so this is also two
megabytes of headroom that will never be reclaimed by accident.

Uncertain: nothing about the cause. Whether the same capability could be had for
less is a question for a phase that has a reason to ask it; `curl` was refused
for build reasons rather than size, and it links a comparable amount of C.

Sources: `cargo xtask bench --compare` on this machine before and after the
client landed; the client and trust records.

## Phase 2 gate

Question: Does phase 2 meet its exit criterion, and what remains unproven.

Options: Close it; hold it open until a machine exists that can run macOS.

Chosen: Closed on its exit criterion, with the gap named exactly.

Because: the criterion is that a large transfer interrupted twenty times
completes with the correct digest and never restarts from zero above rung four.
The test is `a_large_transfer_interrupted_twenty_times_completes_and_never_restarts_from_zero`
in `crates/cli/tests/transfer.rs`. It moves two mebibytes through the real HTTPS
source, the real cache and the real platform, against a server scripted to close
the connection twenty times at even offsets. It asserts the object is in the
cache under its expected digest, that the run stood on rung three throughout,
that bytes were kept on the final attempt, and that the degradation naming a
restart from zero was recorded zero times. It passes on
`x86_64-pc-windows-msvc` and on both Linux targets.

What was built. The HTTPS source behind the Source seam, with redirects
followed by Fetchloom rather than by the client so the credential drop is
decided and reported. The transfer engine: retry with exponential backoff and
full jitter, the five-rung resume ladder, failover across ordered sources, and
one shared retry used by both the digest-known path and `get https://`. The
adversarial server in the faults crate, seventeen tests over its own behavior.
`get https://` end to end through the phase 1 store, and `get` on a reference
naming one object, which was a hole in a stated contract.

What the suite is. 283 tests pass and 5 are skipped, across the workspace, on
Windows and on both Linux targets. The skips are named below.

Defects the work found, each fixed and each with a test that fails without the
fix:

A capability probe used fixed names, so two probes of one directory read each
other's files as the filesystem's answer. A concurrent probe could report a
case-sensitive volume as case-folding. Found by the first Linux run.

Windows reported no on-access scanner on every unelevated machine, because the
filter enumeration returned an empty list for every failure and it fails with
`ERROR_ACCESS_DENIED` without elevation.

A partial is preallocated to its full length, so its size on disk said nothing
about how much had arrived. Resume appended after megabytes of zeros. The valid
prefix is now recorded and anything past it is discarded before appending.

Progress was discarded when a connection died mid-body, because the read-error
path returned before recording what had arrived, so every interrupted transfer
restarted from zero. This is the defect the gate test exists to catch.

Resume re-read the whole partial twice per attempt, which is quadratic over the
gate's twenty interruptions. It reads once.

A 206 whose `Content-Range` names a span other than the one asked for was
appended without complaint. It now fails with `integrity.range_mismatch`. Found
by the Linux run, where the timing let the case actually occur.

The cache reported a network read failure as `cache.corrupt`, which is terminal,
so an interrupted `get https://` exited eighty instead of retrying. Found by the
interrupted-transfer benchmark.

Three homes for the same limits existed, two of them mine. One remains.

The adversarial server answered a metadata request with a body. A client that
pooled that connection then read the body as the start of the next response, so
the first request of every run failed and was retried. Windows hid it and Linux
showed it. A metadata request now carries every header and no body.

What the benchmarks say. On this machine, cold transfer of four mebibytes from
the local server takes 104 ms and the same transfer interrupted twice takes 747
ms, and both materialize exactly 4,194,304 bytes, which is the deterministic
statement that matters: interruption changes time and never changes bytes. A
warm cache is 225 ms against a cold 1,105 ms and grows by exactly zero.

The binary grew from 1,839,104 to 3,894,784 bytes, which is TLS, and it has its
own record. The deterministic gate caught it and it was re-baselined
deliberately rather than silenced.

Costs, and they are the honest gap:

macOS is compiled and never executed, and `cargo xtask verify` says exactly that
on every run. Since the client landed, two crates are not even compiled for
Apple silicon, because `ring` is C and this machine has no Apple software
development kit. Both Windows on ARM targets are neither compiled nor run.

`get https://` cannot resume. A partial is named by the digest it will hold, and
a bare URL states no digest, so nothing on disk can be matched to what the
source is serving. The run says so with a `degrade` on every remote fetch. The
resume ladder is fully exercised by the transfer engine, whose caller with a
known digest arrives in phase 4 with the lock.

Rung two is proven only at the unit level. The HTTPS source derives identity
from an entity tag, so it produces rungs three, four and five; a content address
or an immutable version identity needs a source that publishes one, which is
phase 7.

Five skips remain. Locking across two users and a FUSE mount, both closed in
this phase by the container, are gone. What is left: a filesystem whose locking
fails with `ENOLCK` or `EOPNOTSUPP`, which no filesystem reachable inside the
container refuses, and whose skip names bindfs, tmpfs and procfs as the three
that were tried; and the four child processes the race and kill loops spawn,
which were never skips.

A network-backed volume is not built anywhere in this matrix, so the conservative
locking path and the network magic set stay unproven.

The fault server's script is consumed per request, and the client may retry a
request of its own on a pooled connection, which consumes a scripted reply
without the transfer having made an attempt. Two tests that depended on the
exact count were moved to the source, where the request is explicit. The
benchmark's interruption count is set below the attempt limit for the same
reason and says so.

Uncertain: whether the same twenty interruptions hold on a filesystem that is
slower than this one. The test asserts rungs and byte counts rather than
durations, so it should, and it has not been run anywhere but here and in the
container.

What phase 3 inherits as debt. Resume for a reference that states no digest,
which needs the lock. Rung two, which needs a source with immutable identity.
Two Apple crates that no machine compiles. The `ENOLCK` skip, which needs a
filesystem nothing here builds. Reconcile, which is why `get` into an existing
destination still fails with `destination.foreign`.

Sources: `cargo xtask verify` on this machine; `cargo test --workspace` on
`x86_64-pc-windows-msvc`, `x86_64-unknown-linux-musl` and
`x86_64-unknown-linux-gnu`; `cargo xtask bench --save-baseline`; `cargo deny
check`.

## A wall clock on this machine is recorded and never gated on

Question: The benchmark gate failed twice in one session on timing alone, with
every deterministic metric identical: cold cache moved five percent between two
runs minutes apart, and warm cache twenty eight. What should a timing metric
gate at.

Options: widen the band until it stops firing; re-record the baseline each time;
record timing and never gate on it.

Chosen: record it, report it, never gate on it. Only deterministic metrics gate,
and they gate at five percent everywhere.

Because: the standards already say a timing metric gates against a baseline
recorded on that same runner, and that a timing gate never fails an ordinary
local run. There is no runner. This is a desktop that also runs the container
lane, a browser and whatever else, and the numbers show it: warm cache measured
between 220 and 306 milliseconds across a single afternoon with no change to any
byte of code. A band wide enough never to fire on that noise is a band too wide
to catch a real regression, so it would be a gate in name only. Widening it to
twenty five percent was tried and still fired.

Re-recording each time is the option the standards forbid outright, because it
turns evidence about the machine into a moving target that can never fail.

What still protects the work, and it is the stronger half: cache growth is
exactly the corpus on a cold run and exactly zero on a warm one; an interrupted
transfer materializes exactly the same 4,194,304 bytes as an uninterrupted one;
the binary has an exact size. Each is identical on identical inputs, each gates
at five percent, and each would catch the kind of regression that matters, which
is a second transfer, a lost byte or a dependency that grew.

Costs: a change that makes a transfer twice as slow while moving the same bytes
passes. That is a real hole and it stays open until a quiet machine exists to
measure on. The number is still printed on every run, so it is visible to anyone
who looks, and a baseline is still recorded so the trend can be read later.

Uncertain: nothing about the measurement. Whether a dedicated runner will ever
exist is the open question, and the gate returns the day one does.

Sources: `cargo xtask bench --compare` failing on `cold-cache` at five point one
percent and on `warm-cache` at twenty eight percent in this session, with
`cache-growth` byte-identical in both; standards.md Measure.

## Asking whether an outboard tree exists takes no lock, and one buffer serves every attempt

Question: A profile of the transfer path found two things done once per attempt
that need not be done at all. Whether an outboard tree exists was answered by
opening it, which takes a shared advisory lock and a file handle and drops both
immediately. The buffer bytes move through was allocated inside the attempt, so
a transfer interrupted twenty times allocated and freed a mebibyte twenty times.

Options: leave both, on the grounds that neither is in a per-chunk loop; fix
both.

Chosen: fix both. The store answers `has_outboard` with the same cheap existence
check `contains` already uses, and the buffer is allocated once per source and
lent to every attempt.

Because: the lock is the one that matters. There is a record already stating
that asking whether an object is present takes no lock, for the reason that the
answer is only ever a reason to open it and opening it takes the lease and looks
again. The outboard is the same question and had a different answer, which is
the second way of doing one thing that the standards forbid. It also contended
with the exclusive lease the same attempt was about to take.

The buffer is smaller but it is what the standards say plainly: a buffer is
allocated once and reused.

What it measured, on this machine, with the same corpus and the same server:
cold transfer of four mebibytes moved from a spread of 104 to 290 milliseconds
to a spread of 49 to 58, across three runs each. The interrupted regime moved
from 239 to 531 milliseconds to 381 to 504, which is inside the noise this
machine produces and is not claimed as an improvement. Every deterministic
metric is unchanged, which is the point: this changed timing and touched no
byte.

Costs: one more method on the Store seam, which is the sixth this phase added to
it. That is real pressure on a seam the roadmap says must not widen casually,
and it is the reason this one replaces a misuse rather than adding a capability.

Uncertain: whether the cold improvement is the lock or the allocation. Both were
changed together and the two were not measured apart, because the lock is
correct to remove regardless of what it costs.

Sources: `cargo xtask bench --iterations 3` three times before and three times
after, on this machine; the record on asking whether an object is present;
standards.md Memory.

## A partial is named by what the run knows, and a bare URL knows its source

Question: `get https://host/object` restarts from zero after an interruption. A
partial is stored at `partial/<content digest>`, and a reference that states no
digest cannot name one, so nothing on disk can be matched to the response. Phase
2 shipped a `degrade` on every remote fetch saying exactly that. What names a
partial.

Options: leave the deferral until the lock arrives in phase 4; key the partial by
a digest of the source identity always, and take the object lease separately;
key the partial by whichever of the two the run knows.

Chosen: the partial key is one type with two constructors. When the caller states
a content digest, the key is that digest and every behavior is what phase 1 and
phase 2 already do. When the caller states none, the key is the BLAKE3 of the
canonical source identity, which is the redacted location, the host, and the
identity the source published, in that order, each length-prefixed. The lease,
the partial file, the source record beside it and the owner record are all named
by the key. Publication is still to `objects/<observed digest>`, and when the key
is a content digest the observed digest must equal it or the bytes are refused.

Because: contracts.md describes `partial/` as holding transfers "with recorded
source identity", and the resume ladder stands on identity, not on a digest the
reference happened to state. The digest key was a constraint this implementation
chose, not one the contract asked for. `SourceRecord` already carries the
location, the host, the identity, the entity tag, the last modified value, the
range support and the byte count, so a resume of a bare URL has everything it
needs and had nowhere to put it.

Keying on source identity always was the other real option and it is the one
that would break something: a locked run knows the digest before it starts, and
the single-writer-per-digest claim that makes two concurrent processes do one
transfer is taken on that digest. Moving the key off the digest would let two
processes fetching one object from two mirrors each run a transfer. Two
constructors of one key is not two ways of doing one thing, because there is one
lease, one partial path, one record and one publication path; the key is an
input, chosen by what the caller was told.

Costs: two processes fetching the same bare URL through one cache now contend on
one key, which is the behavior a digest key gave and is what the contract wants.
Two processes fetching the same bytes from two different bare URLs do two
transfers and publish one object, which a digest key would also have done only
after both had finished. A key derived from a location means a location that
changes spelling without changing bytes starts a new partial; the old one is
swept by prune like any other.

Uncertain: nothing in the mechanism. Whether a source identity that is a strong
entity tag alone is stable enough across a long interruption is a property of
the server, and rung four already exists to say so.

Sources: contracts.md Cache and Resume ladder; the phase 2 gate record naming
this as inherited debt; `crates/engine/src/source_record.rs`.

## Three counters that are identical on identical inputs, and no clock among them

Question: The wall clock on this machine is recorded and never gated on, and the
record saying so names what that leaves uncovered: a change that makes a
transfer twice as slow while moving the same bytes passes. Some of that is
recoverable without a clock. The quadratic partial reread fixed in phase 2 was
counted, not timed. What else can be counted.

Options: leave the deterministic gate as cache growth, bytes materialized and
binary size; add a timing gate back at a wider band; add counters of the work
itself.

Chosen: the run counts three things and reports them in its `--json` result:
bytes read from a file, bytes written to a file, and requests issued to a
source. Each is gated at five percent alongside cache growth, bytes
materialized and binary size. No timing gate returns.

Because: each of the three is identical on identical inputs, which is what
standards.md requires of a metric that gates on a developer machine. Together
they catch the class of regression that a wall clock was being asked to catch
and could not, because the machine moved more than the code did: a second read
of a file that was already read, a second write of bytes already written, a
retry storm, a probe per attempt where one probe would do, and a resume that
rereads what it already hashed. The phase 2 quadratic reread would have moved
bytes read by a factor of the interruption count while every existing
deterministic metric stayed byte-identical.

What they cannot catch, said plainly: an algorithm that gets slower while
touching the same bytes. A sort that goes quadratic in memory, a lock held
across a loop, a per-entry syscall storm that moves no bytes, and a
decompressor that got slower all pass. Syscall counts would catch the third and
are not counted, because a count of syscalls is not identical on identical
inputs across three platforms and would have to be gated per platform to mean
anything.

Costs: the counters have to be threaded to every place that reads a file, writes
a file, or issues a request, and a path that forgets to count is a hole that is
invisible rather than loud. They are therefore counted at the boundary each kind
of work goes through, which is the cache for file bytes and the source for
requests, so a new caller counts by construction rather than by remembering.

Uncertain: whether requests issued stays deterministic once concurrency is
measured rather than fixed, in phase 6. It is deterministic today because
concurrency is a fixed default and every retry is driven by a scripted server.
When adaptation lands, this gate is re-examined rather than widened.

Sources: standards.md Measure; the record on a wall clock on this machine; the
record on the quadratic partial reread.

## Two containers, four compressions, and not one line of C

Question: Which archive and compression formats ship, and which crates decode
them, given that a dataset is fetched by a static binary on six targets and that
phase 2 already lost two targets to a C dependency.

Options: per container, `zip` against writing a reader here; per compression, the
C reference implementation with a Rust binding against a pure-Rust decoder;
and for each format, shipping it against failing on it by name.

Chosen. Containers: `tar` and `zip`. Compressions: gzip, zstd, xz, and bzip2,
each usable on its own or wrapping a tar. The format names a manifest may
state, and the only ones:

| Name | What it is |
|---|---|
| `tar` | A POSIX ustar stream |
| `tar+gzip` | A tar wrapped in a gzip member |
| `tar+zstd` | A tar wrapped in a zstd frame |
| `tar+xz` | A tar wrapped in an xz stream |
| `tar+bzip2` | A tar wrapped in a bzip2 stream |
| `zip` | A zip container, store and deflate methods only |
| `gzip`, `zstd`, `xz`, `bzip2` | One compressed object, which materializes as one file |

Crates, all pure Rust, no C toolchain and no build script that compiles one:

| Job | Chosen | Version | Brings |
|---|---|---|---|
| tar | `tar`, default features off | 0.4.46 | `filetime` |
| zip | `zip`, default features off, `deflate-flate2` only | 8.6.0 | `indexmap`, `memchr`, `typed-path`, `crc32fast` |
| gzip | `flate2`, default `miniz_oxide` backend | 1.1.10 | `miniz_oxide`, `adler2`, `simd-adler32`, `crc32fast` |
| zstd | `ruzstd` | 0.9.0 | `twox-hash` |
| xz | `lzma-rust2`, `std` and `xz` only | 0.20.0 | `sha2`, which the workspace already carries for the interop digest |
| bzip2 | `bzip2`, whose default backend is `libbz2-rs-sys` | 0.6.1 | `libbz2-rs-sys` |

Because. What datasets publish decides the list, not what a compression library
supports. Zip is what Kaggle serves, what Zenodo tells depositors to use for
more than twenty files, and what Figshare and the OpenML and UCI collections
hold. Tar is what WebDataset is, which is how Hugging Face ships large
multimodal datasets as sharded tar archives it documents as orders of magnitude
faster to stream than separate files. Gzip wraps most of the tar in that world
and is what Common Crawl's WARC files are. Zstd is what contracts.md's own
manifest example names and what the newer corpora are moving to. Bzip2 is not
legacy trivia: the Wikipedia dumps, which are among the most fetched research
datasets there are, are `.xml.bz2`. Xz is what the Linux-adjacent scientific
distributions use. Rar and 7-Zip are absent because no dataset publisher of any
size distributes on them, and both would be a decoder with an attack surface
larger than everything above it combined.

Zip carries store and deflate and nothing else. A zip entry compressed with
bzip2, lzma, ppmd, xz or zstd fails with `archive.unsupported` naming the method
number and the member. Published dataset zips are store or deflate; supporting
the rest means five more decoders reachable from inside a container for entries
that do not exist in the wild.

Pure Rust is the whole reason for four of the six crate choices, and phase 2 is
the evidence. `ring` is C, and the record on what the C in the cryptography
costs the compile-only lanes says plainly that two crates are not compiled for
Apple silicon on this machine for want of an Apple software development kit. The
C zstd binding, `bzip2-sys` and `liblzma` would each add that same cost to a
target matrix that already cannot run half of itself. Measured against that, the
one place pure Rust is slower is zstd: `ruzstd` documents itself as roughly 3.5
times slower than the C decoder on highly compressible data and about 1.4 times
on data that is already dense. That is a real cost on a hot path and it is paid
knowingly, because a decoder that cannot be built for a target is infinitely
slower there.

The other three cost nothing measurable. `flate2`'s default backend is
`miniz_oxide`, which is Rust. `bzip2` 0.6 made `libbz2-rs-sys`, a Rust port, its
default backend, so the C library is now the opt-in. `lzma-rust2` is Rust and
its only dependency is `sha2`, which this workspace already builds for the
interop digest, so xz costs one crate and zero new transitive ones.

The format of an artifact is stated by its manifest. A reference with no
manifest takes its format from the location's final extensions, and the
archive's own header must agree with what the name said or the run fails with
`archive.unsupported` naming both answers. Neither the name nor the bytes decide
alone, because a name is a claim and a header without a claim to check is a
guess.

Costs: six crates and eleven transitive ones, against a workspace that had
none of them, and a binary that will grow by all of it. Zstd decoding is slower
than the reference implementation by the factors above. A zip using a
compression method outside store and deflate fails rather than extracting.
`tar` brings `filetime`, which extraction never uses, because timestamps are
excluded from the tree digest.

Uncertain: how much of the binary this is. Nothing has been compiled into the
release binary yet, so the number goes in the phase 3 gate record next to the
old one, the way TLS did.

Sources: compiled and linked on `x86_64-pc-windows-msvc` with exactly the
features above, `cargo tree -e normal` and `cargo tree -e build` showing no
build-script C compilation; crates.io metadata for every version and feature
table named; Hugging Face's WebDataset documentation; Zenodo's guidance on
packaging more than twenty files; `ruzstd`'s own published decode comparison.

## Every rejection stops the run, and the error names the entry that caused it

Question: contracts.md lists what extraction rejects. It does not say whether a
rejected entry is skipped or ends the run, and it does not map each rejection
onto one of the five archive error kinds. Both have to be settled before an
extractor exists.

Options: skip the rejected entry and extract the rest, emitting `extract.reject`
for each; skip only the rejections that are about the entry and stop on the ones
about the archive; stop on every rejection.

Chosen: every rejection stops the run. Nothing is published. `extract.reject` is
emitted naming the member and the reason, and the run exits 70 for an archive
rejection and 60 for a destination one. A rejection is called per-entry when the
error names a member path and fatal when it names the archive, which is a
statement about what the message can point at, not about whether the run
continues.

The complete list, with the kind each produces and what the error names:

| Rejected | Kind | Names |
|---|---|---|
| Absolute member path | `archive.unsafe_path` | the member |
| A `..` component anywhere in the path | `archive.unsafe_path` | the member |
| A backslash or drive letter in the path | `archive.unsafe_path` | the member |
| A path that is not valid UTF-8 | `archive.unsafe_path` | the member, as bytes |
| A path holding a NUL | `archive.unsafe_path` | the member |
| A path longer than the volume's maximum | `archive.unsafe_path` | the member and both lengths |
| A path deeper than the nesting limit | `archive.unsafe_path` | the member and both depths |
| A link target that resolves outside the destination | `archive.link_escape` | the member and the target |
| A hard link to a member the archive does not hold | `archive.link_escape` | the member and the target |
| A block device, character device, FIFO, or socket entry | `archive.unsupported` | the member and the type |
| A setuid or setgid bit | `archive.unsupported` | the member and the bits |
| Ownership, an access control list, an extended attribute, or an alternate data stream | `archive.unsupported` | the member and which one |
| Two members with the same path | `archive.collision` | both members |
| Two members colliding under the volume's case folding or normalization | `archive.collision` | both members |
| A local header whose path disagrees with the central directory | `archive.unsafe_path` | the member and both paths |
| A local header whose size or method disagrees with the central directory | `archive.unsupported` | the member and both values |
| A compression method that is not store or deflate, inside a zip | `archive.unsupported` | the member and the method |
| A container or compression this build does not carry | `archive.unsupported` | the archive and the format |
| A header the format does not permit, or a truncated archive | `archive.unsupported` | the archive |
| More members than the entry limit | `archive.bomb` | the archive and both counts |
| More expanded bytes than the limit | `archive.bomb` | the archive and both counts |
| An expansion ratio above the limit | `archive.bomb` | the archive and both ratios |
| A name the target volume refuses | `destination.unrepresentable` | the member and what the volume said |

Because: skipping is a silent degradation of exactly the kind standards.md
forbids, and it is worse here than elsewhere. A user who asked for a dataset and
received it minus the four members that tried to escape has a tree that is not
the tree they asked for, has no digest that matches anything, and has no reason
to look. Emitting `degrade` and continuing does not fix that, because the
result is still a different dataset wearing the right name. contracts.md already
says the destination is all or nothing and that a partially materialized
destination is never visible, and the roadmap's gate for this phase is that the
hostile corpus is rejected with nothing published. Stopping is the only reading
consistent with all three.

The mapping puts everything that is a lie about where an entry lands under
`archive.unsafe_path`, everything that is a lie about what an entry is under
`archive.unsupported`, and everything that is two entries claiming one name
under `archive.collision`. Setuid, device nodes and extended attributes are
`archive.unsupported` rather than a kind of their own because no kind of their
own exists and inventing one is forbidden; the message carries the specific
fact.

Nesting depth is `archive.unsafe_path` rather than `archive.bomb` because it is
a property of one member and the error can name it. The three limits that are
properties of the whole archive are `archive.bomb`.

Costs: an archive holding one hostile member out of a million cannot be fetched
at all, even with a selection that excludes that member. That is deliberate: an
archive that contains an escape attempt is not a dataset with a flaw, and
selecting around it would mean the safety of the result depends on the user
having written the right glob.

Uncertain: whether a real published dataset carries a setuid bit or an extended
attribute by accident of how it was packed. If one does, it fails loudly and
that is the right first answer; the record is revisited with the evidence rather
than in advance of it.

Sources: contracts.md Materialization, Errors, Exit codes, Partial success;
roadmap.md phase 3 Prove; standards.md Failing.

## Reconcile compares against what the run resolved, not against a file it has not written yet

Question: contracts.md says each reconcile outcome is decided against the
receipt. Receipts arrive in phase 4. Reconcile is a phase 3 deliverable, and
without it `get` into an existing destination still fails with
`destination.foreign`, which is inherited debt from phase 2. What does phase 3
compare against.

Options: bring receipts forward and decide their contents now; keep failing on
an existing destination until phase 4; compare against the tree the run
resolved.

Chosen: reconcile compares the destination against the tree the run resolved.
For each entry the run is about to materialize, the destination holds it with
the same content digest, holds it differently, or does not hold it; and the
destination may hold entries the resolved tree does not. Those are exactly the
four outcomes contracts.md names. A receipt, when phase 4 writes one, is a
cached copy of the answer this comparison already produces, never a second
authority for it.

Because: the resolved tree is the only thing that can be an authority. It is
derived from the digest the manifest or the lock states, so it is the same
answer on every machine, while a receipt is local, may be absent, may be stale,
and contracts.md already says a receipt is "never read as an authority for
identity". Deciding receipt contents now would settle a phase 4 question with
phase 3 evidence, and it would put a file on disk that the comparison does not
need.

What a run does with the outcomes. Every entry unchanged means nothing is
written at all: no staging directory, no rename, status `unchanged`, exit 0. Any
entry missing and none modified or foreign means the missing entries alone are
built in staging and published into the destination by rename, one entry at a
time. A modified or foreign entry stops the run before anything is staged, names
every such path, and exits 60.

`--force` accepts overwriting modified entries and removing foreign ones.
`--adopt` accepts the destination as it stands, writes nothing, and reports the
tree the destination actually holds rather than the one that was resolved. Both
are per-invocation and neither is ever implied.

A destination is never partially reconciled, and restoring missing entries is
not a violation of that. The phrase forbids leaving a destination that is
neither the tree it held nor the tree that was asked for. A restoration only
ever adds an entry the resolved tree names, so a failure part way through leaves
a destination that is still a subset of the correct tree and still missing
entries, which is where it started. Overwriting is different, and that is why
overwriting needs `--force` and goes through a full staging publication rather
than an entry at a time.

Costs: the comparison hashes every file in the destination that it cannot rule
out by fingerprint, so a no-op run over a large tree reads metadata for every
entry and, under `--verify always`, reads every byte. That is the price of an
answer that does not depend on a local file being correct, and it is what the
no-op regime measures.

Uncertain: nothing about the comparison. Whether phase 4's receipt ends up
holding anything this does not already compute is the open question, and it is
phase 4's to answer.

Sources: contracts.md Reconcile, Receipt, Partial success, Output streams;
roadmap.md phase 3 Build; the phase 2 gate record naming reconcile as inherited
debt.

## A glob is four rules, and a flattened path that empties is an error

Question: contracts.md defines selection as `--select`, `--exclude`, and a
layout, matched on the canonical member path with `**` crossing directories and
`*` not. It does not say which other pattern syntax exists, whether selecting a
directory selects what is under it, or what happens when flattening leaves a
member with no path.

Options: adopt an existing glob crate's full syntax; define the smallest syntax
that expresses what selection is for.

Chosen: four rules and nothing else. `*` matches any run of bytes within one
path component, including none. `**` as a whole component matches any number of
components, including none. `?` matches exactly one byte within one component.
Every other byte is literal, including `[`, `{`, and `\`. Matching is on raw
bytes, case-sensitive, with no normalization, against the canonical
`/`-separated member path.

A pattern matches member paths, not subtrees: `--select data` selects a member
named `data` and nothing under it, and `--select data/**` selects what is under
it. Every ancestor directory of a selected member is included whether or not a
pattern matched it, because contracts.md requires every directory to be an
entry and a tree with a file and no directory holding it is not a tree.

`--exclude` is applied to the result of every `--select`. An empty result is an
error, as contracts.md already states.

Under `--layout flatten:<n>`, a member left with no path at all after dropping
`n` components fails with `destination.unrepresentable` naming the member and
the count. Two members left with the same path fail with `archive.collision`
naming both, which contracts.md already states.

Because: selection is part of identity. A lock records the patterns, and the
same patterns must select the same members on every machine forever. Character
classes and brace expansion are the parts of glob syntax where implementations
disagree with each other, and adopting a crate's syntax makes a crate's version
part of identity. Four rules have one reading.

Selecting a directory by name selecting everything under it was the alternative,
and it is what most tools do. It is rejected because `--exclude data` would then
mean something different from `--select data` inverted, and because a user who
wrote `--select data` and got a hundred gigabytes was not told that would
happen. `data/**` says it.

Dropping a member whose path flattens to nothing is the silent loss the
standards forbid. Failing names the member and the count, and the fix is a
smaller count.

Costs: a user who wants `*.{jpg,png}` writes two `--select` flags. A user who
wants a character class cannot express it. Both are repeatable flags, so
nothing is unreachable, only longer.

Uncertain: whether `**` should also be allowed inside a component, as in
`a**b`. It is not, and no dataset selection seen so far needs it. If one does,
allowing it later is additive; allowing it now and finding it ambiguous is not.

Sources: contracts.md Selection, Materialization, Lock.

## Every name in staging is created exclusively, and links are created last

Question: roadmap.md says staging extraction uses handle-relative operations.
Unix has `openat`, `mkdirat` and `symlinkat` through rustix. The Win32 surface
this build uses has no relative create, and reaching one means the native API
and a large amount of unsafe code. What makes extraction safe on all three.

Options: handle-relative on Unix and something else on Windows; handle-relative
everywhere through the native Windows API; exclusive creation plus an ordering
rule everywhere.

Chosen: exclusive creation plus an ordering rule, on all three platforms, and no
handle-relative operations anywhere. Every directory and file in staging is
created with the exclusive flag, so a name that already exists is a failure
rather than a reuse. Symbolic links are created after every other entry of the
archive has been created. A hard link member is materialized as a file holding
its target's bytes, and a hard link to a member the archive does not hold is
`archive.link_escape`.

Because: what handle-relative operations buy is that a component of a path
cannot be swapped for a symbolic link between the moment it is checked and the
moment it is used. Creating every component exclusively removes that class
instead of narrowing it: nothing in staging exists that this run did not create,
so there is no component an archive could have supplied to be followed. Creating
links last closes the only remaining door, which is an archive that supplies a
link and then a member underneath it; that member is created first, the link
then fails to be created exclusively, and the archive is refused with
`archive.collision` naming both.

contracts.md already relies on exactly this mechanism for collisions: entries
are created exclusively in staging so the target filesystem's own folding
decides. Using the same mechanism for path safety is one way of doing one thing.
Using `openat` on Unix and a different argument on Windows would be two, and the
second one would have to emit `degrade` on every Windows run, which is the noisy
per-run degradation this phase deleted from the transfer path.

Costs, stated plainly. This does not protect against a second process that has
write access to the staging directory while extraction is running and races the
exclusive create. Handle-relative operations would not fully protect against
that either, but they would narrow it. Staging lives in the cache, whose
directory the cache owns, or beside the destination, which is the user's own
directory, and the phase 1 owner records and advisory locks are what keep two
Fetchloom runs out of each other's staging. A hostile local process with write
access to the user's own destination directory is outside what any extractor
defends against.

Uncertain: whether a filesystem exists that accepts an exclusive create of a
name that already exists under its own folding rules. That would break collision
detection as much as it would break this, and contracts.md already stakes
collision detection on it, so the two stand or fall together and the capability
probes are where it would be caught.

Sources: contracts.md Materialization and Platform capabilities; roadmap.md
phase 3 Build; rustix 1.1.4 `fs` module.

## A compressed tar is read twice, and the second read is the one that writes

Question: The Archive seam lists members and then opens them one at a time. A
zip is random access and a plain tar over a file is seekable, so both answer that
shape cheaply. A tar inside a gzip, zstd, xz or bzip2 stream is neither. What
does listing a compressed tar cost.

Options: widen the seam so extraction is one traversal; decompress the tar into
the cache as a derived object and extract from that; read the compressed stream
twice.

Chosen: read it twice. Listing decompresses the stream and reads headers,
skipping member bodies. Extraction decompresses it again and writes. `open` is
called with members in the order listing returned them, and the tar reader only
ever moves forward.

Because: the seam is one of six the roadmap says never change shape, and this is
not a reason strong enough to change one in the phase that first implements
against it. Listing before extracting is also what makes the phase's gate
reachable: the entry count, the expanded byte total and the expansion ratio are
all properties of the whole archive, and refusing a bomb after writing nine
tenths of it is not refusing it.

Decompressing into the cache as a derived object was the tempting answer and it
is the one to revisit. It would make every read after the first free, because the
plain tar would be content-addressed and reusable, and it would make the reader
deal only with seekable containers. It is not taken now because a derived object
is a concept `objects/` does not have, the contract says an entry there has been
fully verified against a digest a source stated, and inventing a second kind of
object to save a decompression is a larger change than the one it saves.

Costs: a compressed tar is decompressed twice, so the extraction regime pays
roughly double the decompressor's time for that container. Zstd is the format
where that is most visible, because the chosen decoder is already the slower of
the two available. The number is in the benchmarks rather than in this record.

Uncertain: whether the doubling is visible against filesystem time for a real
dataset, where writing several hundred thousand small files is expected to
dominate. The many-small-files regime measures the write side and the one-large-
file regime measures the decompressor, and the two together are what would say
this was the wrong call.

Sources: contracts.md Materialization and Limits; roadmap.md Seams;
`crates/engine/src/seam/archive.rs`.

## The zip index cannot hold two entries of one name, so the central directory is counted rather than asked

Question: The corpus holds a zip whose central directory declares two records
naming one path, which contracts.md says is `archive.collision` naming both. The
reader listed one member and refused nothing. Why, and what counts entries.

Options: trust the crate's entry count and accept that this collision is
undetectable; compare the count the end record declares against the count the
crate exposes; read the central directory's names directly.

Chosen: read the central directory's names directly, before the crate's index is
consulted at all, and claim each path into a set. The container's own records
decide how many entries it holds.

Because: `zip` 8.6.0 indexes members by name, so two records naming one path
collapse into one entry and `len()` answers one. Nothing about that is wrong for
a library whose job is to find a member by name, and nothing about it is usable
for a reader whose job is to refuse an archive that names one path twice. The
crate is not being worked around; it is being asked a different question than the
one it answers.

Comparing the declared count against the exposed count was tried first and it
does detect the collision, but it cannot name the path, and contracts.md requires
both members to be named. An error that says two entries collided without saying
on what is the kind of message standards.md calls a defect.

Costs: the central directory is parsed twice, once here by hand and once inside
the crate. That is one extra pass over a structure that is kilobytes, against a
member listing that already reads every header, so it does not show up in any
regime. It also means the record layout is written down in this codebase and must
stay correct; the corpus's own writer is the thing that keeps it honest, because
the two disagree loudly if either drifts.

Uncertain: whether Zip64 archives with more than 65,535 entries need the Zip64
end record read instead, which this does not do. The benign Zip64 corpus entry
passes because it is small. An archive above that count is the entry limit's
territory and is refused there, so the gap is bounded, but it is a gap and it is
named here rather than found later.

Sources: `crates/faults/src/archives.rs` zip writer and its record layout tests;
the APPNOTE record layouts for the local file header, the central directory file
header, and the end of central directory record; contracts.md Materialization.

## What the reader decides, and what only the destination can

Question: The rejection table in contracts.md lists everything extraction
refuses. The reader lists members and hands out bytes and never touches a
filesystem. Which of those rejections can it decide.

Options: have the reader decide everything and take a destination volume as an
argument; split the table by what a header alone can answer.

Chosen: split it. The reader decides everything decidable from headers and from
the archive's own structure, and the extractor decides everything that needs the
target volume or the order entries are created in.

The reader decides: absolute paths, `..` components, backslashes and drive
letters, paths that are not valid UTF-8, paths holding a NUL, paths deeper than
the nesting limit, device and FIFO entries, setuid and setgid bits, pax extended
attributes and ownership, a zip local header disagreeing with its central
directory, a compression method that is not store or deflate, two members
claiming one path byte for byte, the three bomb limits, and a link target that is
absolute, names a drive, or climbs above the destination root by counting `..`
against the member's own depth.

The extractor decides: two members colliding only under the target volume's case
folding or normalization, a name the volume refuses, a path longer than the
volume's maximum, and a link that escapes only because of what the volume does
with it.

Because: the split is not a convenience, it is what the two have evidence for. A
link target of `../../etc/passwd` is an escape on every volume and needs nothing
to prove it. Whether `A.txt` and `a.txt` are one name is a property of the volume
and contracts.md already says it is answered by creating each entry exclusively
in staging rather than by a table this project would have to keep correct. Asking
the reader to answer it would mean either passing it a volume, which makes a
pure enumerator depend on a destination, or keeping that table.

A link target is checked by counting depth rather than by resolving the path,
because resolving requires knowing what exists, and at listing time nothing does.
Counting is exact for the question being asked: a target climbs out when its
`..` components exceed the member's own directory depth.

Costs: the corpus is now read by two suites rather than one, and an entry the
reader lists rather than refuses is asserted by name in the reader's suite so it
cannot be silently forgotten by both. That list is a standing obligation on the
extraction work rather than a decision that closes anything.

Uncertain: whether a hard link naming a member the archive does hold, but which
was itself refused, is reachable. Every refusal ends the run, so it is not,
today.

Sources: contracts.md Materialization and Platform capabilities;
`crates/archive/tests/corpus.rs`, whose deferred list is the split written down.

## The counters found a speculative write on their first run, and it is not fixed here

Question: The work counters were added to catch a second write of bytes already
written. Their first run reported that a local `get` into a warm cache writes
every source byte and then deletes them, because `ingest_from` writes the bytes
to a scratch file, hashes them, and only then asks whether the cache already
holds that digest. Cold and warm are byte-identical in both counters. Is that
fixed now.

Options: hash the source without writing and write only on a miss; keep the
speculative write and record the finding; change nothing and say nothing.

Chosen: record it, test the behavior that exists, and do not change it in this
change.

Because: a local source states no digest, so its digest cannot be known without
reading it, and the write is speculative rather than wasteful by construction.
Removing it means hashing in one pass and reading the file a second time when
the digest turns out to be absent. That trades one write for one read on every
miss to save one write on every hit, and which is better depends on the ratio of
hits to misses and on what the volume charges for each. standards.md says to
profile before changing anything and forbids guessing at bottlenecks as a
justification, and there is no measurement here yet, so the change is not made
on the strength of the observation alone.

It is recorded rather than left because a counter that finds something on its
first run and is then quietly re-pointed at something it does not find is worse
than no counter. The test asserts what is true: both runs read the source once
and write it once, and it says in its own name why.

The phase gate's claim is a different one. "Repeated runs against an unchanged
destination write nothing" is about reconcile, which decides before anything is
staged and therefore never reaches the ingest at all. That test belongs with
reconcile and is written there, against a destination rather than against a
cache.

Costs: a warm local run does work it could avoid, and the amount is exactly the
size of the dataset. On the many-small-files regime that is the whole corpus
written twice per run. The number is now visible on every `--json` result rather
than hidden, which is the difference between a known cost and a defect.

Uncertain: whether the second read on a miss is cheaper than the write it
removes, on any of the three platforms. Nothing here measures it. The
many-small-files and one-large-file regimes are where that would be answered,
and the answer may differ between them, in which case the honest outcome is that
neither ordering is right for both and the choice is made per regime or not at
all.

Sources: `crates/cli/tests/work.rs`, whose warm and cold counts are equal;
`crates/cache/src/ingest.rs`; standards.md Measure and Disk.

## What the counters said the first time they ran

Question: The three deterministic counters are recorded in every regime. What do
they report, and what does that say that the wall clock did not.

Chosen: recorded as the baseline on this machine, and read rather than filed.

The numbers, on `x86_64-pc-windows-msvc`, three iterations:

| Regime | bytes read | bytes written | requests | cache growth |
|---|---|---|---|---|
| cold-cache | 16,777,216 | 16,777,216 | 0 | 16,777,216 |
| warm-cache | 16,777,216 | 16,777,216 | 0 | 0 |
| cold-transfer | 0 | 4,194,304 | 3 | n/a |
| interrupted-transfer | 4,194,304 | 4,194,304 | 7 | n/a |

Three of these are worth saying out loud.

A warm cache grows by exactly zero and still reads and writes the whole corpus.
That is the speculative ingest write, which has its own record. Cache growth was
the only deterministic metric that saw the warm run before, and it reported
success, because the cache genuinely did not grow. The work counters report the
same run doing sixteen mebibytes of avoidable writing. That is precisely the hole
the counters were added to close, and it was open.

A cold transfer of one object issues three requests. One probe brackets the
resolve, and the transfer's own attempt probes again before fetching, so a run
that needs one HEAD and one GET makes two HEADs and one GET. Nothing was
measuring that before.

An interrupted transfer reads back four mebibytes it already had. That is the
resume rehashing what is on disk to rebuild the digest, which is correct and is
what rung one exists to make unnecessary once an outboard tree is stored. It is
the number that will move when phase 5 lands, and now there is something for it
to move from.

Across two runs minutes apart every one of these twelve numbers was identical
while the warm-cache wall clock moved from 394 to 304 milliseconds, which is
twenty three percent. That is the case for the counters and against the timing
gate, made by the same two runs.

Costs: none measured. The counters are atomic adds on paths that are already
doing a syscall.

Uncertain: whether the redundant probe is removable without losing the resolve
bracket the events depend on. It is not removed here, because removing it means
deciding whether `resolve.start` may be emitted from inside a transfer attempt,
and that is an event-ordering question rather than a counting one.

Sources: `cargo xtask bench --save-baseline --iterations 3` and `cargo xtask
bench --compare --iterations 3` on this machine, both pasted into the phase
report.

## The small-files regime, the lane it ran in, and the number nobody wants

Question: contracts.md requires the many-small-files regime measured with an
on-access scanner enabled, not only with one disabled, and requires every
benchmark number to say which lane it came from. What lane does this machine
give, and what did the two shape regimes measure.

Chosen: both regimes ship, and every run prints the lane sentence beside the
numbers rather than leaving the reader to guess.

The lane. Windows enumerates what inspects a write by listing loaded
minifilters and looking for a scanner altitude, which is why contracts.md gives
this platform the `present` answer that the others cannot produce. That
enumeration requires elevation. In an ordinary unelevated shell `fltmc filters`
fails with `0x80070005`, access denied, so `loaded_minifilters` returns nothing
and the probe falls to `unknown` carrying the measured ratio. That is exactly
what contracts.md says an unknown answer is, and it emits `degrade` naming the
ratio and that the cause cannot be determined. It is correct behavior, and it
means the `present` answer is only reachable from an elevated run. The roadmap's
expectation that Windows is the platform that can enumerate is true of the
platform and not of every process on it.

Defender was verified on for these numbers, out of band:
`Get-MpComputerStatus` reported `RealTimeProtectionEnabled: True` and
`OnAccessProtectionEnabled: True`. So the scanner lane is enabled even though
the probe is not permitted to name it. The measured cost ratio was 48.69, which
is small writes costing roughly forty-nine times what the same bytes cost
written to one file.

The numbers, one iteration, on this machine, scanner enabled:

| Regime | wall | bytes read | bytes written |
|---|---|---|---|
| many-small-files, 4096 files of 1 KiB | 101,516 ms | 4,194,304 | 4,194,304 |
| one-large-file, one file of 256 MiB | 629 ms | 268,435,456 | 268,435,456 |

Four mebibytes spread across four thousand files takes a hundred and sixty times
as long as two hundred and fifty-six mebibytes in one file. That is roughly
twenty-five milliseconds per one-kilobyte file. It is the worst number this
project has produced and it is published rather than buried, because
standards.md says published numbers include the regimes where Fetchloom is
slower.

It is not diagnosed here and nothing is optimized on the strength of it. What is
known: the ingest path touches each file about six times, writing a scratch
file, renaming it to a partial, renaming that to an object, sealing it,
recording a fingerprint beside it, and then cloning it into the destination. On
a volume where every file close is inspected, per-entry cost is multiplied by
that count. The speculative ingest write already has its own record and is one
of the six. Whether the remainder is the scanner, the rename chain, or the
per-file fingerprint record is a profile that has not been run, and guessing at
it is what standards.md forbids.

Costs: the regime takes a hundred seconds per iteration on this machine, so it
runs at one iteration rather than the default nine.

Uncertain: what the same corpus costs with real-time protection off, which is
the comparison that would separate the scanner from the code. It was not
measured, because turning off a machine's virus protection to make a benchmark
look better is not a measurement anyone should trust, and the honest number is
the one a user actually gets.

Sources: `cargo xtask bench --iterations 1` on this machine;
`Get-MpComputerStatus`; `fltmc filters` returning `0x80070005`;
`crates/platform/src/windows/probe.rs`; contracts.md Platform capabilities.

## The small-files profile, and the three things it actually found

Question: The many-small-files regime measured 4 MiB across 4096 files at a
hundred seconds, against 256 MiB in one file at six hundred milliseconds. The
record filing that number said it was not diagnosed and that guessing was
forbidden. This is the profile.

Method: the release binary against 1024 distinct one-kibibyte files, decomposed
by flag, then by scaling curve, then against a hand-written replica of the same
filesystem sequence, then against the real platform primitives measured
directly. Every number below is from this machine with Defender real-time
protection confirmed on.

What was falsified first. The durability flush was the obvious suspect, because
the default tier issues `FlushFileBuffers` per file. It is not the cost:
`--durability fast`, which issues no flush at all, measured 26,235 ms against
`normal` at 19,345 ms, which is to say no improvement and a difference inside
this machine's noise. A hypothesis that survives only because nobody measured it
is worth exactly nothing, and this one did not survive.

What the decomposition showed:

| Run, 1024 files of 1 KiB | total | per file |
|---|---|---|
| cached, normal | 19,345 ms | 18.9 ms |
| cached, fast | 26,235 ms | 25.6 ms |
| no cache, normal | 1,904 ms | 1.86 ms |
| raw `cp -r` | 1,150 ms | 1.12 ms |

The cache path is ten times the cost of the same run without it, and the run
without it is within a small factor of what the operating system charges to copy
the tree at all.

The scaling curve is where the real answer is. Without the cache the per-file
cost is flat: 1.70, 1.59, 1.72 and 1.63 milliseconds at 64, 256, 512 and 1024
files. With the cache it climbs: 8.81, 10.21, 17.76 and 21.22. The cost per file
grows with the number of files, which no amount of per-file work can explain.

A hand-written replica of the cache's exact filesystem sequence, using nothing
but the standard library, reproduces the same climb: 4.5, 4.4, 7.0 and 11.2
milliseconds at the same four sizes. So the superlinearity is not in Fetchloom's
logic at all. It is what this filesystem charges as the directories fill, and the
cache fills them fast, because it writes four files for every one object: the
object, a lock, a lock owner record, and a fingerprint record. A thousand source
files leave four thousand and ninety-eight files in the cache, counted.

Directory fanout, which is what git, restic and casync all do for this exact
reason, was measured and is not adopted here. Across repeated runs the fanout
variants did not separate from the flat ones by more than this machine's own
variance, which reached forty percent between identical runs. Adopting a layout
change on evidence that noisy would be guessing with extra steps. It is written
down as the first thing to try on a quiet machine.

Three things were fixed, each justified by counting rather than by a clock:

The clone capability was asked per file. `clone_or_copy` attempted a
copy-on-write clone for every entry, and on NTFS that attempt opens the source,
stats it, opens it a second time to query the volume, learns that the volume
does not reference-count blocks, and fails. Whether a volume reference-counts
blocks is a property of the volume. It is now asked once per volume and
remembered. Measured against the real primitive: `clone_or_copy` cost 1.670 ms
per file against 0.837 ms for the plain copy it falls back to, so the failed
attempt was doubling the cost of every placement.

The same fact was reported a thousand and twenty-four times. Because the clone
was attempted per file, the run emitted one `degrade` event per file, all
identical, naming a volume-level fact. Counted before: 1024. Counted after: 1.
standards.md says to say a thing once, and an event stream that repeats one
sentence a thousand times is not an event stream anyone can read.

The ingest renamed twice where once suffices. The scratch file is written into
`partial/`, renamed to `partial/<digest>`, then renamed again into
`objects/<digest>`. The contract says publication is a write to `partial/`, a
flush, then an atomic rename into `objects/`, and the scratch file already
satisfies the first clause, so the middle rename bought nothing. Removed.

Costs, stated plainly: none of the three is provable as a speedup on this
machine, and none is claimed as one. The wall clock after the changes read
22,347, 22,358 and 30,794 milliseconds across three runs, which is not
distinguishable from the 19,345 before it. What is provable is that the run does
strictly less work: one volume query instead of one per file, one degrade event
instead of a thousand, one rename per object instead of two. A machine quiet
enough to show that as time does not exist here.

Uncertain, and the honest gap: the four-files-per-object layout is the cause of
the superlinearity and it is still there. Fanout, packing small objects the way
a packfile does, and dropping the separate owner record are all real options and
none of them can be chosen on this machine's numbers. That is the first work of
any phase that has a quiet runner.

Sources: every measurement in this record was run on this machine and is
reproducible from `crates/cli/tests/work.rs` plus the flag decompositions above;
`Get-MpComputerStatus` for the scanner state; the record on the small-files
regime for the original number.

## What the rest of the field does about four files per object

Question: The profile found that the cache writes four files per object and that
per-file cost climbs as those directories fill. Is that a Fetchloom problem or a
known one, and what is the known answer.

Chosen: it is the known one, the known answer is packing, and packing is not
done in this phase.

Because: git stores an object per file until it does not. Its own engineering
writing says that as a repository grows, storing objects loosely "becomes
infeasible, as it strains the filesystem to have so many files", and that reading
one packfile is faster than reading many loose objects; `git gc` and `git repack`
exist to consolidate them. Hosting providers report the same failure mode
concretely, where a fetch that leaves loose objects behind forces later commands
to read thousands of small files. restic reaches the same place from the other
direction: it never writes a file per blob at all, cutting data into blobs
averaging a mebibyte and grouping them into pack files.

So the shape of the answer is settled by everyone who has hit this: do not put
one small object in one file. What is not settled is whether Fetchloom should,
because the two systems above pack for reasons Fetchloom does not share. Git
packs to delta-compress related versions of a text file; Fetchloom stores whole
verified artifacts and deltas between them are not a thing it has. restic packs
partly to hide chunk sizes from an attacker; Fetchloom's cache is not adversarial
in that direction. What both get incidentally, and what Fetchloom would be
adopting them for, is one file instead of thousands.

Why it is not done here: a pack file is a second way for an object to exist, and
every reader of `objects/` would have to know about both. contracts.md says an
entry in `objects/` has been fully verified and that there is no other way for a
file to appear there, and the whole prune, lease and fingerprint model is written
against a file per digest. Changing that is a cache format change, which the
format fingerprint makes safe to do, but it is not a change to make while the
only evidence for it is a machine whose repeated runs of identical code differ by
forty percent.

What is recorded instead, so the next person does not start from nothing: the
cost is four files per object, counted exactly, of which the object itself is one
and three are metadata that a pack index would fold into a shared file. The
scaling curve that proves the problem is in the record on the small-files
profile. Directory fanout was measured here and did not separate from noise; it
is the cheaper half-measure and it is still worth trying first on a quiet
machine, because git uses it too and it needs no second way for an object to
exist.

Uncertain: whether the fingerprint record can simply be dropped rather than
packed. It exists to make `--verify fingerprint` cheap, which is the default, and
it is one of the three metadata files. Whether the same tuple could live in the
object's own filesystem metadata rather than beside it is a per-platform question
nobody has asked here.

Sources: GitHub's writing on the packed object store and Git's own packfile
documentation; restic's design document on blob sizes and pack files; the record
on the small-files profile for the measurements this rests on.

## Phase 3 gate

What phase 3 was for: extraction is where hostile input meets the filesystem,
and where most tools are unsafe. What it delivers is an archive reader, bounded
extraction into staging, selection, reconcile, and the two backlog items phase 2
left behind.

What passed, and on what.

The hostile corpus is forty-five named archives, each carrying its bytes, what it
attacks, and either the exact error kind from contracts.md with the member the
error must name, or the fact that it is benign and must extract cleanly. Every
one is driven through the reader, and every one through reader-then-extraction
into a real staging directory. Nothing is skipped, and the split between what
the reader decides and what only a destination can decide is written down as a
list asserted by name in both suites, so neither can quietly stop covering an
entry. Four hundred and eleven tests pass on `x86_64-pc-windows-msvc`.

Ten formats ship and each is read back from bytes it really holds: tar bare and
wrapped in gzip, zstd, xz and bzip2; zip with store and deflate; and the four
compressions alone. Every decoder is pure Rust and no build script compiles C,
which is the lesson phase 2 paid for when `ring` cost this machine two Apple
targets.

`get https://` resumes. A partial is named by the key the run knows, which is a
content digest when the reference states one and a digest of the source identity
when it does not. The `degrade` that phase 2 fired on every remote fetch is
deleted, and a run that is interrupted until its retries are spent resumes on the
second attempt at rung three with bytes kept.

Running `get` twice against an unchanged destination writes exactly zero bytes,
asserted on zero rather than on a comparison, and reports `unchanged` with exit
zero. That is the headline of this phase and it is the thing `destination.foreign`
used to make impossible.

Three deterministic counters now ride in every result and gate at five percent:
bytes read from a file, bytes written to one, and requests issued to a source.
They found three real defects on their first runs. A warm cache grows by exactly
zero and still writes the whole corpus, because the ingest writes before it knows
the digest. A cold transfer of one object issued three requests where two suffice.
A copy-on-write clone was attempted per file on a volume that can never clone,
which doubled the cost of every placement and emitted one thousand and twenty-four
identical `degrade` events for one volume-level fact.

Two of those three are fixed and the fix is proven by counting rather than by a
clock: requests per cold transfer went three to two and per interrupted transfer
seven to six with bytes materialized unchanged, and degrade events went 1024 to 1.
The third is recorded and deliberately not fixed, because the change trades a
write on every cache hit for a read on every miss and no measurement here can say
which is better.

A correctness defect this phase found and fixed: `create_symlink` had no caller in
any materialization path. A tree containing a symbolic link was walked, recorded
in the tree digest, and never created, so the destination did not reproduce the
digest the run reported. The walk discarded the target bytes, so the link could
not even have been recreated from what was kept. Links are now created, last, and
a test materializes a tree holding one and asserts the destination verifies back
to the tree the run reported.

The costs, and they are the honest gap.

Nothing ran on macOS or on a real Linux machine this phase. The container lane
covers the two Linux targets and Apple is compile-only, exactly as phase 2 left
it. The Windows-reserved-name guard and the case-folding collision heuristic in
extraction are verified on Windows alone.

Many small files cost about twenty milliseconds each with a scanner enabled,
against one large file at roughly two microseconds per kilobyte. That is the worst
number this project has produced and it is published rather than buried. It is
profiled rather than guessed at: the cause is that the cache writes four files per
object and this filesystem charges more per operation as those directories fill,
which the no-cache path does not do and which a hand-written replica of the same
sequence reproduces exactly. Packing small objects is what git and restic both do
about it and it is not done here, because it is a cache format change and the only
evidence available is a machine whose identical runs differ by forty percent.

The scanner answer on this machine is `unknown` rather than `present`, because
enumerating minifilters needs elevation and an ordinary shell is refused. Defender
was confirmed on out of band, so the small-files number is a scanner-enabled
number even though the probe is not permitted to name the scanner.

Expansion ratio is enforced by the reader and not again by extraction, because
extraction cannot see the archive's on-disk length through the Archive seam.

What phase 4 inherits, as this record first stated it, and see the amendment at
the end of this file for what has since changed. The speculative ingest write. The four-files-per-object
layout and the packing question. Rung two, still, because no source here publishes
an immutable identity. Two Apple crates no machine compiles. A destination
fingerprint store, which reconcile does without by hashing, and which phase 4's
receipt may or may not change.

Sources: `cargo xtask verify` on this machine; `cargo test --workspace`;
`cargo xtask bench`; `cargo deny check`; and the profile, formats, rejection,
reconcile, selection, partial key, counters and small-files records above.

## What real archives did that the corpus never asked about

Question: The hostile corpus is forty-five archives this project wrote itself,
and every one passed. Does `get` read an archive that an ordinary tool produced.

Chosen: it did not, in three ways, and the corpus could not have found any of
them, because a suite that only reads its own output tests the writer as much as
the reader.

What a real tool produced, and what happened.

`tar -C dir .`, which is how most people tar a directory, writes every member
with a `./` prefix and a bare `./` for the root. Every one was refused, because
`.` is a relative component and an entry path forbids those. The rule is right
for `..` and wrong for `.`: a single dot names the directory it is already in, so
`./a` and `a` are one entry. Worse than the refusal is what accepting it
unchanged would have meant, which is two different tree digests for two archives
holding identical files. The canonical member path now drops `.` components, and
a member that is nothing but dots is the archive root and is not an entry at all.
There is a test asserting that a plain tar, a `./`-prefixed tar, and a pax tar of
the same three files produce one tree digest, and they do.

`tar --format=pax`, which is the POSIX format and what modern tar writes by
default, attaches an `mtime` record to every member. Every pax archive was
refused, because the reader allowed three pax keys and called everything else an
extended attribute. contracts.md is explicit that timestamps are excluded from a
tree, which is not the same as an archive stating one being an error. The reader
now discards `path`, `linkpath`, `size`, `mtime`, `atime`, `ctime`, `charset` and
`comment`, refuses `uid`, `gid`, `uname` and `gname` as the ownership the
contract does forbid, refuses a `SCHILY.xattr.` record as the extended attribute
it is, and refuses a `GNU.sparse.` record because a sparse member is one this
build cannot reconstruct and quietly writing its holes as zeroes would be a
different file. Anything else is refused by name rather than assumed harmless.

`Compress-Archive`, the zip command that ships with Windows, writes member names
separated by backslashes. Those were refused outright, which was the decision
rather than an oversight and has since been narrowed by one case; the reasoning
below is why the general rule stands and the amendment at the end of this file is
why the one case does not fall under it. The zip specification says in 4.4.17 that all slashes
in a stored name must be forward slashes, so such an archive is malformed;
Microsoft's own .NET changed its writer to conform in 4.6.1 rather than teach
readers to guess. A backslash is a legal byte in a member name on Unix, so a
reader that treats it as a separator is inventing structure the archive did not
state, and inventing structure is how a path escape gets through. What was wrong
was the message, which said only that the name contained a backslash and left the
user with no idea their zip was malformed or what to do. It now says to repack
with a writer that uses forward slashes, and why the archive cannot be read as
written.

A fourth thing this found, on the other side: the reserved-name list. Extraction
carried a table of Windows device names and refused `CON` outright. On this
machine a file named `CON` is created and listed back exactly, so the volume
represents it and contracts.md's `destination.unrepresentable` does not apply.
That table was also the thing the collision record already said not to build, a
table Fetchloom would have to keep correct. The list now decides only which names
are worth confirming, never the verdict: a suspicious name is created and the
directory is read back, and the name is refused only when the volume stored
something other than what was asked for. That is what catches the two cases that
genuinely are unrepresentable, where Windows silently drops a trailing dot and
turns a colon into an alternate data stream on a shorter name, and it accepts
`CON` because this volume genuinely stores it.

Cleanup changed with it. Removing the names extraction recorded is not enough
when the volume stored a different one, because the file that exists is not a
file this run can have recorded. Staging is documented as given empty, so a
failure now empties it rather than unwinding a list.

Costs: the confirming read of a directory happens for a name that looks
suspicious, which is a syscall those members do not otherwise need. It is bounded
by how many such names an archive holds, which for every real dataset is none.

Uncertain: whether a volume exists that stores a trailing dot, in which case
Fetchloom will accept a name that most Windows software cannot open. That is the
measurement being trusted over the table, deliberately, and the run says what it
did.

Sources: GNU tar 1.35 output on this machine for the `./` and pax forms;
`Compress-Archive` on this machine for the backslash form; the zip specification
section 4.4.17; Microsoft's documentation of the .NET 4.6.1 separator change; the
pax keyword list the `tar` crate defines; contracts.md Materialization.

## Where a mode comes from, and where it cannot come from

Fetching `hello-2.12.tar.gz` from ftp.gnu.org and immediately running `verify` on
what it produced gave two different tree digests. That is the one hard rule in
vision.md broken on the first real archive anyone fetched, and no test in this
workspace saw it, because every test that walks a tree walks one this workspace
wrote and no such tree carried an executable.

The cause was one function. `walk` read a file's mode from a stat: the real bits
on Unix, `0644` on Windows, which has no bit to read. `get` on an archive took
`0755` from the archive header as contracts.md requires, and `verify` walked the
destination and got `0644` back on Windows. The same walk also made Windows and
Linux disagree about a plain source directory holding an executable, which nobody
had noticed either.

contracts.md was not silent about this. It says mode is taken from the source
archive or manifest rather than from a destination stat. A stat was being read
anyway.

The decision is that a bare filesystem tree states no mode at all. It is neither
an archive nor a manifest, so there is nothing there to take, and reading the bit
where a volume happens to carry one produces a tree that digests differently on
Windows and on Linux. Every file a walk finds is `0644` on every platform, and
the run says with `degrade` that it read none. That fixes the source-directory
disagreement outright: one directory holding an executable now digests the same
on all three platforms, which it did not before.

It does not make `get` and `verify` agree for an archive that states `0755`, and
nothing in this phase can. `get` has the archive and must report what the archive
said. `verify` has a path and, on Windows, has no way to learn that a file was
meant to be executable, because the volume does not carry the fact. There are
only three answers available. Read the stat where the platform has it, which is
what was there and makes Windows and Linux disagree. Refuse to answer, which
removes a command that has worked since phase 1. Or answer with the modes a walk
can state and say so. The third is the only one that keeps one answer on all
three platforms, and the gap it leaves is exactly the gap a receipt closes,
because a receipt is the thing that remembers what the source said. `verify`
compares against a receipt in phase 4. Until then it reports a tree with every
file at `0644` and emits a `degrade` naming that it read no mode.

Reconcile follows from the same rule rather than from a second one. It takes each
entry's mode from the tree the run resolved and never from what it found, so a
mode is not a reconcile signal on any platform. contracts.md already described
that consequence for a platform that cannot represent the bit; it is now true
everywhere, which is one behavior instead of two.

Costs: a mode change in a destination is invisible to reconcile on Linux, where
it previously would have been caught. That is the price of one answer on three
platforms, and it is the price contracts.md already named. A `verify` digest and
a `get` digest of the same archive tree are two different numbers until receipts
land, and anyone comparing them by hand will be confused; the `degrade` is there
to say why.

Sources: contracts.md Materialization; `hello-2.12.tar.gz` fetched from
ftp.gnu.org on this machine; vision.md on one tree digesting the same everywhere.

## Running it twice against an archive

The second run of `get` against a directory reported `unchanged` and wrote
nothing. The second run against an archive did not reconcile at all: it found the
destination present and failed with `destination.foreign` before looking at what
the destination held. The phase 3 headline was true for half the sources it
claimed.

Reconcile needs the tree a request resolves to before it decides whether anything
needs writing, and for an archive that tree was only obtainable by extracting,
which writes. Contracts require that an unchanged run write nothing at all, no
staging directory and no rename, so extracting first was not available.

The archive crate now answers the question without writing. `resolve` walks the
same selection and plan extraction walks and hashes each selected member's bytes
as they stream past, producing the entries extraction would have written and
creating nothing. The two paths share the member listing, the selection, the
canonical path, and the plan, so there is one answer to what an archive holds and
two things done with it rather than two answers.

One thing `resolve` cannot find is a name the destination volume refuses, because
only creating the name on that volume finds it, which is the whole point of the
collision decision. That is not a gap: `resolve` answers what the request
resolves to, and any outcome that needs writing goes through extraction, which
answers what the volume accepts. A destination that already holds the tree was
written by an extraction that already asked.

Costs: an unchanged second run of an archive reads and decompresses the whole
archive to learn it has nothing to do, and hashes the destination to compare. It
writes nothing, which is what was promised, but it is not free, and a receipt is
what makes it free later.

## A lane that fetches something nobody here wrote

Nothing in this project had ever fetched from a server it did not also run.
Forty-five hostile archives, a local test source, and an adversarial server that
agrees with the client by construction. The mode defect and the archive
reconcile defect both survived every one of them and died to four commands
against ftp.gnu.org.

`cargo xtask verify` now carries a `network` lane that fetches three real
archives and holds each to a tree digest recorded in the task. It asserts the
fetched tree, the entry count, that a second run reports `unchanged` and writes
zero bytes, and that `verify` reports its own recorded tree. The subjects are
`hello-2.12.tar.gz` and `tar-1.34.tar.xz` from the GNU FTP archive, which keeps
every release it has ever published, and `six-1.16.0.tar.gz` from PyPI, whose
files are immutable once uploaded and whose `packages/source` form redirects, so
the lane exercises a redirect against a server nobody here wrote.

A host that cannot be reached skips the lane and says so, and a skipped step is
counted as neither a pass nor a failure in the summary. A lane that passes when
it ran nothing is worse than no lane.

Costs: the gate now depends on two external hosts. When they are down the gate
reports a skip and a degradation rather than green, which is the honest answer
and is also a gate that can no longer be fully green offline. The recorded
digests are values this build produced rather than values derived from the
specification, so they lock a regression rather than prove a truth; what makes
them worth recording is that the Linux lane must produce the same ones.

## One decompression for the archive, not one per member

The network lane found this the first time it fetched an xz archive. Fetching
`gzip-1.12.tar.xz`, 825 KB holding 515 members, took two minutes and forty-five
seconds. The same fetch of a gzip archive of similar size took seconds.

The cause was not xz. `open_member` rewound the source and rebuilt the
decompressor for every member, then read forward to that member's offset. For
515 members that is 515 decompressions of the stream prefix, which is quadratic
in the member count and hidden entirely behind gzip being cheap enough that a
462-member `hello-2.12.tar.gz` still finished. Nothing in the corpus has more
than a handful of members, so nothing in the corpus could show it.

The reader now holds one decompressed stream open between members and moves
forward through it. A member that sits behind where the stream has already
reached rebuilds it, which happens only for a hard link naming an earlier member,
and extraction otherwise reads members in the order the archive holds them. The
member body advances the stream's position by exactly what it yielded, so a body
dropped before it is exhausted leaves the stream describing itself correctly
rather than silently desynchronized.

Measured, same machine, same debug binary, same archive:

  before  2 m 45.6 s
  after         8.9 s

That is 18.6 times, and the tree digest is byte-identical either way
(`blake3:efed3329…`), which is the only thing that makes the number worth
anything.

Costs: the stream is now shared state between the reader and the body it handed
out, held behind the same `Rc<RefCell<…>>` the source already used. Two bodies
open at once would interleave; the seam already documents that a caller reads one
body to completion before opening the next, and both callers in this workspace do.
An archive read out of order pays the old cost for the members that go backwards.

The archive is still decompressed twice overall, once to list members and once to
read them, which the compressed-tar record already covers and which a member
index would close.

## Not writing what the cache already holds

`get` of a local archive ran twice reported `unchanged` and still wrote the whole
archive into the cache, because `ingest` wrote every byte to a scratch file and
only then learned the digest, discovering the object was already there and
deleting what it had written. That was recorded during phase 3 as a known cost and
left alone, on the grounds that nothing here could measure whether a write per hit
beat a read per miss.

It is no longer a question of measurement. Contracts say a run with nothing to do
writes nothing, and the deterministic counters are what the gate reads. A second
run that reports `unchanged` while `bytes_written` equals the size of the source
is not telling the truth about what it did.

`ingest` now reads the file once to learn its digest, writing nothing, and only
reads it a second time to store it when the cache does not already hold it. Cold
costs one extra read of the source; warm costs no write at all.

  cold   read 2x, write 1x
  warm   read 1x, write 0x

Before, both ran read 1x, write 1x. Anything fetched more than once is ahead;
anything fetched exactly once pays one extra sequential read, which on every
platform here comes back out of the page cache the write just filled.

A remote object cannot do this, and the network lane says so rather than
pretending: nothing yet tells this build that a location still holds the object it
already has without downloading it again. That is rung two of the resume ladder
and it needs a source that publishes an immutable identity. Until then, the lane
asserts what the contract actually promises for a second run, which is that
nothing under the destination moved, by comparing every path, length, and write
time across the two runs.

## Amendment to the phase 3 gate: what four real commands found

The phase 3 gate above was recorded against a green matrix, four hundred and
sixteen passing tests, and a clean tree. It was wrong about three things, and all
three were found by pointing a release binary at ftp.gnu.org and typing four
commands. This amends that record rather than replacing it, because what it got
wrong is more useful than what it got right.

**`get` and `verify` disagreed about the same directory.** Fetching
`hello-2.12.tar.gz` and verifying what it produced, seconds apart, gave two
different tree digests. `walk` read each file's mode from a stat, which
contracts.md forbids in the sentence that defines the field. Windows and Linux
also disagreed about a plain source directory holding an executable. The mode
record above says what a mode may come from now and what `verify` cannot answer
until a receipt exists. No test caught it because no tree this workspace writes
carried an executable, and the conformance corpus that does hold one is never
driven through a walk.

**The headline was true for half its sources.** `get` run twice against a
directory reported `unchanged` and wrote nothing. `get` run twice against an
archive, local or remote, failed with `destination.foreign` without ever looking
at what the destination held. Reconcile needed a tree, and for an archive the only
way to get one was to extract, which writes. The archive crate now answers it
without writing, and both paths reach the same four outcomes through one
settlement.

**A relative `--output` failed and lied about why.** `Path::parent` answers
`Some("")` for a single-component relative path, and an empty path handed to a
filesystem call is not the working directory it stands for. The failure surfaced
as `cache.corrupt` with a next action that began with a bare colon and named
nothing. `--output` and `--cache-dir` are now resolved against the working
directory once, where the user names them, rather than travelling through joins
and parents in relative form, and the seam no longer receives a path it cannot
open. The kind was wrong because the platform seam labels every path it cannot
open `cache.corrupt`, which is right for the cache paths that are most of its
callers and wrong for a destination; the seam cannot tell them apart, so the fix
is to never hand it an unresolvable path rather than to have it guess better.

Two things the gate listed as inherited by phase 4 are done. The speculative
ingest write is fixed and the reason it stopped being a measurement question is
recorded above. Reading one archive member no longer costs one decompression of
the whole prefix, which was quadratic and which nothing in a forty-five-archive
corpus of small archives could show.

One thing the gate stated is now narrower. A zip whose member paths hold a
backslash and no forward slash anywhere is separator-written by evidence rather
than by assumption, and its backslashes become forward slashes before any
rejection is applied, so a traversal member is still refused as a traversal. A zip
holding both is ambiguous and still refused, and a tar is never translated. The
cost is real and accepted: a Unix member genuinely named `weird\name.txt`, alone
in an archive with no directories, is reinterpreted as `weird/name.txt`, and the
run says so.

What phase 4 inherits after this amendment: `verify` against a receipt, which is
the only thing that can make `get` and `verify` agree about an archive stating
`0755`. The four-files-per-object layout and the packing question. Rung two, and
with it a remote second fetch that does not download what the cache already holds.
Two Apple crates no machine compiles. The archive is still decompressed twice,
once to list and once to read, which a member index would close.

The lesson, and it is the one the `./` and pax findings were already teaching: a
corpus you wrote yourself tests your writer as much as your reader. The gate now
carries a `network` lane that fetches three archives nobody here made and holds
each to a recorded tree digest, and it skips rather than passes when no host can
be reached.

## The write the counter could not see

Inverting the local cache path made the benchmark fail, which is the counters
doing their job and is also the more interesting half of the story.

The first attempt at not writing what the cache already holds was to hash the
source, check, and only write on a miss. That works and the warm run writes
nothing, but it costs a second read of every source on a cold run, and the gate
said so: `cold-cache bytes-read` went 16 MiB to 32 MiB. Trading a read for a write
is defensible and it was still the wrong shape.

The right shape is that the run has to write the destination regardless. So the
source is read once and written straight to its destination, hashed in the same
pass, and the cache is then handed that file rather than the source. `adopt` takes
a digest the caller just computed, shares blocks with the file where the volume
can, and does nothing at all when the cache already holds the object.

Real bytes moved, on a volume that cannot clone:

  before   read source, write cache scratch, rename, copy object to destination
  after    read source, write destination, copy destination to cache scratch, rename

That is the same work, and on a volume that can clone the copy in the second line
is a block share and the after column is strictly less. On a warm cache the after
column stops after the second step: no cache write at all, where before there was
a full one.

The counter then reported `cold-cache bytes-written` rising 16 MiB to 32 MiB, and
that is not more work. It is work that was always happening and that the counter
could not see: `clone_or_copy` goes through the Platform seam, which holds no work
counter, so the copy from the cache object to the destination was invisible. The
old 16 MiB was an undercount of a 32 MiB operation. The baseline is re-recorded
rather than the change reverted, because the number changed meaning and the new
meaning is the honest one.

`adopt` counts the length it adopted rather than what the volume physically wrote,
so a cloning volume and a copying volume report the same figure. That is
deliberate: a metric the gate compares across machines has to be a property of the
inputs, not of the filesystem underneath.

What this says about the metric, which the counters record above already began:
`bytes_written` covers what the cache and the materializer write through their own
paths and does not cover what the Platform seam moves on their behalf. It catches
a change in how much this program writes; it does not catch a change in how much
a clone falls back to a copy.

Wall time is not gated and this is why: three consecutive runs of the unchanged
binary on this machine measured one-large-file at 773 ms, 2364 ms, and 4165 ms.
Five times, same code, same input.

## A cache that cannot lock stops the run

The container lane failed on its first network fetch with `cache.corrupt` and the
advice to run without `--no-cache`, which the user had not given. The cache root
was under the bind-mounted workspace, the platform correctly saw a network volume
whose locks cannot be trusted across the machines that share it, and `cache::open`
degraded to running without a cache. Every remote fetch then failed, because this
build streams a remote object through the cache and there was none.

contracts.md is not ambiguous here: a cache on a filesystem that cannot express
cross-user locking is refused with `cache.locking_unsupported` rather than used.
It is now refused, alongside the format mismatch that was already refused, and for
the same reason: degrading produces a run that cannot do the thing it was asked
to do and then blames a flag nobody typed.

The lane itself also moved off the workspace and into the platform's temporary
directory, which is where a test that writes hundreds of megabytes belonged
anyway.

Both halves matter. The refusal is the contract. The lane's location is why the
contract was reachable at all: nothing before it had ever run this binary against
a real server from inside the container.

## A receipt lives in the cache, supplies modes, and never supplies a digest

Question: `verify <path>` reported `0644` for every file and a degrade with it,
because a filesystem tree states no mode. That made `verify` unable to confirm
any tree that came out of an archive, which is the thing `verify` is for. A
receipt records what a run materialized. What may it be used for.

Answer: a receipt supplies the mode of each file and nothing else.

The circularity a receipt invites is real and it is avoided by taking exactly one
thing from it. Verify walks the destination, hashes every file, and builds the
entry stream from what is actually there: the paths, the sizes, the content
digests, the entry set. The one field it cannot read back from a filesystem is
the mode, because a volume that carries no executable bit answers differently
from one that does, and phase three already refused to guess. The receipt states
that field, and the tree digest that comes out is then the digest `get` reported.
The receipt's own `tree` is compared against the result and never substituted for
it. A record that attested to its own correctness would prove nothing, and this
one does not: delete a file, change a byte, add an entry, and the recomputed
digest moves while the receipt does not.

The cost is stated rather than hidden: a mode that changed on disk after the run
is not detected, because nothing here reads a mode. That is the same cost phase
three accepted when it stopped reading modes from a stat, and it is the price of
one tree digesting the same way on Windows and on Linux.

Where a receipt lives. contracts.md says a receipt is local, may hold absolute
paths, and is never committed. It does not say where it is kept, and there were
three candidates. Inside the destination is wrong: it would become an entry of
the tree it describes. Beside the destination is wrong: it writes into a
directory the user did not ask to be written into, and reconcile would then call
it foreign. So it lives in the cache, under `receipts/<hex>` where the hex is a
derived-key digest of the absolute destination path.

That makes a receipt derived data, and it is: running the same request again
produces the same one. `cache clear` discards receipts and the degrade returns,
which is honest, because a discarded receipt costs a rerun and never a wrong
answer.

Three cases, decided and asserted by name.

No receipt. The degrade stands, every file is `0644`, and nothing is compared.
This is the phase three behavior and it is not deleted.

A destination that moved. The receipt is named by the path, so a moved
destination finds nothing and the degrade stands. There is no search, because a
search would have to guess which of several receipts describes this directory.

A receipt from another machine. It is used only when its own `destination` field
is the path being verified. A receipt copied here from elsewhere names that
machine's path and is ignored. If the paths do coincide, it is used, and that is
safe for exactly the reason above: it supplies no digest, so the worst it can do
is state the wrong mode for a file and produce a digest that does not match its
own recorded tree, which fails.

Sources: contracts.md Receipt and Verification policy; the phase three amendment
that named this gap; `crates/cli/tests/mode.rs`.

## One canonical form, ordered by key, with no line ending in it

Question: the manifest digest appears in every lock, and it covers the canonical
form rather than the source text. What exactly is that form.

Answer: canonical JSON, ordered by the raw bytes of every mapping key, with no
whitespace, no null, and no line ending.

Ordering by key rather than by declaration is the decision that could have gone
the other way. contracts.md's examples list a lock's fields as manifest, release,
artifacts, tree, and a form ordered by declaration would print them that way.
Ordered by key it prints artifacts, manifest, release, tree, which reads slightly
worse. It was taken anyway, because declaration order is a property of a Rust
struct and a canonical form may not depend on one. Reordering two fields in a
source file would otherwise change the digest of every manifest in the world, and
nothing in review would catch it.

No line ending is the answer to the thing that was going to bite. A canonical
form holding a newline has to state which newline, and a Windows editor, a git
checkout with `core.autocrlf`, and a shell redirect each have opinions. The
canonical form holds none: it is one line with no separators, so there is nothing
for a platform to disagree about. The test asserts on the absence of `\n` and
`\r` in the bytes rather than on their equality across two machines, which is the
stronger statement and the one a single machine can make.

The text form written to a lock, a receipt, and a plan does hold line endings, and
it always writes one line feed. It is read back tolerantly: a carriage return
before a line feed is stripped, so a file that travelled through a checkout still
parses to the same model and digests the same. Tolerant on the way in, exact on
the way out.

Every scalar in the written form is double-quoted with JSON escaping. That is one
rule rather than a decision per value, so nothing has to reason about whether a
digest, a glob, or a release string needs quoting, and nothing read back can mean
something other than what was written. A mapping key is written plain when it is
alphanumeric with underscores, hyphens and dots, and quoted otherwise, because a
lock whose every key was quoted is harder to read for no gain in exactness.

Sources: contracts.md Compatibility and Canonical form;
`crates/engine/tests/document.rs`.

## A YAML subset written here, rather than a YAML library

Question: manifests parse from YAML, TOML, and JSON. TOML and JSON already have
parsers in this workspace. YAML does not. Add a dependency or write one.

Answer: write one, for the restricted subset contracts.md already describes.

contracts.md requires refusing anchors, aliases, merge keys, and implicit boolean
coercion of strings, and bounding depth and node count. A general YAML library
does the opposite of three of those by design: it resolves anchors and aliases,
applies merge keys, and coerces `yes` to a boolean under YAML 1.1 rules. Using
one would mean parsing the document, then scanning the source text for the
constructs the library had already silently resolved, which is two passes and a
guess. The parser here refuses them where they appear, by name, with the line
number.

It is also the smaller commitment. The subset is block mappings, block sequences,
flow sequences, flow mappings, and three kinds of scalar. That is what the
manifest, lock, receipt, and plan shapes need and nothing else, and every line of
it is covered by tests that assert the refusals rather than only the acceptances.

The dependency that was added instead is `serde_json`, which the workspace
already carried, promoted from a development dependency to a real one. It is the
canonical form's own writer and the intermediate every surface syntax parses into,
so there is one model and three front doors rather than three models. `toml` was
already the configuration parser and is now also one of those doors.

Sources: contracts.md Manifest and Canonical form; `crates/engine/src/yaml.rs`.

## Portable versus local, field by field

Question: a lock carries no absolute paths, no local hostnames, no usernames, and
no timestamps of the local run. Which fields is that, and where does each of the
things a run knows go.

Answer: the table, decided before the code was written.

| Field | Where | Why |
|---|---|---|
| dataset name | lock | The last component of a reference, which is the same everywhere |
| `manifest` | lock | A digest of a model that holds no path |
| `release` | lock | The publisher's word |
| `digest`, `interop`, `size` | lock | Properties of the bytes |
| `select`, `layout` | lock | Part of identity, and identical on every machine |
| `tree` | lock | The same on every platform or the run fails naming what cannot be represented |
| `source_used` | receipt | Which of several sources answered here |
| `trust` | receipt | What was known here, at this moment |
| `destination` | receipt | An absolute path |
| `executable` | receipt | What a filesystem does not state |
| `completed_at` | receipt | A wall clock |
| `fetchloom` | receipt | Provenance |
| `cached`, `disk`, `conflicts` | plan | This machine's answer, reported and never acted on |
| fingerprint | neither | A cache of the phrase probably unchanged |

The subtle one is the manifest digest for a reference that names no manifest. A
run against `https://host/x.tar.gz` has no manifest, and a lock entry needs one.
The answer is a synthesized manifest holding the dataset name and one artifact
carrying that name and, for a network reference, that location. A location is
portable and belongs there.

A local path is not, and this is where a machine would have leaked. Synthesizing
`sources: [D:\data\raw]` would have produced a manifest digest that differed
between two people holding the same bytes, and the difference would only surface
the moment two people compared locks. So a local reference records no source at
all, and its synthesized manifest reduces to the dataset name. The lock it
produces pins the bytes and says nothing about where they sat.

The test asserts on the bytes rather than on the fields: the lock is read back as
text and searched for the destination, the cache path, the value of `USERNAME`,
`USER`, `COMPUTERNAME` and `HOSTNAME`, for `completed_at`, for `destination`, and
for a backslash, which is how a Windows path leaks in. A second test fetches one
source into two scenes and asserts the two locks are byte-identical.

Sources: contracts.md Lock and Receipt; `crates/cli/tests/lock.rs`.

## What a locked run compares, and which failure each difference is

Question: `--locked` fails if resolution differs from the lock. Differs how, and
which of the exit codes does each kind of difference take.

Answer: two kinds, and the split is whether refetching could fix it.

A difference in the bytes is an integrity failure and exits 30. The lock stated a
digest, the source served something that does not hash to it, and the run has been
lied to by a source. `digest`, `interop`, and `size` are that, and so is `tree`,
because a tree that differs from the pinned one means the same bytes materialized
differently.

A difference in identity is a resolution failure and exits 10, as
`alias.unstable`. The lock describes a different request than the one being made:
another manifest, another release, another selection, another set of artifacts.
Refetching cannot fix that and the user has to decide, which is what the message
says.

A dataset the lock does not pin at all is a policy failure and exits 40, as
`policy.trust_refused`. This is not an invented rule. contracts.md says a locked
run never accepts `tofu`, and a run with no prior digest is by definition `tofu`.
The refusal is that sentence applied.

No new error kind was added for any of this, which was a constraint rather than a
convenience: `alias.unstable` already meant a name resolving to something other
than what it resolved to before, and that is exactly what a moved manifest digest
is.

When each comparison happens matters as much as what it compares. Everything
knowable before a byte moves is compared before a byte moves: the manifest digest,
the release, and the selection are all properties of the request. So a locked run
whose request differs from the lock publishes nothing and writes nothing. The byte
comparisons happen where they can: the digest is handed to the transfer, so the
store refuses the commit itself, and the destination never exists.

A locked run writes no lock. A run that may not differ from the lock has nothing
to add to it, and writing one would mean rewriting a file the user is holding the
run to.

Sources: contracts.md Locked runs; `crates/cli/tests/lock.rs`.

## Rung two, closed by the lock rather than by a source

Question: standing debt from phase two and three. A remote refetch re-downloads,
because nothing tells this build the server holds what the cache holds. Rung two
needs a source publishing immutable identity, which is phase seven.

Answer: it is closed for a locked run, and the counter says so.

A lock states a digest. A cache holding that digest needs no request, and the
transfer already knew how to short-circuit on an expected digest that is present.
What was missing was anyone handing it one. A locked run does, and a second
locked run against a warm cache issues zero requests, asserted on zero rather than
on a comparison.

It is closed only for a locked run, and that is deliberate. contracts.md defines
`--locked` as failing when resolution differs from the lock and says nothing about
a lock steering a run that was not locked. Reading a digest out of a lock and
skipping the network on the strength of it, in a run the user did not lock, would
be choosing a behavior the contract is silent about. The first version of this did
exactly that and the test suite caught it within the minute: a stale lock file left
in the workspace by an earlier test made an unrelated transfer fail on integrity,
because a run nobody had locked was being held to a digest nobody had asked for.

Phase seven still owns the other half. A source that publishes an immutable
identity lets a run with no lock skip the request too, and no source here does.

Sources: contracts.md Locked runs and Resume ladder;
`crates/cli/tests/lock.rs`; the phase three gate's inherited debt.

## A plan takes its digests from the lock, and is written to standard output

Question: a plan contains resolved digests so it can execute offline, and `plan`
moves no bytes. Those two together leave nowhere for a digest to come from.

Answer: from the lock. A reference the lock pins nothing for cannot be planned.

The alternatives were both worse. Probing the source for a digest is moving bytes
and would make `plan` need the network to describe a run that may not. Leaving the
digest out makes a plan that cannot execute offline, which is the only reason a
plan exists. So a plan is downstream of a lock: `get` records what a reference
resolves to, and `plan` reads it. Planning something with no recorded digest fails
with `policy.trust_refused` and says to run `get` once, for the same reason a
locked run with no entry does.

How a plan proves it is still valid at apply time: it does not, and nothing about
a plan expires. Apply re-resolves the digests the plan records. An object the
cache holds is materialized from it. One it does not is fetched with the plan's
digest handed to the transfer, so a source now serving something else fails on
integrity and never falls back to another source. There is no clock in this and no
signature over the plan, because a plan that a machine will act on is only as good
as the digests in it, and those are checked against the bytes every time.

`unknown: [expanded]` means the source stated no expanded size. A run that needs
that number is the disk precheck, and it does not get one: the plan reports `?`
and apply proceeds and fails with `resource.disk` if the volume runs out. A field
is never estimated into a number, so the alternative was inventing one.

A plan applied against a cache that already holds some of its objects uses them.
The `cached` field a plan records is what the machine that made the plan saw and
is reported rather than acted on, along with `disk`, `conflicts`, and
`destination`, which are all that machine's answers. `--output` names the
destination when the plan's own is not this machine's.

Where a plan goes. contracts.md gives `plan` no output flag and says stdout
carries the final result. The plan is the final result of `plan`, so it is written
there: the canonical text form by default and JSON under `--json`, both reading
back as the same plan. Nothing was invented to make this work, which was the
point.

Sources: contracts.md Plan and Output streams; `crates/cli/tests/portable.rs`.

## A bundle is a tar, because a tar has no index to trust

Question: a bundle travels on removable media between machines that do not trust
each other. What container, and how is it verified without trusting what it says
about itself.

Answer: an uncompressed tar whose member names are content digests.

The rule this follows is the one that made the backslash-zip call right in phase
three: do not trust structure the input asserts. A zip has a central directory
that says where every member is and how long it is, and an importer that walked it
would be believing the attacker's index. A tar has no index at all. Every member
is found by walking the stream from the beginning, and there is nothing in the
format that could be believed instead of the bytes.

A member name is a claim, never an instruction. Import hashes the bytes it reads
and compares the result to the name; the name is never used as a path, never
joined onto anything, and never reaches a filesystem call. A member named
`../../etc/passwd` is refused as `archive.unsafe_path` before anything else looks
at it, and a member named anything else that is not a digest is refused as
`cache.corrupt`.

Nothing is published until all of it checks out. Every member is staged into the
cache's partial area, hashed as it is read, and held against its name; only when
the whole bundle has been read does anything enter `objects/`. A truncated bundle,
a flipped byte, and a member whose bytes hash to something other than its name
each fail with nothing published, asserted by counting the objects directory
afterwards.

Uncompressed, because the objects a cache holds are usually already compressed
archives and a second compression pass would cost the whole bundle for nothing.

`cache export` writes every object the cache holds. A plan-scoped export would be
smaller and is not invented here, because no flag in contracts.md names one and a
cache is already the set of objects a machine chose to keep.

Sources: contracts.md Bundles; `crates/cli/tests/portable.rs`.

## The interop digest now rides in the transfer, and the seam widened for it

Question: a lock records `interop` for every artifact it pins. Nothing computed
one for a transfer.

Answer: both digests are taken in the one pass the bytes make, and the Store seam
returns both.

contracts.md has said from phase zero that both digests are computed on every
transfer. It was true for a local walk, which goes through `hash_stream`, and
false for everything that went through the store, which kept a single BLAKE3
hasher. Nobody noticed because nothing read the interop digest until a lock did.

The fix widens a seam, which is not free: `Store::commit` returned a content
digest and now returns both. The justification is the one the roadmap asks for.
The alternative was rereading every object to answer a question the bytes had
already gone past, which is a full read on every run and exactly the kind of
second read standards.md forbids.

The cache also had to learn to answer the question for an object it already holds,
because a warm run never reads those bytes at all. The interop digest of a
published object is recorded beside it, under `meta/interop/<hex>`, at the moment
it is published. It is a separate record rather than a field of the fingerprint
record, because a fingerprint is a cache of the phrase probably unchanged and
never appears in a lock, and mixing the two would have put a fingerprint one
`serde` derive away from a portable artifact.

Both hashers are updated through `hashing::update_digests`, which runs them on
separate threads of the processor pool, so neither serializes the other. That
required handing the cache a processor pool, which it did not have. It now takes
one the way it takes the work counter.

Sources: contracts.md Identity; standards.md CPU; `crates/cache/src/store.rs`.

## What a lock and a receipt record when part of a run failed

Question: artifacts are independent, verified objects stay cached, and the
destination is all or nothing. What is written down for a run where one artifact
of five failed.

Answer: the lock records the four that verified, with no tree. No receipt is
written at all.

contracts.md already contains the sentence that decides half of this: a lock
without a tree still pins the bytes. That sentence exists for exactly this run.
The four objects that verified are in the cache and are reused next time, and the
lock says what they are, so the next run has less to do and a `--locked` run
against the missing fifth is refused on policy rather than resolving it as a first
use. A partial lock entry cannot be mistaken for a complete one, because the
artifact that failed is simply absent and the missing-artifact comparison is what
a locked run does first.

The receipt is the other half. A receipt records what a run materialized, and this
run materialized nothing: the destination is all or nothing and was left alone. A
receipt naming a tree that was never published would make `verify` compare a
destination against a run that did not happen. So none is written, and `verify`
falls back to the pathless behavior, which is the truth.

Sources: contracts.md Partial success and Lock.

## The binary grew ten percent, and nineteen kilobytes of that is the TOML parser

The deterministic gate failed on the one metric that moved: binary size went
4,704,768 bytes to 5,199,360, which is 10.5 percent and twice the five percent a
deterministic metric is allowed. Every other deterministic metric is unchanged to
the byte: cache growth, bytes read, bytes written and requests are identical in
all six regimes, so nothing this phase added moves any bytes that were not being
moved before.

The number was attributed rather than guessed at. Building the release binary with
the TOML arm of the document reader removed and the dependency dropped from the
engine gives 5,180,416 bytes. The TOML parser therefore costs 18,944 bytes, and it
costs that little because the binary already linked it for configuration, which is
written in TOML and always was. The remaining 475,648 bytes are this phase's own
code: the canonical model and its two writers, the YAML subset reader, the JSON
writer promoted from a development dependency into the engine, locks and locked
runs, receipts, plans, bundles, and the `plan` and `apply` commands.

The baseline is re-recorded rather than the change reverted, which is what phase
two did when the binary doubled learning to speak TLS. The rule the baseline
exists for is that a regression is visible and explained, and this one is both.
What it would take to shrink it is not a smaller feature set but a smaller way of
writing the same one, and no measurement here says which part of the 475 kilobytes
is worth attacking.

Sources: `cargo run -p xtask -- bench` before and after; the release binary built
twice, once with the TOML arm removed.

## Phase 4 gate

What phase 4 was for: reproducibility stops being a property of one run and
becomes a file someone else can act on. What it delivers is the canonical form
every portable artifact is written in, locks and locked runs, receipts and a
`verify` that can use one, `plan` and `apply`, and bundles.

What passed, and on what.

Everything below ran on `x86_64-pc-windows-msvc`.

The verify gap phase three opened is closed and gated. Fetching an archive holding
a `0755` file and verifying the destination against its receipt reproduces the
digest `get` reported, asserted on equality of the two rather than on a constant.
The degrade still fires with no receipt, for a destination that moved, and for a
receipt naming another destination, each asserted by name. A destination changed
after its receipt was written fails `verify` with exit 30.

One model, three surface syntaxes, one digest. The same manifest written in YAML,
TOML and JSON parses to one model and digests identically. Reformatting it,
reordering its keys, and putting a carriage return before every line feed change
nothing. The canonical form holds no line ending at all, which is asserted on the
bytes. Anchors, aliases, merge keys, tags, directives, block scalars, document
separators, tabs as indentation, and a repeated key are each refused by name, and
`yes` is a word rather than a boolean.

A lock pins the bytes and the tree, holds nothing that belongs to one machine, and
is byte-identical across two runs of one source. One archive written out byte for
byte produces a lock asserted character for character, which is the cross-platform
statement a single machine can make: the container lane runs the same assertion
against the same bytes.

A locked run with no entry exits 40. A locked run of an unchanged source issues
zero requests, which is rung two closed by the lock. A locked run against a source
that has started serving different bytes exits 30 and publishes no destination.

The gate itself, as one test: fetch from a server, plan against the lock, export a
bundle, drop the server, import the bundle into a cold cache, apply the plan
offline, and get the tree digest the connected run reported, with zero requests
issued.

Bundles are hostile input. Truncated, one byte flipped inside a member, a member
whose bytes do not hash to its name, and a member named `../../etc/passwd` each
fail by the named kind with the objects directory still empty afterwards.

The costs, and they are the honest gap.

The offline lane in the container is written and wired into `cargo xtask verify`
as two runs, the second with `--network none` and a check that name resolution
itself fails, but the container lane has not been executed in this session. The
in-process gate proves the flow and the request counter proves no request was
issued; what is unproven here is that the flow survives with no network device at
all. That is the first thing to run on return.

Nothing ran on macOS or on a real Linux machine, exactly as phase two and three
left it. Apple stays compile-only.

The binary grew 10.5 percent and the deterministic gate failed on it. Every other
deterministic metric is unchanged to the byte. The growth is attributed in the
record above and the baseline is re-recorded rather than the change reverted.

`plan` and `apply` take one reference and one artifact, as `get` does. Partial
success across several artifacts is decided and written down and is not reachable
yet, because nothing in this build resolves more than one artifact.

`cache export` writes the whole cache. A plan-scoped bundle is smaller and is not
invented here.

Manifests parse and digest but nothing yet resolves a reference to one: every run
still resolves to the synthesized manifest a direct reference describes. The
parser, the canonical form, and the digest are what phase eight builds `init` on.

A reference resolves to a manifest, and partial success is reachable. A local
path whose extension is one a manifest is written in is read as a manifest, and
nothing else decides it, and a directory is never searched for one. The remote
manifest the grammar names is not resolved by this build, as the object store,
provider and metadata forms beside it are not. `crates/cli/tests/manifest.rs` runs a two-artifact dataset
through materialization, pinning, a second unchanged run, an artifact that cannot
be resolved, a declared digest the bytes do not have, a manifest that does not
parse, and two artifacts landing on one path. Partial success is no longer
written down and unreachable.

The archive is listed once and read once, and that is now asserted on the bytes
rather than argued about. `crates/archive/tests/passes.rs` counts the compressed
bytes pulled through the source and holds an extraction to two passes over it.
Breaking the held-stream reuse makes it read 513 times the archive's own size for
a 512-member archive instead of twice, so the test fails for the reason it
exists. Two passes is the floor for a compressed tar: the member list is only
knowable by inflating the whole stream, and selection is decided against the
whole list before any member is read. Removing the second pass costs either the
decompressed size in memory or a spill file, and contracts name neither.

Finding a member no longer costs the entry count. `open` walked the member list
comparing whole members, so an extraction cost the square of the entry count in
string comparisons. The listing now carries a map from member path to position,
which is exact rather than approximate because a member path is claimed by
exactly one entry, and the equality check is kept on the one candidate the map
returns.

Measured, on this machine, a cold run of 2000 files of 4 KiB each: 42.20 seconds
with the cache and 5.13 seconds with `--no-cache`, for the same tree digest. The
cache write path is 88 percent of that run. Per object it performs the lock file,
the lock owner record, the partial, the partial owner record, the durability
flush, the rename into `objects/`, and the object record; timed one at a time on
this volume those sum to 4.8 milliseconds, against a measured 18.5. The remainder
is what an on-access scanner charges for creating six files where one would do.
Every one of those operations is named by the cache contract: one writer per
digest held by an advisory lock, the lease and the partial and its source record
and its owner record all carrying one key, and publication as write then flush
then rename. None of them can be removed without changing that contract, so none
were. The `exists` check before the exclusive create is one syscall either way
and was left alone.

An unlocked warm run of a remote reference refetches everything. Measured against
a local server: two requests and the whole object, on every run, where the same
run with `--locked` makes no request and writes nothing. Contracts state the
resume ladder for an interrupted transfer and state nothing about revalidating a
completed object an unpinned URL still names, so nothing was invented. Whether an
unpinned remote should revalidate with a conditional request is a contract
decision and is written here rather than answered.

Reconcile still hashes the destination rather than fingerprinting it. Contracts
allow it: `unchanged` is fingerprint or hash. What is missing is a place to keep a
destination fingerprint. A receipt cannot hold one, because a receipt is never an
authority for identity, and `meta/` holds resolution metadata and per-host
measurements and witnesses, which a destination fingerprint is not. The store it
needs is a cache format change. Measured, a warm run over 256 MiB takes 0.48
seconds, of which the destination rehash is about half.

The binary is 5,302,272 bytes. Against the baseline phase 3 committed, 4,704,768,
that is 12.7 percent, and phase 4 is what grew it: the document model, the YAML
subset, the bundle reader, planning, and locked runs. Against the baseline phase 4
re-recorded, 5,199,360, resolving a reference to a manifest and indexing archive
members added 102,912 bytes, which is 1.98 percent and under the five percent
gate. The growth is attributed rather than reverted, as it was above.

What phase 5 inherits. The four-files-per-object cache layout and the packing
question, now with numbers: it is the dominant cost of a many-small-files run and
the numbers are above. Two Apple crates no machine compiles. A destination
fingerprint store. Revalidation of an unpinned remote. Rung two for a run that is
not locked, which needs a source publishing immutable identity and is phase
seven.

Sources: `cargo xtask verify` and `cargo test --workspace --exclude xtask` on this
machine; `cargo run -p xtask -- bench`; the records above;
`crates/engine/tests/document.rs`, `crates/cli/tests/lock.rs`,
`crates/cli/tests/mode.rs`, `crates/cli/tests/portable.rs`,
`crates/cli/tests/manifest.rs`, `crates/archive/tests/passes.rs`.

## The lock deduplicates a transfer, so a local write does not take one

Question: A cold run of 2000 small files spends 88 percent of its time in the
cache write path, which performs the lock file, the lock owner record, the
partial, the partial owner record, the flush, the rename and the object record
for every object. Phase 4 measured it and left the question open. Which of those
files exists because of a contract, and which because of an assumption.

Options: pack many objects into one file; keep a size threshold below which the
coordination is skipped; take the lock only where it does something.

Chosen: take the lock where there is a transfer to deduplicate, and write
directly where there is not. A `file:` source, an archive being extracted and a
member of an imported bundle are all bytes that are already on this machine.
They are written to a name of this process's own and renamed into `objects/`,
with no lock, no partial and no owner record. A remote transfer is unchanged.

Because: the contract states what the lock is for in one sentence. A second
process wanting an object being written waits and reuses the result, and never
starts a second transfer of the same digest. Where the bytes never cross a
network there is no second transfer to prevent. Two processes writing identical
bytes under one digest is not a race that can produce a wrong answer, because a
content-addressed write is idempotent and the rename already makes a torn object
impossible; it is only duplicated work, and the work duplicated is a local copy
that costs less than the three files the coordination costs.

Rejected, packing: it makes the cache a container format with its own free-space
accounting, its own compaction and its own corruption modes, and it puts a
second way of storing an object beside the one that exists. The contract says an
entry in `objects/` has been fully verified and there is no other way for a file
to appear there; packing means there is another way.

Rejected, a size threshold: a threshold answers the wrong question. Whether
coordination is worth its cost does not depend on how many bytes an object holds
but on whether those bytes have to be fetched. A one-byte remote object still
deduplicates a request, a round trip and a possibly metered download; a hundred
megabyte local file deduplicates a copy. A threshold would get both backwards,
and it would drift, because a number nobody can derive is a number somebody
tunes.

Proven by: the file operation counter, not a clock. The cold 2000-file run's
file operations per object fall, and every digest and every tree digest is
identical to the byte before and after. The concurrency suite and the thousand
kill loop stay green; had either failed, the reasoning above would have been
wrong somewhere and the answer would have been to say so rather than to weaken
the test.

Cost: the mark a prune wrote is cleared by publication in one place rather than
two, because the lockless path publishes without holding what a prune must wait
for. That was a hole in the existing local path as well: it published without
clearing the mark at all.

## Counting files, because bytes never saw the cost

Question: Phase 4's numbers say six files per object is the dominant cost of a
many-small-files run, and phase 4 could only say it in prose. Bytes read, bytes
written and requests all stayed flat across the change that mattered. What
number moves when the file count does.

Options: a timing gate; a syscall count; a count of file creations, renames and
flushes.

Chosen: `file_operations`, counted where the platform performs the operation.
It is a key added to `Work` and to the JSON result, which Compatibility permits
because it is additive. It gates deterministically at five percent like the other
three.

Because: standards say a deterministic metric is identical on identical inputs
and gates everywhere, and a timing metric is a property of the machine. The cost
being measured is a per-file cost that a scanner or a slow volume multiplies, so
timing it measures the volume and counting it measures the code. A raw syscall
count would be neither portable nor stable: the same three operations cost a
different number of calls on Windows and on Linux, and the number would move
when a standard library did.

Counted at the platform seam rather than at each call site, so a new caller
counts by construction, which is the same rule the byte and request counters
already follow.

## Revalidating an object a URL still names

Question: A warm run of an unpinned remote reference refetches the whole object
to learn it has not changed. Contracts state the resume ladder, which is about an
interrupted transfer, and state nothing about a completed one. What should a warm
run ask.

Options: refetch, which is what this build did; a metadata request followed by a
comparison; one conditional request.

Chosen: one conditional request built from the validator recorded when the object
was published. `If-None-Match` from the entity tag, `If-Modified-Since` from the
last modified value, both when both were recorded. A `304` reuses what the cache
holds. A `200` is the transfer, from zero, because the response already carries
the bytes. Anything else is handled by the retry classification that exists.

Because: the run is asking one question and this is the request that asks it. A
metadata request followed by a comparison asks the same question in two requests
and leaves a window between them in which the object changes. The conditional
request has no window: the source decides, atomically, whether the answer is the
bytes or nothing.

The trust class is unchanged by a `304`, because a `304` is the source restating
a validator it already gave. It is not a new observation of the content and does
not become a witness.

The validator lives in `meta/resolution/`, which the cache layout already names
as where resolution metadata lives. It is derived data: losing it costs one
transfer.

Proven by: the request counter and the byte counter. A warm remote run issues one
request and reads zero body bytes.

## A destination fingerprint is a receipt field

Question: Reconcile hashes every file in the destination. Phase 4 found the place
to keep a fingerprint missing and left it. A receipt is never an authority for
identity; `meta/` holds resolution metadata, per-host measurements and witnesses,
and a destination fingerprint is none of those. Where does one live.

Options: a new cache directory; a record in `meta/`; the receipt.

Chosen: the receipt, as `fingerprints`, one tuple per file entry path.

Because: contracts already say what a fingerprint is and what it is not. It is a
cache of the phrase probably unchanged, is never evidence of content, and never
appears in a lock. Recording one in a receipt therefore does not make the receipt
an identity authority, for the same reason recording the executable mode does not:
nothing is ever concluded from it except whether bytes have to be read. It is
exactly the model `--verify fingerprint` already applies to a cache hit, and a
mismatch falls through to hashing exactly as it does there.

One policy governs both sides. `--verify always` hashes every destination file
whatever its fingerprint says, `--verify fingerprint` reads the bytes only of the
files whose fingerprint moved, and `--verify never` lets the fingerprint decide
alone. A destination with no receipt has no recorded fingerprint and is hashed
whole, which is what this build did for every destination.

A receipt is local and may hold absolute paths, so a tuple naming a volume and a
file within it belongs there and nowhere portable. Nothing ever compares one
machine's fingerprint against another's.

## When a tree is stored, and where one comes from for an object that has none

Question: Limits give the outboard threshold as 64 MiB and the chunk group as
1 MiB. Every object this cache held before this phase has no tree. What happens
to one, and can a tree be built without fetching the object again.

Options: refetch to build a tree; build one from the cached bytes; store a tree
for every object regardless of size.

Chosen: a tree is stored for an object longer than the threshold and for no
other, it is built in the pass that writes the bytes, and an object already held
whose tree is absent has one built from its own bytes.

Because: the bytes in `objects/` have been fully verified, which is the
invariant that directory carries and the only reason anything is served from it.
A tree built from bytes that already hash to their name is authenticated by that
hash, so building one costs one read and refetching costs the whole object over
a network. There is no question of trusting the wrong bytes: a tree built from
bytes that did not hash to their name would fail its own root check the first
time it was walked.

Below the threshold nothing is stored, because the content digest already
authenticates an object that small whole and a tree for it would be a second
file per object for no localization worth having: the smallest repairable unit
is a chunk group, and an object at or below the threshold is at most 64 of them.

A tree is derived data in the sense Compatibility uses the word. The content
digest is the only authority. A stored tree that disagrees with the digest is
discarded and rebuilt rather than believed, which is what makes a tree an
attacker controls unable to make bad bytes verify: the walk's top comparison is
against the digest, not against anything the tree says about itself.

## Localizing damage, and the two bounds that stop a repair costing more than a refetch

Question: The walk descends the tree to find which chunk groups fail and the
refetch asks for exactly those spans. What happens when the damage is spread
across many groups.

Options: always refetch the named ranges; a byte fraction; a span count; both.

Chosen: both, and adjacent damaged groups are merged into one span first. A
repair fetches the object whole when the merged spans exceed the repair span
limit of 64, and when the damaged bytes exceed the whole-refetch share, which is half the object. Either bound
emits `degrade` naming what was asked for, what was used and the two numbers.

Because: a ranged repair trades bytes for requests. Each span costs a request,
a round trip and a range the source has to honor, and each saves the bytes it
does not carry. At one half of the object the bytes saved no longer pay for
anything, because the alternative is one request for all of them. At 64 spans
the request count is past the four connections a host is allowed, so the spans
serialize and the repair becomes slower than the refetch it replaced while
issuing sixteen times as many requests to somebody else's server.

Neither bound is a preference and neither is tunable for taste. Past either of
them the ranged repair costs more than what it replaces, which is a fact about
arithmetic rather than a choice about behavior.

The last word is never the tree. A repair rewrites the named ranges, then
rereads the whole object and hashes it, and publishes only when it hashes to the
digest it is named by. A repair whose localization was wrong therefore fails
loudly instead of publishing bytes nothing checked whole, and a tree that does
not check out against the digest is discarded, the object refetched whole and
the tree rebuilt.

## What travels beside a quarantined object

Question: Phase 1 built `quarantine/`. What is written beside an object in it,
and what is enough for a person to understand the damage without rerunning
anything.

Options: nothing, and rely on the run's own output; a log line; a record beside
the object.

Chosen: `quarantine/<hex>.diagnosis`, holding the digest the object is named by,
what its bytes actually hash to, its length, the byte ranges that failed against
the tree, why the damage could not be narrowed when it could not, the redacted
location and validator the cache recorded for it, when it was moved, and the
command that fetches those bytes again.

Because: the run that quarantined an object is over. Its output has scrolled
away or went to a file nobody kept, and `cache verify` quarantines objects in
bulk, so the fact that something failed is separated from which thing failed by
however many objects the cache held. The evidence has to live next to the
evidence.

An empty `damaged` is never read as no damage. It means the damage could not be
narrowed, and `localized` says which of the three reasons applies: no tree was
stored, the stored tree does not check out against the digest, or the object
could not be read at all. An object is in quarantine because it failed, and a
field that is empty for three different reasons has to say which.

Removed with the object it describes, by prune and by clear, because a diagnosis
of an object nothing holds describes nothing.

## Two witnesses from one machine are one witness

Question: `corroborated` means no prior digest, but the observed digest matches
at least two independent recorded witnesses. This is the definition most easily
weakened by accident. What is a witness, where does it live, and what makes two
of them independent.

Options: count observations; count origins; count runs; require all three to
differ.

Chosen: require all three. Two witnesses are independent only when they differ in
the machine that observed them, the origin that served the bytes, and the run
that recorded them. `corroborated` therefore requires at least two machines, at
least two origins and at least two runs.

Because: each of the three is a way the same evidence gets counted twice.

Two observations from one machine are one observation, because a machine that is
lying, compromised, or simply has a bad disk lies the same way the second time,
and because a machine that could count its own observations twice could raise its
own trust by running a command again.

Two from one origin are one observation, because the thing being corroborated is
what the bytes are, and one origin serving them twice is one claim restated. A
mirror that proxies another mirror is one origin wearing two names, which is why
the origin recorded is the host and path that served the bytes and not the alias
the user typed.

Two from one run are one observation, because one run has one view of the
network, one resolver answer and one path. An attacker on that path serves every
request in it, including the one to the second mirror.

Where they live: `meta/witness/<key>`, which the cache layout already names as
where witnesses live. The key is the derived-key hash of the manifest digest and
the artifact identifier, each length-prefixed. The key never involves the
content, because a witness identified by its subject's content would only ever
agree with itself.

How a witness avoids becoming a channel for talking a machine into trusting bad
bytes: nothing read from a source, a bundle, a lock, a receipt or a plan ever
becomes one. A witness is written only by a run on this machine that transferred
the bytes in full and verified them as they arrived. A cache hit writes none,
because it observed nothing. So no remote party can put a witness anywhere.

What remains, stated rather than hidden: a party that can write the cache
directory can write a witness file, and a shared cache directory between machines
is how a second machine's witness reaches this one. That is the only channel, and
it is exactly as trusted as the cache directory itself, which the same party
could change directly by writing into `objects/`. The witness rule does not claim
to defend against a writer of the cache; it claims to defend against a source, a
network path and a bundle, and it does.

Consequence, stated because it is the point rather than a limitation: a single
machine fetching from a single mirror never reaches `corroborated`, however many
times it runs. There is no independent evidence in that situation, and a class
that appeared anyway would be the definition silently weakened.

Rejected: counting an imported bundle as a witness. A bundle is bytes plus a
member name, and the name is checked against what the bytes hash to, so a bundle
can assert anything about itself and nothing about anybody else. It is not an
observation of a source.

Rejected: counting two entries of a manifest's source list reached in one run.
That is two origins and one run, which fails the rule for the reason above.

Rejected: recording a witness for an object the run found in the cache. The run
observed a file it already had, and has no evidence about what any source is
serving.

## What each verification policy does once a tree exists

Question: `--verify always`, `fingerprint` and `never` already exist. What does
each do now that an object may have an outboard tree.

Chosen: `always` walks the tree, verifying every group against it, when a tree is
stored, and rehashes the object whole when none is. `fingerprint` is unchanged.
`never` is unchanged.

Because, and this corrects the reason the change was proposed with: walking the
tree is not less work. It reads every byte of the object exactly as a rehash
does, and reads the tree on top, so it reads more. The byte counter says so, and
this record says so rather than repeating a claim the measurement did not
support.

What the tree actually buys is the shape of the failure. A rehash can say the
object is wrong. A walk says which byte ranges are wrong, which is the difference
between an object that has to be fetched again and one that can be repaired by
fetching a megabyte. That is worth the tree read, and it is the whole reason the
phase exists.

What `never` promises, stated plainly because the gate tests it: nothing about a
cache hit. It does not reach verification and fail to reject damaged bytes; it
does not verify at all, which is why the result is `unverified` and not any other
class. It never disables verification during transfer, which contracts say is
always on and not configurable, so bytes that entered the cache during the run
were checked as they arrived. A run under `never` that serves damaged cached
bytes has not had verification pass on them. It skipped verification and said so.

## Rung one, reachable at last, and the only place it is reachable from

Question: Rung one has been unreachable since phase 2 because it needs an
outboard tree that did not exist. Now trees exist. When is a tree known for a
digest whose bytes are not in `objects/`.

Chosen: after a quarantine. The object moves to `quarantine/` and its tree stays
in `outboard/`, because the tree is what a repair needs to find the damage. That
is the state rung one describes: the expected digest's tree is known, bytes for
it are on disk, and neither has been trusted yet.

So rung one is wired as the repair path uses it. The bytes on disk are verified
group by group against the tree, and the transfer resumes from the first group
that is bad or missing rather than from the recorded byte count. A group that
verifies is kept whatever the source now says identifies its bytes, because the
tree is stronger evidence than any validator: it says these exact bytes belong to
this exact digest, and a validator only says the source thinks nothing changed.

An object in `objects/` never reaches rung one, because a run holding the object
has nothing to transfer.

## Two commands named repair, and the line between them

Question: The command surface lists `repair <ref>` with a description, and lists
`repair` among the cache subcommands with none. `pin` and `unpin` are annotated
as taking a digest and `repair` is not, so the cache subcommand takes no argument
and therefore has no source to fetch from. What does it do.

Chosen, by the human the question was put to rather than by the session: `cache
repair` rebuilds the derived data the cache can regenerate from what it already
holds. An outboard tree that is missing or does not check out for an object whose
bytes still verify. An object record whose fields are lost. A lock whose holder is
not alive. A partial or staging entry with no object behind it. It takes no
argument, reaches no network, and resolves no reference. An object whose own bytes
fail verification is not repairable from the cache, so it is left in quarantine and
the report names `repair <ref>` as the command that can fetch those bytes again. It
reports what it rebuilt and what it could not.

Because: the two do not overlap once the line is drawn at the network. Every other
cache subcommand is local and argument-free, and a subcommand that took a reference
would be the reference-taking command reached through a second name, which is the
second way of doing something that standards delete. Derived data is precisely what
Compatibility says can be regenerated rather than migrated, and this is the command
that regenerates it, which is also what clears the objects this cache held before
outboard trees existed.

The question was asked rather than answered here, because contracts were silent and
a behavior invented for a named command is a placeholder wearing a description.

## Phase 5 gate

Question: Does phase 5 meet its exit criterion, and what remains unproven.

The criterion, from the roadmap: repair of a one-megabyte region inside a very
large object transfers approximately one megabyte and restores the correct
digest.

It is met, and the assertion is on counters rather than on a clock.
`crates/cli/tests/prove.rs` fetches a 68,161,537-byte object over the fault
server, overwrites one megabyte of the cached copy at group forty, and runs
`repair`. The result is one range, 1,048,576 bytes moved, two requests -- one
asking the source about the object and one carrying the megabyte -- and the
object equal byte for byte to what the source served.
`cache verify` passes afterwards. A stopwatch appears nowhere in it, because a
stopwatch cannot state that a megabyte moved.

What phase 5 was for: localized verification and repair is the capability
nothing else has. Every other tool can say an object is corrupt. This one says
which byte ranges, and fetches only those.

What passed, and on what.

`cargo xtask verify` on this machine, native `x86_64-pc-windows-msvc` and the
container lane for `x86_64-unknown-linux-musl` and `x86_64-unknown-linux-gnu`,
with `aarch64-apple-darwin` compiled and never run. The real output is in the
session that produced this record.

The suites this phase added: `crates/engine/tests/damage.rs`, twenty tests over
locating damage; `crates/engine/tests/witness.rs`, thirteen over the
independence rule; `crates/cache/tests/witness.rs`, six over the store that keeps
witnesses; `crates/platform/tests/counting.rs`, six over the file operation
counter; `crates/cli/tests/prove.rs`, fourteen over repair end to end.

Damage at every boundary the roadmap named is covered, each as a named test: the
first group, the last partial group, a region spanning two groups, one flipped
bit, a truncation, and damage to the outboard tree rather than to the object.
The last one matters most, and it is covered twice. At the engine, a tree built
over the damaged bytes -- the strongest tree an attacker can produce, internally
consistent at every node -- still fails, because the walk's top comparison is
against the content digest and nothing the tree says about itself is trusted.
At the command, the same forged tree makes `cache verify` quarantine the object
rather than pass it.

An object stored before trees existed is covered from both ends. `repair`
fetches it whole and emits `degrade` naming that no tree was stored, so the
damage could not be narrowed. `cache repair` builds the tree from the object's
own bytes, which are already verified, and a `repair` afterwards reports nothing
damaged.

Verification never passes on damaged content under any policy, and what `never`
promises is written into the test rather than implied by it. Under `always` and
under `fingerprint` a damaged cache hit stops the run with an integrity or cache
failure. Under `never` the run completes and records `unverified`, because
`never` does not reach verification and reject the bytes; it does not verify at
all. It never disables verification during transfer, so bytes that entered the
cache in the run were still checked as they arrived. A run under `never` that
serves damaged cached bytes has not had verification pass on them. It skipped
verification and said so.

Trust classes are produced as their definitions state, and the witness rule is
tested adversarially in the three ways it could be weakened by accident. Two
observations differing only in origin, only in machine, or only in run each stay
`tofu`. The same observation recorded twice under two instants is stored once
and stays `tofu`. Four observations made by one run, across two machines and two
mirrors, stay `tofu`, because one run has one view of the network. At the
command, fetching one origin twice from one machine never reaches
`corroborated`, which is the point of the definition rather than a limitation of
it.

What the gate found that nothing else would have.

The cache-hit verification policy was not applied on the materialization path at
all. A warm run that cloned an object's blocks into a destination never called
`open`, so it never reached the check, and `--verify always` reported success on
a damaged object while reading zero bytes. The policy now runs wherever an
object is reused, whether the bytes are read or cloned, because the policy is
about reusing the object rather than about how the bytes travel. This is what
the roadmap's "verification never passes on damaged content under any policy"
was for, and it was false until the test was written.

The file operation counter could not see the two files the phase 4 record blamed
for the small-files cost. The lock file is created inside the platform's own
locking path and a cache record is written with the standard library rather than
through the platform, so neither reached the counter. Both are counted now. This
is the same shape as "The write the counter could not see" from phase 3, and it
is the second time a counter has been introduced and immediately been found
blind to the thing it was introduced for.

A truncated object could not be localized, because the walk was given the length
of the file on disk rather than the length the tree records. Those two differ in
exactly one case, which is truncation, which is the case the walk was being
asked about.

The numbers.

The cold many-small-files regime, measured on this machine with the same corpus,
before and after the lock came out of the local write path:

    file operations   2026 -> 1273
    bytes read        1048576 -> 1048576
    bytes written     1305600 -> 1305600
    requests          0 -> 0

Cold cache, 64 objects: 530 -> 338 file operations, every byte count identical.
One large file: 29 -> 26. Warm cache, cold transfer and interrupted transfer are
unchanged, which is what the decision predicted: a remote transfer keeps its
lock because there is a transfer to deduplicate.

The drop is exactly three file operations per distinct object -- the lock file,
and the create and rename of its owner record -- times the 251 distinct objects
the 1024-file corpus holds, and times the 64 the cache corpus holds. Every
digest and every tree digest is identical before and after, which is what the
byte counters and the whole suite say.

A warm run of an unpinned remote reference issues one request and reads no body
bytes, where it previously issued two and refetched the object.

The repair of one megabyte writes 1,052,744 bytes: the megabyte it fetched, plus
the 4,168-byte tree that covers the whole object and is rewritten when the object
is published again.

The costs, and they are the honest gap.

A repair copies the object before patching it. It has to: `objects/` holds only
what has been verified, so the damaged object cannot be edited in place and the
patched copy cannot be published until it hashes whole. Where the volume clones
blocks the copy is cheap, and where it does not a repair of one megabyte inside
a hundred-megabyte object physically writes a hundred megabytes locally to save
ninety-nine over the network. That is the right trade against a network and the
wrong one against a local mirror, and nothing measures which it is.

`always` with a tree reads more, not less. It reads every byte of the object
exactly as a rehash does, and the tree on top. The reason to prefer it is the
shape of the failure, not its cost, and the record above says so rather than
repeating the claim the change was proposed with.

`corroborated` is unreachable on a single machine with a single mirror, by
construction. Nothing in this build produces it outside a shared cache
directory, so the class is proven by the classifier and by the store and not by
a run.

Witness origins are the redacted location of the first source in the artifact's
list, not the host and path the bytes actually arrived from after redirects. A
source that redirects two aliases to one host would be recorded as two origins.
Contracts say the origin is what served the bytes; this build records what was
asked for. Closing it needs the transfer to report the origin it settled on,
which is one field on `Transferred` and is not written here.

`repair` resolves its digest from the lock or from what the cache last resolved
the reference to. A reference this cache has never fetched cannot be repaired,
which is correct, but it also means a repair after `cache clear` is a `get`.

The benchmark baseline was re-recorded, because `file-operations` is a metric the
baseline did not carry and a metric that is added is not a number that moved.
The comparison said "regime cold-cache is not in both runs", which was wrong and
is now its own message naming the metric.

The binary is 5,515,776 bytes. Against the baseline phase 4 recorded, 5,302,272,
that is 4.03 percent, under the five percent gate. What grew it: the repair
command, the rebuild command, the diagnosis, resolution and witness records, and
the conditional request path. The baseline is re-recorded with the new metric in
it, so this growth is stated here rather than gated.

The container lane found a defect the native lane could not. A receipt records a
destination fingerprint, whose file identifier and two instants are a hundred and
twenty-eight bits wide, and the canonical form carries a number as a run of
digits that fits sixty-four. On Windows the values were small enough to render.
On Linux they were not, and every archive reconcile test failed with
`manifest.invalid` and the message that a number was out of range. Those fields
are opaque values a receipt only ever compares for equality, so they are written
as text: a value that is not a quantity is not written as one. The test that
would have caught it on any platform now exists and renders a receipt holding the
widest values any platform can produce.

One test flaked under the verification matrix and was found to be asserting more
than its contract. `a_stalled_connection_exits_twenty_rather_than_hanging` gave
the transfer a five-hundred-millisecond response timeout as well as a
five-hundred-millisecond idle timeout, so on a machine running the whole matrix
it sometimes lost the race for the response headers and recorded zero bytes.
Only the idle timeout has to be short for a stall to end quickly; the response
timeout was racing this machine's scheduler, which is not a contract. It is back
at its default and the test is unchanged otherwise.

What phase 5 leaves undone, gathered from every gate, as the audit's input.

From phase 0. Block cloning has never succeeded on any machine in this matrix,
because the Windows volume script needs an elevated shell and Hyper-V and the
Linux images do not include a cloning filesystem the container can mount as one.
What would clear it: a runner with ReFS, btrfs or XFS that the matrix can write
to.

From phase 0. The portable corpus is built by the same code on every target
rather than carried between them. Phase 4's bundle lane closed this for objects;
a materialized tree still has not physically moved between two machines. What
would clear it: two machines.

From phase 1. Locking that fails with `ENOLCK` or `EOPNOTSUPP` is a named skip.
No filesystem reachable in the container refuses a lock; bindfs, tmpfs and
procfs were each tried. What would clear it: a filesystem that refuses, or a
fault-injecting platform implementation, which the fault library could carry.

From phase 1. A network-backed volume is built nowhere, so the conservative
locking path and the network magic set are unproven. What would clear it: an
NFS or SMB export the container can mount.

From phase 2. macOS is compiled and never executed, and two crates are not even
compiled for it, because both link C cryptography this machine has no Apple
software development kit for. Both Windows on ARM targets are neither compiled
nor run. What would clear it: an Apple machine and a Windows on ARM machine, or
a pure-Rust TLS stack, which would also close the first half.

From phase 2. Rung two is proven only at the unit level, because it needs a
source publishing an immutable content address or version identity, which is
phase 7.

From phase 3. Many small files cost about twenty milliseconds each with an
on-access scanner enabled. Phase 5 removed three of the six files per object;
what remains is the object, its record, and the destination write. What would
clear the rest: packing, which contracts refuse, or an exclusion the user
configures, which `doctor` will state in phase 9.

From phase 4. `plan` and `apply` take one reference and one artifact. Partial
success across several artifacts is written down and reachable through a
manifest, and not through a plan.

From phase 4. `cache export` writes the whole cache. A plan-scoped bundle is
smaller and is not invented.

From phase 4. The remote manifest the reference grammar names is not resolved,
and neither are the object store, provider and metadata forms beside it. Those
are phases seven and eight.

From phase 5. The three items above under costs: the repair copy, the witness
origin, and `corroborated` unreachable without a second machine.

Sources: `cargo xtask verify` on this machine; `cargo test --workspace --exclude
xtask`; `cargo run -p xtask -- bench` before and after the write-path change;
`crates/cli/tests/prove.rs`, `crates/engine/tests/damage.rs`,
`crates/engine/tests/witness.rs`, `crates/cache/tests/witness.rs`,
`crates/platform/tests/counting.rs`; the decision records above.

## macOS is removed, and what it would cost to bring back

Question: The project has promised Windows, macOS and Linux since phase 0. No
Apple machine exists here and none is coming. Does the Apple code stay.

Options: Keep it compiled and never run, on the chance a machine appears; keep
it and stop compiling it; remove it end to end and correct every document that
promises it.

Chosen: Removed end to end.

Because: for five phases the Apple code was compiled, linted, and never
executed once. Since the client landed in phase 2 it has not even been fully
compiled: `fetchloom-sources` and `fetchloom-cli` link `ring`, whose
cryptography is C, and this machine has no Apple software development kit, so
the compile check covered four crates of six. Every Apple constant in the
deleted module was confirmed against the C library's own headers rather than
against a running kernel, which the phase 0 record already said. Code in that
state is not a supported platform; it is a claim, and standards.md calls a
claim nothing verifies a defect.

What was removed. `crates/platform/src/unix/macos.rs`, four hundred and two
lines carrying the six calls `rustix` does not wrap: the volume capability
query, preallocation, the two flush barriers, block cloning, machine and boot
identity, and process start time. The five Apple branches in the volume probe:
maximum path length, the clone and sparse answers read from capability bits,
maximum component length through `pathconf`, and network backing through
`statfs` `MNT_LOCAL`. The macOS cache and configuration paths in the binary.
Both Apple targets from `deny.toml`'s graph and the four Apple crates
`rustls-platform-verifier` pulled in behind them. `verify/volumes-macos.sh` and
the three sparse disk images it built. The Apple compile lane in `cargo xtask
verify` and the two degradations that only described it. Eight hundred and
eighty-two lines left the tree against one hundred and ninety-nine added.

`libc` left the workspace's own dependencies with it. Its stated reason in
`deny.toml` was the six Apple calls, and no Fetchloom crate reaches it now: the
probe's remaining `statfs` and `pathconf` answers both come from `rustix`. It
is still in the dependency graph on the Linux targets, arriving through
`filetime`, `getrandom` and `tempfile`, so its `deny.toml` entry stays and its
reason now says that rather than naming code that no longer exists.

`unix/` is renamed to `linux/`, and the module it dispatched to is merged into
it. The directory was named for a family and held one member; the split between
`unix/mod.rs` and `unix/linux.rs` existed only so a `host` alias could select
between two Unixes, and with one Unix left the alias was an indirection that
promised a portability the code no longer has. `#[cfg(unix)]` survives
elsewhere in the tree, where it means what it says: the platform that is not
Windows. `crates/platform/src/lib.rs` now selects on `target_os = "linux"`
rather than on `unix`, so a build for another Unix fails at the module
selection instead of compiling Linux syscalls for it.

What the project gives up. macOS users, entirely. There is no partial support
and no degraded mode: the binary does not build for an Apple target. The
conformance statement narrows from six cross-platform directions to two, and
the roadmap's phase 0 exit criterion narrows with it. One capability row loses
its only witness: `normalization` answered `normalizing` on HFS+ alone, and no
volume this matrix can build answers it now, so that row joins the
network-backed volume as a property the test harness knows about and no
filesystem here exercises. The Apple certificate verification path in
`rustls-platform-verifier` is still in the dependency tree and is now
unreachable on every target this project builds.

What it would cost to bring back. The platform module is the whole of it, and
it is one file: the six calls, the five probe branches, and `libc` back in the
workspace, which is what was deleted here and is recoverable from this commit.
The cache and configuration paths are four lines each. `deny.toml` needs both
targets and the four Apple crates. The volume script needs writing again.
None of that is the expensive part. The expensive part is the machine: without
one, macOS returns to exactly the state this record ends -- compiled, linted,
never executed, and constants confirmed against headers. A pure-Rust
replacement for `ring` would let the whole workspace compile for Apple again,
which closes the compile gap and closes none of the execution gap. Bringing
macOS back is worth doing when there is a machine to run it on, and is worth
nothing before that.

Earlier records in this file describe macOS as a live target. They were true
when they were written and are not amended. This record is what supersedes
them.

Sources: `cargo build --workspace`, `cargo clippy --workspace --all-targets`,
and `cargo clippy --target x86_64-unknown-linux-musl` for the platform, engine,
cache and archive crates, all on this machine; `cargo deny check`;
`cargo test --workspace`.

## Four shipped behaviors nobody had ever run

Every one of these was found by spawning the binary and walking the contracts
tables, which nothing in this tree had done. 548 tests passed while a bare tar
could not be fetched. The suite is the deliverable; the four fixes are what it
found.

A bare tar could not be fetched at all. `run.rs` read sixteen leading bytes and
handed them to `recognize`, which finds the tar magic at offset 257. Every other
magic this build sniffs lives in the first sixteen bytes, so tar alone was
affected and the failure blamed the archive: "the name said tar and the
archive's bytes said unrecognized bytes". The number of bytes recognition needs
is knowledge the archive crate owns, so it is now `SNIFF_LENGTH` there and the
caller reads it rather than restating it. The surface suite fetches all ten
shipped containers and compressions through the command, from bytes that really
hold them.

Flattening failed on every archive that declared its directories. contracts.md
said a member left with no path after dropping `n` components fails, and the
code implemented that exactly. A top-level directory entry has one component, so
at `flatten:1` -- which is the only thing flatten is for -- the run always failed
when the writer wrote directory headers and always succeeded when it did not.
Fetchloom synthesizes the ancestor directories itself, so the same logical tree
flattened or failed on an encoding detail the user cannot see, which is a
determinism defect as much as a usability one.

The contract is now that a directory left with no path names the destination
itself and is dropped, and a file left with no path is still fatal. Nothing is
lost by dropping it: the directories the surviving members need are synthesized
whether or not the container declared them, which is why `zip -D` already
produced the identical tree. A selection that flattening empties entirely fails,
because an empty destination is not an answer. Selection now takes a
`Candidate` carrying the member path and whether it is a directory, rather than
a path alone; both callers already knew which their members were.

A reference naming one file wrote no lock. The one-object path was entered only
when the name carried an archive extension, so a plain file fell through to the
directory walk, resolved to no object, and emitted a degrade saying "this
reference names a directory", which for a file is untrue. `plan`, `apply` and
`--locked` were unusable for that whole reference shape. The gate was in the
wrong place: whether a local file is an archive is decided from its own bytes,
later, by `recognized_format`. `object_to_resolve` now ingests any local file
and lets that decision happen where it already lived. One object out of the same
change: the archive reader was being given the cache object's path as the
archive's name, so a bare `.gz` materialized its one file under a digest in
hexadecimal instead of under the object's own name, which contracts.md's
reference grammar states.

`cache clear` could not clear a cache whose format did not match. The check that
refuses every command naming `cache clear` as the fix also refused `cache
clear`. The product printed a remedy that did not work and never mentioned the
one that did. `Cache::clear` was a method on an open cache, which is the reason
it could not run: clearing is the only operation that needs no format at all,
because removing a directory does not depend on what wrote it. It is now a free
function taking a root. The confirmation still reports what will be removed when
the cache can be opened, and says it could not count when it cannot.

One more, found by the same suite and fixed with them. A run after `--adopt`
reported a tree the destination did not hold. `--adopt` records the adopted
tree; the next run resolved a different tree, found every recorded fingerprint
still matching, and concluded `unchanged` against that different tree -- then
wrote it into the receipt as truth. `verify` disagreed with `get` on the same
directory. A recorded fingerprint is a cached answer to the question the run
that wrote it asked, so it now answers only when the receipt describes the tree
this run resolved, and every other case hashes. contracts.md already said a
receipt is "a cached copy of that answer, never a second authority for it"; this
is the code catching up with that sentence.

Sources: `cargo test --workspace` on this machine, 586 passed and 5 ignored,
against 548 before; `crates/cli/tests/surface.rs`.

## Making the numbers mean what they say

Phase 6 gates on the benchmark and on the event stream. Both were measuring
something other than what they named, so both gates were decorative.

The many-small-files corpus generated file content from `index % 251`, so the
1024 files it wrote held 251 distinct objects and the other 773 were cache hits
on a store the regime exists to stress. The regime named "many small files" was
measuring deduplication. The generator now writes the file's own index into the
leading bytes, so every file in a corpus is distinct. Every number that regime
reports got worse, and none of them got worse because the code did.

`bytes_written` read zero wherever the byte copy did not go through the write
path the counter was attached to: the archive extraction wrote its members
without counting them, and `NativePlatform::copy_bytes` -- the publish that runs
when the volume refuses to clone -- counted the file and not its bytes. Both now
count. This is why the written totals roughly double in every regime that
materializes a destination: one copy was always being made and one copy was
always being reported.

`file_operations` disagreed across paths because the CLI created directories
with `std::fs::create_dir_all`, which the Platform seam never sees. A run with a
cache reported 403 operations for the same corpus a run without one reported 66,
and the earlier 338-against-2 gap was not two paths doing different work, it was
one path being visible and the other not. Directory creation is now
`Platform::create_directories`, which recurses so that each directory it
actually creates counts once, and every call site goes through it. Both counts
are now correct, and they still differ: the cache path performs five operations
per object and publishes each file individually, and the no-cache path stages a
tree and publishes it with one directory rename. That difference is real work.

The baseline is re-recorded rather than repaired. Which numbers moved because
the measurement got honest, and which because the code changed:

| regime | metric | before | after | why |
| --- | --- | --- | --- | --- |
| no-op | binary-size | 5515776 | 5548032 | code |
| cold-cache | bytes-written | 33554432 | 50331648 | measurement |
| cold-cache | file-operations | 338 | 403 | measurement |
| warm-cache | file-operations | 2 | 67 | measurement |
| cold-transfer | bytes-written | 4194304 | 8388608 | measurement |
| cold-transfer | file-operations | 33 | 34 | measurement |
| interrupted-transfer | bytes-written | 4194304 | 8388608 | measurement |
| interrupted-transfer | file-operations | 53 | 54 | measurement |
| many-small-files | bytes-written | 1305600 | 3145728 | corpus and measurement |
| many-small-files | file-operations | 1273 | 6163 | corpus and measurement |
| one-large-file | bytes-written | 536887240 | 805322696 | measurement |
| one-large-file | file-operations | 26 | 28 | measurement |

Unchanged: warm-cache bytes-written, many-small-files bytes-read, every cache
growth figure, and every tree digest. The one deterministic move attributable to
code is the binary, which grew by the events and the reporter below. One code
change in this set was not isolated against the benchmark: `move_missing` now
publishes through the Platform seam rather than renaming directly, so it applies
the durability tier the rest of publication applies. It is a repair-path
publish, and no regime here exercises it; the claim is that it is correct, not
that it is measured.

Timing is reported and never gates. The many-small-files wall clock moved from
2936 ms to about 21777 ms on this volume, which is the corpus fix and a small
write costing between thirty and sixty times a large one depending on when the
scanner looks.

## A stream a reader can reconstruct the run from

Phase 9 builds the live view on the event stream and nothing else, so anything
the view must show has to be in the stream first. A cold archive fetch emitted
six events and named neither the transfer, the extraction, nor the cache
decision, and every `*.end` event but one carried `duration_ms: 0` because the
field was written as a literal zero at almost every site.

Duration is now a `Span`, taken at the start of the operation and asked for its
elapsed milliseconds at the end, and every end event carries a real one. Four
contracted events that no production code emitted now have emitters:
`extract.start` and `extract.end` around extraction, `cache.hit` and
`cache.miss` on the ingest path, and `cache.wait` when a writer actually waited
for a lease, which the Store seam now answers with `waited`. Six remain
unemitted and each belongs to an unshipped phase: `resolve.alias` needs the
alias resolution of phase 7, and `listing.start`, `listing.skipped`,
`listing.end`, `source.probe` and `source.selected` need the multi-source
listing of phase 8. They are contracted shapes waiting for the code that
produces them, not holes.

Failures reached the stream twice or not at all, because four call sites in
`main.rs` hand-rolled an `EventPayload::Failure` alongside the printer that
already emitted one, and the extraction rejection path emitted neither. There is
now one `Reporter`, which every command error goes through: it emits
`extract.reject` naming the member when the failure came from the extraction
layer, then the `error` event, then prints once in the shape the flags asked
for. The hand-rolled emissions are gone. `Error` carries the member it rejected,
as `Option<Box<str>>` rather than `Option<String>`, because the latter puts the
type at exactly the 128 bytes `clippy::result_large_err` refuses.

A result now carries the trust class the receipt recorded, which is the weakest
class of any artifact in it, and the policy is asked whether it accepts that
class before the run reports success. `--verify never` is the only setting that
accepts `unverified`.

The Policy seam is still called by nothing that decides anything: it answers
`cache_directory`, `accepts` and the verification and durability settings, and
those are read. What it does not do is arbitrate a decision between two
candidates, which is what phase 7 needs it for. It is a settings carrier today.
Phase 7 either widens it or admits it is one.

Sources: `cargo test --workspace` on this machine, 593 passed and 5 ignored;
`crates/cli/tests/stream.rs`, `crates/cli/tests/work.rs`;
`cargo xtask bench --save-baseline` under `FETCHLOOM_VERIFY`.

## Errors that named something else

`cache.corrupt` was the catch-all for every filesystem failure in two crates,
which made it the third kind to stand in for something it is not. The cause was
not a hundred bad judgements at a hundred call sites; it was that eight
different functions, six of them byte-identical, each took an `io::Error` and a
kind the caller had picked, and only ever changed the kind for a full volume.
Every other condition kept whatever the site had guessed.

There is now one decider, `error::filesystem_failure`, and a call site says
which of the two surfaces its path is on -- the cache or a destination -- which
is a fact about the path rather than a judgement about the failure. A full
volume or an exceeded quota is `resource.disk` on either surface, a crossed
volume boundary is `cache.cross_volume` or `destination.cross_volume` by
surface, and everything else falls to the surface it happened on. Its sibling
`error::lock_failure` decides the one operation whose kind comes from what was
attempted rather than from what the filesystem said, because a volume that
cannot express an advisory lock and a volume with no room left are different
failures and only one of them is the cache's fault. A test walks the sources of
both crates and fails on any function that turns an `io::Error` into an `Error`,
so a ninth decider cannot be added quietly.

Two failures were being told the wrong way round by that change. Opening the
lock file failed with `cache.locked`, which says another writer holds it, when
what happened was that the file could not be opened at all. Probing a cache
volume reports `destination.unrepresentable`, which is wrong for a cache and is
left wrong: the probe takes a directory and the Platform seam does not tell it
whose, and widening a seam costs more than the imprecision does.

A host name nothing can resolve was retried five times and reported as refused.
The transport classifier was matching lowercased substrings of whatever the
client printed, so it could not tell a name that does not exist from a socket
that closed. It now matches on `ureq::Error` itself. A failed handshake is found
by asking the io failure for the `rustls::Error` it carries rather than by
looking for the word in its message. A name lookup is the one answer the
standard library does not type on Unix, so it is read from the text
`getaddrinfo` failures carry, and a resolver that has not heard of a name is
kept apart from a resolver that could not be reached: only the first is
terminal. An existing test asserted the opposite, that a name that does not
resolve today may resolve later. It was right about the transient case and
wrong about the authoritative one, and it now says which.

Redirecting past the limit was reported as `network.status`. The redirect count
is in the Limits table, `resource.limit` is defined as a run exceeding a
configured limit, and a source that redirects forever has not answered with a
status at all.

Seven kinds in the Errors table were produced by no test, and two of those by no
production code. `policy.credential_invalid` is now produced where contracts.md
says it should be: a source answering 401 or 403 to a credential the run
presented is reported as the credential rather than as a raw status, which the
same sentence forbids surfacing on its own. A status no credential was carried
for stays a status. `alias.unstable`, `network.timeout`, `network.tls`,
`cache.locking_unsupported` and `resource.limit` now each have a test that
produces them, one of which needed the fault server to answer a secured
connection with a fatal handshake alert, seven bytes and no certificate.

`source.identity_changed` remains produced by nothing, and clearing it needs a
human. It is in the Errors table, and the Source seam's own documentation
promises `fetch` fails when a source "serves an object whose identity has
changed", but `fetch` is given the location, a range and a credential and never
the identity a previous response stated, so it cannot detect it. The only
situation that could name it is a resume whose recorded identity differs, and
contracts.md under Bundles settles that one by restarting from zero rather than by
failing, which the code does. Clearing this needs either a contract sentence
saying when the kind fires or the Source seam carrying the recorded identity
into `fetch`. Both are decisions, not implementations.

The ENOLCK skip is closed. Five gate records said it needed a filesystem that
answers ENOLCK, which no volume in the lane does, and the test it was blocking
was an empty function behind an `#[ignore]`. What it was actually asserting
splits in two, and neither half needs a filesystem: that a lock refused by the
volume is called `cache.locking_unsupported`, which is `lock_failure` and is now
tested directly against the errno family a FUSE filesystem without lock support
answers, and that opening a cache propagates a refused probe lock rather than
swallowing it, which is now tested through `FaultyPlatform` -- the first caller
that library's platform half has ever had.

Dependency added: `fetchloom-faults` as a dev-dependency of `fetchloom-cache`,
so the cache can be opened over a platform that refuses an operation. It is a
workspace crate that already depends only on the engine, so there is no cycle
and nothing new enters the build of the binary.

Sources: `cargo test --workspace` on this machine, 607 passed and 4 ignored,
against 593 before; `crates/engine/tests/filesystem_failure.rs`,
`crates/sources/tests/http.rs`, `crates/cache/tests/volumes.rs`,
`crates/cli/tests/lock.rs`.

## Contracted, and now built

Four limits in the Limits table were read by nothing. Two are now read. An
optional credential is offered only when the projected gain exceeds the offer
threshold, which contracts.md under When Fetchloom asks states and the code ignored: it prompted at
any gain and, worse, emitted `credential.offer` and `credential.declined` below
the threshold, where the same sentence says there is no prompt and no message.
A listing with more entries than the limit allows is refused with
`resource.limit` rather than read.

The other two are deferred with their reasons. `Probed candidates` bounds a
choice between sources, and there is one source and no probing across
candidates until phase 8, which is also where `source.probe` and
`source.selected` come from. `Resident memory` is a ceiling on what the process
holds, and nothing here can read what the process holds: it needs a resident set
query on both platforms behind the Platform seam, which is a seam widening and a
piece of work in its own right rather than a line that reads a field.

A witness named the origin that was asked rather than the origin that served.
`observation` took the first of the locations the manifest listed, so a run that
followed a redirect, or that failed over to a second source, recorded a witness
against a host that served nothing. Since a witness is the evidence behind
`corroborated`, and independence is decided partly by origin, that is evidence
about the wrong thing. `Source::fetch` now returns `Served`, pairing the bytes
with what the response said about them, so the location that answered travels
with them; `Transferred` carries it, and the witness records it. This widens the
Source seam by making one method return what its sibling already returned, which
is the justification: the seam could not report the origin that served because
it was never asked to.

`repair` could not reach an object a local reference put in the cache, which is
half the reference forms the command accepts. Two things were missing. The
digest could not be found, because a local reference records no resolution --
there is no validator to record one against -- so it is now taken from the file,
which repair reads anyway. And there was no source that could serve a range of a
local file, because `Repair` named `HttpSource` directly. It is now generic over
the seam, and `FileSource` serves any span of a path, states no validator
because a modification time is not evidence about bytes, and refuses to list
because a path is walked rather than asked for an index.

Deferred, with the reason. Disk accounting still happens where the write fails
rather than before the transfer. `plan` states the requirement per volume from
the size the lock pins, which is what contracts.md asks of it, and no sentence
requires `get` to check free space before starting. What is wrong is smaller and
sharper: the plan reports `staging` and `destination` as zero bytes for an
archive whose expanded size is unknown, while listing `expanded` under
`unknown`. contracts.md under Plan says a field is never estimated into a number, and
zero is a number. Fixing it changes the shape of a portable artifact, which is
additive-only, so it is a decision rather than an edit.

Sources: `cargo test --workspace` on this machine, 613 passed and 4 ignored,
against 607 before; `crates/cli/tests/cancel.rs`, `crates/cli/tests/policy.rs`,
`crates/cli/tests/prove.rs`, `crates/sources/tests/http.rs`.

## Windows on ARM compiles, and compiling is not running

`ring` was the reason this machine could cross-compile nothing: its
cryptography is C and assembly, and there is no C compiler here for another
target. `rustls-graviola` replaces it. Two corrections to what the audit
recorded: the crate is at 0.4.0, and 0.4.1 is `graviola`, the library under it;
and it is pure Rust with no build script that shells out.

The swap costs no cipher suite. Graviola offers the same three TLS 1.3 suites
and the same six TLS 1.2 ECDHE suites the ring provider offers, and its key
exchange list is a superset: X25519, P256 and P384 as before, plus the hybrid
X25519MLKEM768 that ring has no equivalent for. `cargo xtask network` performs
real handshakes against real hosts and every recorded subject still matched.
What it does cost is a maintenance question worth stating: graviola is a
younger library with a smaller deployment than ring, and it is now the only
thing standing between this binary and every remote source it reads.

`ring` was not the last C. blake3 compiles a NEON intrinsics file on aarch64,
so the target still needed a compiler. Its `no_neon` feature is now set for
`aarch64` on `windows` only, which drops blake3 to its portable Rust
implementation there. That is a real performance difference on Windows on ARM
and it is measured nowhere, because nothing here runs on that machine. Every
other target keeps the assembly.

`aarch64-pc-windows-msvc` is now a step in the host lane: `cargo check
--workspace --all-targets` against it. It compiles. Nothing runs it, and a
target that compiles has been shown to typecheck and link and nothing else --
no test has executed there, no volume has been probed there, and no benchmark
has been taken there. `arm64ec-pc-windows-msvc` does not compile: graviola
refuses the architecture outright, and the ABI exists for mixing ARM and x64
code inside one process rather than for a standalone binary, so it is recorded
as unreachable rather than pursued.

Sources: `cargo check --workspace --all-targets --target
aarch64-pc-windows-msvc` on this machine, clean; `cargo xtask network`, every
recorded subject matched; docs.rs for `rustls-graviola` 0.4.0 `suites` and `kx`.

## Packing, and the one lookup that makes it one way

decisions.md:3733 refused packing because a pack file is a second way for an
object to exist. That objection is answered by placement, not waived. Loose and
packed are one storage layer with size-determined placement, the way the 64 MiB
outboard threshold already sends small and large objects down different paths,
and it stays one way only under one discipline: exactly one lookup answers
"where is this digest", and every reader goes through it.

That discipline was the work. Twenty-two call sites across two crates turned a
digest into a path and opened it, each deciding for itself what an object is.
`Cache::placement` is the one lookup, and `read`, `size_of`, `holds`,
`fingerprint_of`, `place_object`, `publish_object`, `remove_object`,
`quarantine_object` and `owns_object` are what a caller asks instead. A reader
is a bounded, seekable view rather than a whole file. A test walks both crates
and fails on anything outside the lookup that turns a digest into a path, and it
found the one site that mattered: `hash_object` opened the container and read it
whole, which for a pack is every object it holds. That is the failure the
original objection predicted, caught by the rule rather than by a user.

The threshold is 1 MiB, which is `OUTBOARD_CHUNK_GROUP`. An object at or below
one chunk group is smaller than the unit the verifier works in, so its damage
can never be narrowed to a range and a repair of it is a whole refetch either
way. Nothing is given up by moving it, and the threshold is a constant the
project already reasons in rather than a new number.

A pack is self-describing: each object is preceded by its content digest, its
interop digest and its length. So the pack is the only authority on what it
holds, the lookup builds its answer from the packs themselves, and a packed
object needs no record file beside it -- which is where two of the file
operations went. A pack belongs to the process and boot that writes it and is
only ever appended to by that writer, so two writers never contend for one pack.
An entry is committed by its bytes reaching the pack; an entry whose length runs
past the end of the pack was cut short by a crash and is not one the cache
holds. Removing a packed object rewrites its pack without it, under a lock on
the pack, because a tombstone would be a second authority on what a pack holds.

The concurrency the store already survives is unchanged. The thousand-kill loop
and the eight racing writers pass without modification to what they do; what
changed is what they check, because the invariant "everything in `objects/`
hashes to its name" was only ever a statement about one placement. It now reads
every pack as well, and it holds.

Proven with file operations and bytes, never with a clock. The many-small-files
regime against the fixed corpus:

| metric | before packing | after | change |
| --- | --- | --- | --- |
| file-operations | 6163 | 3092 | -50 percent |
| bytes-written | 3145728 | 2170880 | -31 percent |
| bytes-read | 1048576 | 1048576 | none |

An object ingested locally now costs two file operations where it cost seven,
and no file of its own. cold-cache file-operations fell from 403 to 212 and its
bytes-written from 50331648 to 33559040, because the 256 KiB corpus files it
uses are below the threshold too.

One measurement had to be fixed to stay honest, which is the same defect as F5.
`cache-growth` measured `objects/` and called the answer the cache, so a cache
whose objects were all packed reported growing by nothing. It measures both
placements now, and cold-cache growth reads 16781824 against a 16777216-byte
corpus, which is the corpus plus one header per object.

The binary grew from 5548032 to 7110656 bytes. That is not this change. Measured
by building at each commit: 5595136 before the cryptography provider swap,
7023616 after it, 7110656 after the storage layer and the pack. Graviola costs
1428480 bytes and the whole storage layer costs 87040.

vision.md:43 said "The cache is plain directories" and ended "Deleting Fetchloom
leaves the data usable". The last sentence is the promise and the first was a
mechanism wrongly claimed for it. It now states the promise: nothing Fetchloom
writes needs Fetchloom to read, and every byte in the cache is either a file of
its own or a span of a pack that states, ahead of each object, the digest it is
under and how long it is. That is a correction, not a softening: the pack format
is forty bytes of header more than the old claim, and it is written down.

Sources: `cargo test --workspace` on this machine, 627 passed and 4 ignored,
against 613 before; `crates/cache/tests/storage.rs`;
`cargo test -p fetchloom-cache --test concurrency`, 5 passed and 2 ignored in
107 seconds; `cargo xtask bench --save-baseline` under `FETCHLOOM_VERIFY`.

## Three findings that get a disposition rather than a fix

**Every cold local fetch writes the bytes twice.** Confirmed, and deferred with
the reason the audit gave. `one-large-file` still writes 805322696 bytes for a
268435456-byte source, which is the destination, the cache copy and the tree.
The shape is right: the cache is handed the file the destination already holds,
and on a volume that shares blocks the second write is metadata. This volume
does not share blocks. What would clear it is a volume that clones, which is the
phase 0 debt still open, and no machine in this matrix can measure what removing
it would buy. Packing did not change this: the threshold is 1 MiB and the object
is 256 MiB, so it stays loose and stays copied. What packing did change is the
small end, where `cold-cache` bytes-written fell from 50331648 to 33559040
because a packed object is written once into the pack rather than once into a
scratch file and once into its own.

**Four seams have one implementation, and three have no defense.** Resolved in
standards.md, which is where the contradiction was. The Design rule said a trait
with one implementation and no test double is deleted; roadmap.md said the six
seams are defined in phase 0 and never change shape. Both cannot hold, and three
seams sat in the gap. The six named seams are now exempt, with the reason: their
point is that the phase adding the second implementation does not get to change
the shape, which requires the shape to exist before that phase does. A seam that
reaches 1.0 with one implementation and no reason to expect a second is a defect
to report then. The Platform seam is no longer among them in any case, because
`FaultyPlatform` now has a caller.

**Three seams hold whole lists in memory.** Deferred, and the audit's own
instruction is the reason: measure first, and the measurement has not been
taken. `Archive::members` is justified in the phase 4 record -- selection is
decided against the whole list before any member is read. `Store::list` and
`Source::list` are justified nowhere, and `Store::list` just grew: it now
concatenates the packed index, so a cache of a million small objects holds a
million digests and their placements rather than a million paths. That is a
larger number against the same unenforced ceiling, and `resident_memory` is
still read nowhere. What would clear it is one experiment nobody has run -- a
million-member tar, and a cache with a million objects, measured -- and a
resident set query behind the Platform seam to enforce the ceiling against.
Both are work, not lines.

## What packing does to per-user ownership

contracts.md under Quarantine diagnostics says objects are writable only by their creator and that prune
removes only objects the invoking user created. A pack belongs to the process
and boot that writes it, so ownership of a small object is ownership of its
pack. That is the same statement at a coarser grain and not a weaker one: two
users on one machine have different process identifiers, so they write different
packs, and every object in a pack was written by the user who owns it. There is
no arrangement in which one pack holds two users' objects.

The Linux volumes suite caught this, because its test published two objects as
one user and then gave one of them away, which a pack makes impossible to
express. The test now gives away the pack, which is what actually happens, and
keeps a large object loose so that both placements are covered by one run.

Sources: `cargo test -p fetchloom-cache --test volumes` under the Linux lane.

## Two defects in the verification harness itself

The cancellation tests waited for entries to appear in the destination. A
directory publish creates the destination in one rename at the end, so under the
load of a full verify the interrupt landed before there was anything to see, and
both tests failed there while passing when run alone. They wait for the run to
say `plan.ready` on its own event stream now, which is true whatever the run
publishes and when, and they finish in three seconds rather than twenty.

`verify/volumes-linux.sh` mounted six images with `mount -o loop`, which takes a
loop device and never gives it back. The machine has eight and Docker holds two
for its own images, so the first run of the day passed and every run after it
failed on a mount, with a different image named each time. The script releases
what a previous run left and asks loop-control for a device rather than taking
whatever is there, so the count the machine happened to start with no longer
decides whether the lane runs.

Neither is a defect in the product, and both were making the gate lie about it.

## A degradation the crypto swap closed

The verify matrix carried a degradation saying the Linux targets are linted in
the container only, because "the cryptography the client performs its handshake
with is C, which this machine cross-compiles none of". Removing `ring` removed
that reason. `cargo clippy --workspace --all-targets --target
x86_64-unknown-linux-musl` runs clean on this Windows host now, so the host
lints Linux as well as its own target and the degradation is gone rather than
reworded. Four remain, and each names a machine or a privilege this matrix does
not have rather than a decision anyone made.

## Phase 6. What a measurement is allowed to decide

Seven decisions were named for this phase. They are recorded here in the order
roadmap.md names them, and every one of them is bounded by the same rule: a
measurement may change how long a run takes and may never change what a run
produces. Where a decision needed a sentence contracts.md did not have, that
sentence was written into contracts.md in the same change.

### What is measured, how a decision is scored, and how it is stored per host

Three numbers per host: the concurrency the last run settled on, the throughput
it observed, and the time to first byte it observed. Nothing else, because
nothing else is read by anything. They are stored one host per file at
`meta/host/<hex>`, where the hex is the derived-key digest of the host name, so
the cache directory does not leak the list of hosts a user fetches from to
anything that can read a directory listing but not a file.

They are derived data under contracts.md under Compatibility and contracts.md under Revalidating a completed remote object, so they carry
no version field and are discarded rather than migrated when the format
fingerprint changes. A record that will not parse, is truncated, or was written
under a different fingerprint is discarded and the host is measured again. That
is safe precisely because the record can only move timing: a run that reads no
measurement and a run that reads a good one produce the same bytes.

Scoring is deliberately not a formula. The concurrency number is the previous
run's answer and is used as a starting point, not as a target, because the
controller below moves it from there on this run's evidence. Throughput and time
to first byte are inputs to the source order in contracts.md under Source selection and are not
combined into any other number.

### The increase and backoff policy for concurrency

Additive increase, multiplicative decrease, one step per completed transfer. A
clean transfer adds one. A rate limit halves, with a floor of one. Any other
failure subtracts one, with the same floor. A host nothing is recorded for
starts at two: one request to move bytes and one more to learn whether a second
helps, which is the smallest starting point that can produce evidence at all.

The ceiling is the thread budget clamped to eight, then bounded by the
politeness limit of four connections per host. A measurement never raises that
ceiling. Only `--aggressive` raises it, and it prints a warning when it does.
The asymmetry is the point: the cost of being one connection too slow is
seconds, and the cost of being several too fast is a host that stops answering.

### How protocol choice is evaluated

It is not evaluated separately. contracts.md under Source selection already fixes the order —
reachable, supports ranges, exposes immutable identity, recorded throughput for
that host, time to first byte, egress cost, remaining politeness headroom, ties
broken by manifest order. Phase 6 supplies two of those inputs from the
measurement cache and reorders nothing. A candidate list with no measurements
behind it scores every candidate equally and therefore comes out in manifest
order, which is what every source-order test in the suite already asserts.

Probing, and with it `source.probe` and `source.selected`, stays deferred to
phase 8 with the rest of multi-source selection. What phase 6 adds is that a
source abandoned for the next one records a `degrade` naming the one left, the
one taken, and why, rather than only emitting `source.failover`. A failover is a
fallback, and a fallback that is not in the degrade list is a fallback the
`--strict-degrade` gate cannot see.

### When ranged splitting of a single object is worthwhile

Never, in this phase, and this is a decision rather than an omission. Splitting
one object across several ranged requests to one host spends the politeness
budget on a single object, so it can only pay when that budget is otherwise
idle — a run fetching exactly one large artifact. The regime that would show the
gain is `one-large-file`, and on this machine that regime is bounded by hashing
and by the write path rather than by the network. Building the split now would
add a second way to transfer an object, which this project forbids, for a gain
no regime here can measure. It is deferred with a condition that would clear it:
a `one-large-file` measurement against a real remote host where the single-stream
throughput is below the measured link rate.

### How disk write rate applies backpressure to network concurrency

Through the same controller, as a fourth answer rather than a second mechanism.
The write path reports the rate at which the store accepted bytes. When that
rate falls below a fraction of what the same run had recently been sustaining,
the transfer answers the controller as though the host had faltered, which
subtracts one. There is no separate disk controller and no separate ceiling.
This keeps one number in charge of how many transfers are in flight, which is
the only way the politeness ceiling can remain a ceiling.

Recently is the operative word, and it is the second thing this was written as.
The rate a window is judged against is the fastest of the last eight windows,
not the fastest of the transfer. A peak taken over the whole transfer is wrong
here for the reason BBR gives for using a windowed max-filter rather than an
all-time maximum: an estimator that never forgets stays locked to a capacity
that is no longer available. The concrete case is not exotic. The first writes
into a new file are absorbed by the page cache at a rate no volume sustains, so
an all-time peak makes every honest window after it a collapse, and concurrency
ratchets to one and stays there for the rest of the run. Measured on the
implementation before the window was added, sixty-four of sixty-four steady
windows after one absorbed write answered the controller. With the window, at
most eight do, which is the time it takes for the absorbed peak to age out.

### Which I/O mode is chosen per platform and volume

`--io buffered` writes through the operating system's page cache. `--io
uncached` asks the operating system not to retain the written bytes once they
are durable, which is worth asking for because a cache object is verified as it
is written and is not read again by the run that wrote it.

On Linux that is `posix_fadvise` with `POSIX_FADV_DONTNEED` over the written
range after the flush. On Windows there is no per-file equivalent that does not
also require every write to be sector aligned in buffer, offset and length,
which would mean a second write path. So `--io uncached` on Windows uses
buffered writes and emits a `degrade` naming what was requested, what was used,
and that the platform offers no way to release written pages without
constraining every write.

`auto` chooses from the capability answers the Platform seam already produces,
never from the platform name, per contracts.md under Display modes. It chooses `uncached` only
on a volume whose backing is local, whose scanner answer is absent, and on a
platform that has the call. Network backing keeps the page cache, because on a
network volume the cache is the only thing hiding the latency. A scanner that is
present or unknown keeps the page cache, because a scanner reads the bytes back
immediately and dropping them buys a reread. `auto` never degrades, because
`auto` requested nothing in particular.

### How user limits and core counts bound every decision

Every decision above is bounded before it is made, by `Ceilings::resolve`, and
nothing downstream may exceed what it returns. The order is fixed: the detected
thread budget after affinity, container and job limits; clamped to the transfer
ceiling; then the politeness limit per host unless `--aggressive`; then anything
the user asked for, which may only lower it. A user asking for more than the
machine has is clamped and told so, per contracts.md under Display modes. A measurement enters
only after all of that, and only as a starting point inside the range those
bounds already allow.

`--deterministic-io` removes every one of these inputs at once. No measurement
is read, none is written, the controller is fixed at its starting value and does
not move, and the I/O mode is `buffered` regardless of what the volume can do.
It is the switch that makes the timing-independent counters reproducible, and it
exists so that "adaptation cannot change the bytes" is a claim with a test
behind it rather than an argument.

## Phase 6 gate

Question: Does phase 6 meet its exit criterion, and what remains unproven.

The criterion, from the roadmap: default settings beat hand-tuned fixed settings
across the regime matrix, with no correctness difference in any run.

The correctness half is met and is the stronger half. The performance half is
met only in the weak sense that defaults are never beaten, and the matrix that
was supposed to establish the strong sense cannot establish it. That is the
finding of this gate and it is stated before anything else, because the honest
outcome was to name the regime and the reason rather than to adjust the regime
until it passed.

What the correctness half rests on. `no_tuning_setting_changes_the_bytes_a_run_
produces` in `crates/cli/tests/tune.rs` runs one eight-file corpus under ten
configurations -- settled defaults, two concurrency and per-host pairs,
`--aggressive`, a bandwidth ceiling, both explicit write paths,
`--deterministic-io`, and two thread ceilings -- each in its own workspace so
the cache is cold for every one of them, and asserts that all ten agree on the
tree digest, on the content digest of every file that reached the disk, and on
all four deterministic counters. It compares what was materialized rather than
what the receipt said was materialized, because a receipt is the thing under
test. Given its own corpus per configuration it also fails for the right reason:
perturbing one configuration's input by a single file makes it name the
configuration and print both tree digests.

What the performance half rests on, and why it is weak. Three configurations --
settled defaults, a fixed one-and-one, and a fixed four threads -- were run
across all seven regimes, three rounds each, interleaved rather than blocked, so
that thermal drift and background load fall on every configuration equally
instead of on whichever ran last. Medians, in milliseconds, with the spread of
the defaults column beside them:

| Regime | Defaults | Fixed 1/1 | Threads 4 | Defaults spread |
|---|---|---|---|---|
| no-op | 14 | 14 | 13 | 4 |
| cold-cache | 1482 | 1727 | 1413 | 227 |
| warm-cache | 236 | 278 | 223 | 28 |
| cold-transfer | 131 | 138 | 151 | 9 |
| interrupted-transfer | 518 | 687 | 477 | 381 |
| many-small-files | 7958 | 7867 | 7869 | 872 |
| one-large-file | 1089 | 1269 | 1126 | 234 |

Defaults beat the fixed one-and-one on five regimes and tie on two. Against four
fixed threads the two are indistinguishable on six of seven, and the single
regime where the difference clears the noise floor is cold-transfer, which
defaults win. So defaults never lose. They also do not demonstrably win, and the
reason is not that the controller is bad.

The reason is that the matrix barely exercises it. Four of the seven regimes --
cold-cache, warm-cache, many-small-files and one-large-file -- make zero network
requests. Cold-transfer makes two and interrupted-transfer makes six. A per-host
concurrency controller with a politeness ceiling of four has nothing to decide
against two requests to one host, and nothing at all to decide against zero.
Five sixths of the wall time in this matrix is local filesystem work that no
tuning input touches, which is why every column looks alike. A benchmark suite
inherited from phases that were about the store is not a benchmark suite for a
phase about the network, and running it under six configurations does not make
it one.

So the correct statement is that the exit criterion as written is not yet
provable on this harness, and the missing piece is a regime that holds many
objects across more than one host with enough latency for concurrency to matter.
That is the work, and phase 6 does not close until it exists. Calling the
criterion met on a matrix where the controller is inert would be the same
mistake as the interrupted regime a benchmark agent once derived from another
regime's result rather than measuring.

What the gate found that nothing else would have.

The write rate filter never forgot. Backpressure judged each window against the
fastest rate the transfer had ever reached, and the first writes into a new file
are absorbed by the page cache at a rate no volume sustains. The consequence is
not subtle: on the implementation as first written, sixty-four of sixty-four
steady windows after one absorbed write answered the controller as a collapse,
which drives concurrency to one and holds it there for the rest of the run --
the exact opposite of what backpressure is for. This is the failure mode BBR
names when it argues for a windowed max-filter over an all-time maximum, and the
fix is the same one: judge against the fastest of the last eight windows, so an
absorbed peak ages out. `a_rate_the_page_cache_absorbed_once_does_not_condemn_
every_window_after_it` holds it, and fails at sixty-four when the window is
removed. Every existing backpressure test passed both before and after, which is
why this was found by reading rather than by the suite.

The published page had to be generated rather than written. The comparison
against `cp -r`, `curl` and the platform copy is produced by the harness from
the same run that produces the gated counters, so a number on the page cannot
drift from the number that was measured. Six of its seven rows are losses. That
is the roadmap's requirement and it is also the accurate picture: Fetchloom
hashes every byte twice, writes an outboard tree, publishes through staging and
records what it did, and none of the tools beside it do any of that.

What is carried forward, unclosed.

The `--no-cache` floor is worse than the audit claimed and phase 6 does not fix
it. Re-measured as a median of three against the same `cp -r` the original used,
it is 8278 ms against 1190, which is 7.0x rather than the 1.39x on record, and
it performs 3093 file operations against the warm cached path's 1027. The cost
is the double write that was already deferred, not a tuning decision, so no
setting reaches it. The retraction is in audit.md rather than an edit to the
original claim, because a record that can be edited backwards is not a record.

The many-small-files regime is bound by the same thing. Its 1 KiB files are far
below the pack threshold and are packed, so the amplification is on the
destination side: roughly three file operations per file against the one that
`cp -r` pays. This is the write amplification that per-file metadata cost makes
expensive on NTFS, and it is the largest single number on the published page.
It is a store problem, named here so that phase 6 is not credited with it.

F10's arbitration half goes to phase 7. The settings half is closed: the tuning
surface is routed through the Policy seam and production code now calls eleven of
the trait's fifteen methods. `offline`, `credential`, `offer_credential` and `terms` still
have no production caller, and arbitration between sources cannot be exercised
while there is one Source implementation to arbitrate between.

Six events remain unemitted and go to phases 7 and 8. F19's two remaining limits
are unenforced. F21's cloning volume and F26's million-object measurement both
need hardware this machine does not have and are unmeasured rather than
deferred by choice.

One piece of seam surface had no production caller, and the fix was to route to
it rather than to delete it. `Policy::offline` was the last settings-shaped
method on the seam that nothing called: the offline check read the resolved
settings directly while `verification`, `durability`, `cache_directory`,
`concurrency`, `per_host` and `io` all went through the seam. Framing that as
keep-versus-delete was the wrong reading. The duplication is not the method
against the settings; it is the enforcement point disagreeing with its own
siblings about where a setting is read. `allowed_offline` now takes a
`&dyn Policy` and asks it, which removes the second way in the direction that
keeps the shape consistent and satisfies the reason standards.md gives for the
seams' exemption: the phase that adds the second implementation must not get to
change the shape, which requires the shape to exist first. standards.md now says
that the exemption covers the method surface too, so this is not re-opened.

The behavior did not move. An offline run still refuses a network reference with
`policy.offline` and exits 40 before any avoidable work, per contracts.md under Offline.
`the_seam_is_what_refuses_a_network_reference_offline` asserts it against a
policy carrying no settings at all, so nothing but the seam can have answered.
Twelve of the trait's fifteen methods now have a production caller; `credential`,
`offer_credential` and `terms` are the three that do not, and they wait on the
credential work in a later phase.

## What the eighth regime answered

The finding above stands, and the regime built to settle it did settle it, in the
other direction from the one that was hoped for.

many-hosts exists because the first seven regimes could not exercise the
controller. It does exercise it: eight objects from each of two loopback hosts,
one host rate limited and the other not, and the run records `127.0.0.1` at a
concurrency of one and `::1` at four. A host that was pushed back sits at the
floor, a host that was not sits at the politeness ceiling, and the two were
tracked apart. Stub `Controller::answered` to ignore every answer and the regime
refuses the run naming that every host is recorded at two. That is the half of
the exit criterion about adaptation happening at all, and it is met.

The half about defaults winning is still not met, and the reason is now specific
rather than circumstantial. Concurrency exists to hide latency. Every source
this harness can build is a loopback socket or a local directory, and neither has
any latency to hide, so no number of transfers in flight can pay for itself. The
matrix says exactly that. Across eight regimes and four configurations -- settled
defaults, a fixed one and one, a fixed eight and four, and a fixed eight and two
that was chosen because it is what a careful person would actually pick -- run
four rounds in a rotation so that each configuration sits in each position once,
no configuration beats another by more than the run-to-run spread of a single
configuration on the same regime.

Medians in milliseconds over four rotated rounds, with the lowest and highest
single reading of the defaults column beside them, because that range is what any
difference has to clear to mean anything:

| Regime | Defaults | Fixed 1/1 | Fixed 8/4 | Fixed 8/2 | Defaults low | Defaults high |
|---|---|---|---|---|---|---|
| no-op | 15 | 18 | 16 | 14 | 14 | 33 |
| cold-cache | 1538 | 1561 | 1499 | 1484 | 1483 | 1610 |
| warm-cache | 228 | 204 | 182 | 196 | 183 | 258 |
| cold-transfer | 113 | 120 | 102 | 111 | 113 | 123 |
| interrupted-transfer | 466 | 352 | 438 | 303 | 432 | 926 |
| many-small-files | 5118 | 6568 | 6758 | 6501 | 4993 | 8843 |
| one-large-file | 937 | 934 | 878 | 930 | 934 | 4779 |
| many-hosts | 9204 | 8612 | 8617 | 8636 | 8618 | 10649 |

The correctness half is asserted separately and it holds everywhere.
`no_tuning_setting_changes_the_bytes_a_run_produces` runs ten configurations over
one corpus and `no_tuning_setting_changes_what_two_hosts_produce` runs five over a
manifest spanning both hosts, each configuration in its own workspace so every
cache is cold, and both assert one tree digest, one content digest per file that
reached the disk, and identical deterministic counters. Perturb a single
configuration's input by one byte and the second names the configuration and
prints both tree digests.

That is not a measurement failure to be re-run on a quieter machine. It is
structural. Seven of the eight regimes have no mechanism by which a per-host
concurrency ceiling could change their time: five make no network request at all,
and cold transfer and interrupted transfer each move a single object, for which a
per-host ceiling is meaningless whatever it is set to. The eighth has the
mechanism and no latency for it to act on. standards.md:137 already says a
constrained network is named nowhere in the harness because it cannot be produced
in the verification lane without a fault injector this build does not have. That
sentence, written before this phase, is the reason phase 6 cannot close, and the
missing piece is delay injection in the fault server rather than anything about
the controller.

So: the controller is built, it decides, its decisions are per host, it never
changes a byte, and no setting a user can pass changes a byte either. What is
absent is a source slow enough for any of it to show up on a clock.

## What the gate found that the tests did not

Two defects survived a green suite and were found by asking what a number was
made of, which is worth recording because neither was reachable from a test.

The many-hosts regime spent ninety-five percent of its wall time resolving a
name. Reached with half the objects spelled `localhost`, sixteen objects took
17,958 ms; reached entirely as `127.0.0.1`, the same sixteen objects with the
same thirty-two requests took 831 ms. Windows resolves `localhost` to `::1`
first, the fault server bound only `127.0.0.1`, and every transfer so spelled
stalled before falling back. The regime was measuring name resolution, and it
had already been committed. Both host keys are now loopback addresses that exist
on both platforms by default, and the regime costs 9,195 ms of which 8,400 ms is
the retry backoff it exists to provoke.

The first version of this matrix ran the configurations in a fixed order within
each round, and reported that defaults lost on many-small-files by 775 ms against
a spread of 307. Re-run in a rotation, defaults are the fastest configuration on
that regime by 1,383 ms. The first result was the cold page cache landing on
whichever configuration ran first, every round. It was reported as a named losing
regime in the draft of this record, and it was wrong. What caught it was not a
test but the question of how a regime that issues no network request could be
responding to a network setting.

Both have the same shape. A number was plausible, the suite was green, and the
mechanism made no sense. The mechanism is the check.

## One disagreement between the code and this file

decisions.md:2214 says `Retry-After` "replaces the computed backoff rather than
adding to it". `crates/engine/src/transfer.rs` sleeps
`asked.unwrap_or(backing_off).max(backing_off)`, which takes the longer of the
two, so a `Retry-After` shorter than the computed backoff is discarded and the
computed one runs instead. It replaces only when the source asks for longer.

contracts.md is silent on `Retry-After` entirely, so this is not a contract
violation, and it is left as it is rather than decided here. The code is very
likely the right behavior: honoring a wait shorter than our own jittered backoff
defeats the stampede protection that the same paragraph argues for two sentences
later. The record is the thing that is probably wrong. It is named here rather
than quietly changed because which of the two moves is a decision about how
Fetchloom treats a source that asks to be hammered, and that is not a decision to
make while amending a benchmark.

## Latency the harness can inject, and what it revealed instead

Question: standards.md said a constrained network is named nowhere in the
harness because it needs a fault injector this build does not have. Phase 6
could not close its performance half for that reason. Build the injector and
re-run the criterion.

Options: shape the network from outside the process, with a platform traffic
control facility; charge the wait inside the fault server.

Chosen: inside the fault server. `Latency` charges a configurable wait per
request and a further wait to requests naming one host, applied before any
response head is written, so a loopback socket answers with the time to first
byte of a remote one. A traffic control facility needs privileges the
verification lane does not have on either platform and shapes every socket on
the machine rather than the one under test.

The many-hosts regime now serves all sixteen of its objects behind 100 ms of
injected latency per request. That is 40 requests and 4000 ms of latency in a
regime whose whole median was 9204 ms before it.

Because: concurrency exists to hide latency. Without latency no per-host ceiling
can pay for itself, which is exactly what the phase 6 gate concluded and what
this was built to remove.

What it found: the answer is still no, and the reason is not the one phase 6
recorded. Medians of three, same machine, same regime:

| Configuration | many-hosts |
|---|---|
| Settled defaults | 12696 ms |
| Fixed one and one | 12751 ms |
| Fixed eight and eight | 12725 ms |

Three configurations whose per-host ceiling differs by a factor of eight agree
within 0.4 percent, and every one of them paid all 4000 ms of injected latency.
If any two requests had been in flight together, the ceiling of one would have
cost seconds more than the ceiling of eight. None did.

The cause is that no run issues two requests at once. `materialize_manifest`
resolves artifacts in a `for` loop, one at a time, and the only parallelism
anywhere under `crates/*/src` is the `rayon::join` in `hashing.rs` that runs the
two digests of one stream beside each other. The adaptive controller computes a
permitted count per host, records it, and moves it on what the host answers, and
nothing ever runs that many transfers. `--concurrency` and `--per-host` bound
nothing.

That is a contract violation on its own terms. contracts.md Flags gives
`--concurrency` the meaning "global in-flight transfers" and `--per-host`
"in-flight transfers per host", and contracts.md Command surface says a flag
exists in the binary only once it performs what is written there, because
anything else is a placeholder. features.md Transfer says independent artifacts
transfer concurrently inside global and per-host limits. Neither holds.

So phase 6's performance half stays open, and its open half is now a specific
missing behavior rather than a missing measurement: concurrent transfer of
independent artifacts, bounded by the two ceilings the controller already
computes. Delay injection was necessary to learn this and does not fix it. The
regime was not adjusted until it passed.

Costs: the many-hosts regime is 3.5 seconds slower than it was, on every run of
the harness, buying a regime that can tell a working controller from an inert
one once there is something for it to bound.

Uncertain: nothing about the measurement. Whether concurrent transfer belongs to
a re-opened phase 6 or to a later one is not decided here.

Sources: standards.md Measure; contracts.md Flags and Command surface;
features.md Transfer; `crates/cli/src/run.rs` `materialize_manifest`.

## The disagreement at 2214, settled

Question: This file said `Retry-After` "replaces the computed backoff rather
than adding to it". The code takes the longer of the two. contracts.md was
silent, so the last record left it open rather than deciding it while amending a
benchmark.

Options: obey the source exactly, whatever it asks; treat its wait as a floor.

Chosen: a floor. The wait before the next attempt is the longer of the run's own
jittered backoff and the wait the source asked for. The record at 2214 is
amended to say so and contracts.md now carries the rule, under Retry.

Because: contracts is unambiguous that politeness is never lowered by a
measurement, and a source asking to be retried sooner than our own policy would
have waited is asking for exactly that. Full jitter exists so that a rate-limit
storm does not become a synchronized second storm, and a `Retry-After` of zero
seconds from every host in a storm would defeat it precisely when it matters.
Obeying a longer wait costs nothing and is what the source is really asking for.

Costs: a source that genuinely recovers in one second is not asked again until
our backoff is spent, which on a late attempt is up to a minute.

Sources: contracts.md Retry; RFC 9110 section 10.2.3.

## Phase 7. Which source is built, and why a grammar row was deleted for it

Question: roadmap.md:129 asks which sources meet measured demand and in what
order. contracts.md's reference grammar carried `s3://bucket/prefix/`,
`hf:datasets/org/name@rev`, `zenodo:...` and `croissant:...`.

Options: build the `s3://` scheme against a default endpoint; build it against a
configured endpoint; build the object store protocol behind an HTTPS container
reference and delete the `s3://` row.

Chosen: one adapter, object storage, addressed as an HTTPS container reference
whose endpoint answers the object store list API, and the `s3://` row is
removed from the grammar rather than carried forward unresolved.

A reference is an identity. The whole promise of a lock, a plan and an apply is
that the reference one machine resolved names the same bytes on another.
`s3://bucket/prefix/` structurally cannot carry an endpoint: the authority slot
of the form is spent on the bucket name, and four vendors serve that protocol.
It can therefore only be completed from local configuration, which means the
same reference names different bytes on different machines. That is not a
feature this build has yet to write; it is a form that cannot honor the promise
the project is built on, and a configuration key would institutionalize the
problem rather than solve it.

Leaving it in the table unresolved would be the F1 defect deliberately: a
contracted reference form that does not work. The audit has already cost this
project once for exactly that shape. Removing surface that cannot honor the
contract is the direction standards.md:19 points -- when a format changes the
old one stops existing -- and before 1.0 there is nothing to preserve.

Nothing is lost by it. Every S3-compatible store has an HTTPS endpoint:
`https://s3.amazonaws.com/bucket/`, `https://minio.local:9000/bucket/`, and the
same for R2 and Ceph. The endpoint travels inside the reference, which is what
portability requires. Recognition is not sniffing either: contracts.md Directory
listing already contracts object store listing APIs as recognized and
`reference.unresolved` when they are not, so what makes a container an object
store prefix is specified rather than guessed.

Because: roadmap.md's purpose for this phase is that the contract is proven by
one real implementation before more are added, and what proves it is that an
adapter can be written against the seam and judged by the shared suite. Which
scheme spells the reference does not bear on that.

Costs: a user holding an `s3://` reference rewrites it as the endpoint that
serves it, which is a mechanical change and the only form that travels.
`ReferenceForm::ObjectStore` remains an enum variant nothing reads, as it was
before this phase.

Sources: contracts.md Reference grammar, Directory listing; standards.md One
thing; roadmap.md:129 and :131.

## Phase 7. How object storage maps onto the Source seam

Question: roadmap.md:129 asks how each source maps to the Source seam,
especially immutable identity and range support.

Chosen, field by field, with nothing inferred that the store did not say:

| Seam field | Where it comes from | When the store says nothing |
|---|---|---|
| `size` | `Content-Length` on the probe | absent, and the plan names it in `unknown` |
| `supports_ranges` | `Accept-Ranges` naming `bytes` | false, and a run wanting a range emits `degrade` and transfers whole |
| `identity` | the store's object version identifier, else the entity tag | `SourceIdentity::None`, so no rung above five is reachable |
| `content`, `interop` | never; a store states no digest of ours | always absent |
| `last_modified` | `Last-Modified` | absent |
| `cost` | the store's requester-pays answer | unknown, and the plan names it |
| listing | the list response, bounded by the listing entry and byte limits | `reference.unresolved`, never guessed |

Immutable identity is the version identifier when the store returns one, because
a versioned object identifier names bytes that cannot change under it, which is
what `SourceIdentity::ImmutableVersion` means. An entity tag is read by the HTTP
rule already in use: a `W/` prefix is weak, anything else is strong. A store
returning neither exposes no identity, and the run says so rather than resuming
against a name that proves nothing.

Range support is read from the store's own answer and never assumed from the
protocol. A store that does not advertise ranges degrades honestly: `degrade`
names that a range was requested, that the whole object was used, and that the
source does not serve ranges. Localized repair against such a source is
unavailable rather than silently whole.

Because: both are what the roadmap says contracts cares most about, and both are
places where a wrong assumption stays invisible until it corrupts a resume.
Reading them from the response and degrading when they are absent is the only
form that cannot be quietly wrong.

Sources: contracts.md Resume ladder, Source selection, Directory listing;
roadmap.md:129.

## Phase 7. Bearer only, and why SigV4 is the next adapter's job

Question: contracts.md's credential model is one token per host, placed in
`FETCHLOOM_TOKEN_<HOST>` and sent as `Authorization`. Real S3 uses SigV4 request
signing, which contracts never mentions.

Options: implement SigV4 now; pack a key and a secret into the one token string;
send the credential as a bearer token and fail honestly where that is not
enough.

Chosen: bearer only.

The decisive check is in the code rather than in an argument.
`Credential` in `crates/engine/src/credential.rs` is `Credential { host, origin, value:
Secret<String> }`: one opaque string. SigV4 needs an access key, a secret key and
a region, and the secret never crosses the wire -- it is not a value that is
sent, it is a value that is signed with. That is a different shape, and
`Credential` lives in the engine.

roadmap.md:135 is this phase's exit criterion: a new adapter can be added by
implementing the seam and passing the shared suite, with no change to the engine.
Implementing SigV4 now means widening an engine type to fit the first adapter,
which is the precise failure that criterion exists to detect. It would be proving
the seam by breaking it.

Packing `key:secret` into the one string to avoid that is refused. It is
stringly-typed and it is a second credential shape wearing the first one's type,
and standards.md forbids both. It would also hide the signal the criterion is
there to surface.

Bearer is not a toy path. Google Cloud Storage and Azure Blob Storage both accept
real bearer tokens for private containers, so the credential tiers, the offer
flow and the redaction rules are exercised against actual private access rather
than only against public buckets and presigned links.

SigV4 is the deliberate carry-forward, and it is named as what it is: the second
credential shape. The phase that adds it widens `Credential` on purpose, and that
widening is itself the real test of whether this seam holds. A bucket needing it
fails now with the provider help record naming exactly what it needs, per
contracts.md Provider help records.

Costs: a private AWS bucket reached with a long-lived key pair cannot be fetched
by this build. It can be fetched through a presigned URL, which is one object at
a time.

Sources: contracts.md Credentials, Provider help records; roadmap.md:135;
standards.md One thing and Failing; `crates/engine/src/credential.rs`.

## Phase 7. Where a credential is looked for, on each platform

Question: contracts.md fixes the order -- `FETCHLOOM_TOKEN_<HOST>`, then the
platform credential store, then the provider-native helper. What each platform's
store actually is was left to this phase.

Options for the second tier: one abstraction with a platform flag; each
platform's own store; a file on both.

Chosen: each platform's own store, because they are different stores and not one
store with a flag.

Windows reads the Windows Credential Manager, a generic credential whose target
name is `fetchloom:<host>`, through `CredReadW`. It is present on every Windows
target, its secret is encrypted at rest under the user's own key, and it costs no
dependency, because the platform crate already links `windows-sys`.

Linux reads a file at the user configuration location, `credentials` beside
`config.toml`, one `host = "token"` entry per line, and refuses it when any user
but its owner can read it, naming the mode it found. The Secret Service was
considered and rejected: it is a desktop daemon reached over a session bus, and
Fetchloom's Linux users are on clusters, in containers and in continuous
integration, where there is no session bus and no daemon, so the tier would be
unavailable in exactly the places it is needed. Speaking D-Bus to reach it would
add the largest dependency in the workspace to serve the machines least likely to
have it.

The third tier ships nothing this phase, because the one adapter built has no
provider-native helper to call. It is asked and answers nothing. That is not a
degradation and emits no `degrade`, because nothing was requested and nothing
lower was used.

The whole lookup is lazy, per standards.md Startup: a command needing no
credential opens no store and reads no file.

Because: first match wins and the source is reported without the secret, which
contracts already fixes. What is decided here is only what the second match is,
and the answer that keeps one behavior on both platforms is per-platform storage
rather than a lowest common denominator that is wrong on both.

Costs: two implementations to keep correct, and a Linux store that is a file
rather than an encrypted vault. The file is refused when its mode is loose,
which is the protection a file can offer.

Uncertain: whether a Linux desktop user would rather have the Secret Service.
They would; they are not who this is for, and nothing stops the environment
variable tier from being fed by one.

Sources: contracts.md Credentials, Configuration files, Environment;
standards.md Startup; roadmap.md:129.

## Phase 7. What redaction covers, and why it is a class rather than a list

Question: roadmap.md:129 asks for the complete redaction target list.
contracts.md already gives it.

Chosen, implemented exactly as written and extended nowhere: bearer tokens, API
keys, passwords, the `Authorization` and `Cookie` headers, the userinfo component
of a URL, and the value of every query parameter. The replacement is the fixed
text `[redacted]`. It applies identically to logs, events, receipts, plans and
error messages, and it happens at construction, per standards.md Observability,
so a secret never exists inside a record that could be written.

Query parameter values are redacted as a class. Every value goes, whatever the
parameter is called, because the alternative is a list of names known to be
sensitive and a list is a thing that can be incomplete. A signed URL's signature
parameter is not called the same thing by two stores, and the one that is missed
is the one that leaks.

Because: this is the one defect in the phase that is a breach rather than a bug.
A rule that holds on the happy path and not under fault is not a rule, which is
why the proving step exercises every stream under fault injection rather than
asserting the redactor in isolation.

Sources: contracts.md Credentials; standards.md Observability.

## Phase 7. What makes an optional credential worth interrupting for

Question: contracts.md sets the threshold at two minutes of projected transfer
time, or a source supporting resume where the alternative does not. What was
undecided is what projected means, given that contracts.md forbids estimating a
field into a number.

Options: project from the size alone; project from the size and the recorded
throughput for the host; do not project and offer on the resume difference only.

Chosen: project from the size the source stated and the throughput recorded for
that host, and make no offer at all when either is missing. A host with no
recorded throughput has no projection, so there is no prompt and no message,
which is what contracts says happens below the threshold.

A projection is not a plan field and is never written to one. It decides whether
to interrupt a person and nothing else, which is why it may be computed at all:
what contracts forbids is estimating a value the source did not supply into a
field that will later be read as fact, and a decision discarded once made is not
that.

The resume half needs no projection. A source supporting resume where the
alternative does not clears the threshold by itself, at any size, because the
difference it makes is not a duration but whether an interrupted transfer starts
again from zero.

Because: two minutes is the point where a person would rather have been asked.
Below it the interruption costs more than it saves, and contracts is explicit
that below the threshold there is no prompt and no message rather than a quieter
one.

Costs: the first run against a host never offers, because nothing has been
measured for it yet. That is the correct silence rather than a missed
opportunity: the run has no evidence and would be guessing.

Sources: contracts.md Credentials, Limits, Plan; roadmap.md:129.

## Phase 7. Cost is two facts, and never a figure

Question: roadmap.md:131 requires cost reporting in plans. The plan shape carried
`credentials` and `terms` and no cost field, and contracts was silent.

Options: a currency estimate; a rate; two facts.

Chosen: an additive per-artifact `cost` field carrying two facts and no money.
`egress_charged` says whether the source charges the requester for bytes leaving
it. `requester_pays` says whether the source refuses to serve them until the
requester accepts that charge. Neither is a currency figure, because contracts
says a field is never estimated into a number and a sum of money is exactly that:
it depends on a price list, a region, a tier and a billing account, none of which
a transfer can read.

A fact the source did not state is omitted. An artifact whose source stated
neither omits `cost` entirely and names it in the plan's `unknown` list, like
every other field a source could not supply.

The seam widens for it. `SourceMetadata` gains a cost the adapter fills in, and
the widening is justified the way a contract change is: contracts now carries the
field, egress cost is already a scoring input at contracts.md Source selection,
and that input had nothing to read. An adapter that knows nothing about cost
states nothing, which is the honest answer rather than a default.

Because: a plan is executed on another machine, possibly months later, and a
figure written into one is wrong by the time it is read. A fact about who is
billed stays true.

Costs: a user who wants to know what a transfer will cost is told who pays and
not how much. That is less than they wanted and all that can be said truthfully.

Sources: contracts.md Plan, Source selection; roadmap.md:131.

## Phase 7. Terms are asserted by a person and recorded, and never interpreted

Question: roadmap.md:129 asks for the terms acceptance flow. contracts.md fixes
it.

Chosen, implemented exactly: a manifest recording `requires_acceptance` refuses
to transfer until the user asserts acceptance, with `--yes` or with an
interactive confirmation. The assertion is recorded in the receipt. Fetchloom
makes no legal determination.

The receipt records it in `accepted_terms`, a field `crates/engine/src/receipt.rs`
has carried since phase 4 and which contracts.md Receipt never documented and no
run ever filled in. contracts.md is amended in this change to state it rather
than to add a second field beside it: the code already had the right shape and
the document was the thing that was wrong.

A run that cannot prompt and was not given `--yes` fails with
`policy.terms_required` and exit 40 before a byte moves, per contracts.md Output
streams: prompts appear only when stdin and stderr are both terminals, and
otherwise a required prompt is a policy failure.

Because: acceptance is an assertion by a person about their own obligations.
Fetchloom's part is to refuse to move bytes until the assertion exists and to
record that it was made, which is what a receipt is for. Reading the license and
deciding what it permits is not something a program can do and not something this
one claims to.

Costs: an automated pipeline against a dataset requiring acceptance must pass
`--yes`, which is a person deciding once rather than a program deciding never.

Sources: contracts.md Terms and licenses, Receipt, Output streams, Exit codes.

## Phase 7. F10 settled: arbitration runs beside Policy, not through it

Question: The audit recorded that Policy is a settings carrier and that phase 7
either widens it to arbitrate between candidates or admits it is one. That
decision is due now.

Options: move source scoring behind `Policy`; leave scoring where it is and say
what Policy is.

Chosen: scoring stays beside Policy, and Policy is what it is.

Source selection at contracts.md Source selection scores on reachability, range
support, immutable identity, recorded throughput, time to first byte, egress
cost and remaining politeness headroom. Every one of those is a measurement or a
statement by the source. None of them is something a user configures. Policy
carries what the user is allowed to do and what they were asked: limits,
verification, durability, the two ceilings, the write path, trust acceptance,
credentials and terms. Routing measurements through it would make a settings
carrier depend on the measurement cache and on a source's answers, which is a
dependency pointing the wrong way under standards.md's rule that dependencies
point inward.

So Policy is a settings carrier and a gate on credentials and terms, and that is
not a defect. Two of its three remaining un-called methods from the phase 6
record now have production callers: `credential` is called on every remote
transfer, and `terms` before any byte of a manifest recording
`requires_acceptance` moves. Fourteen of fifteen methods are now reached by a
run.

`offer_credential` is the one that is not, and it is unreachable for a
structural reason rather than for want of wiring. contracts.md offers an
optional credential only when a source needing one scored better than every
reachable alternative. Scoring two candidates against each other is the probe
phase, and the probe phase does not exist, so no run can ever reach the state in
which an offer is the right thing to do. It is carried forward with the probe
phase and not with the credential work.

Sources: contracts.md Source selection; standards.md Design; audit.md F10.

## Phase 7. F19's probe limit cannot be wired, because there is no probe

Question: The audit deferred the probed-candidates limit on the ground that
there was one source until a later phase. This is that phase, so the limit
should become readable.

It does not, and the reason is worth more than the wiring would have been.

contracts.md Source selection describes a probe phase: candidates are probed in
parallel up to the probe limit, then scored on seven inputs in fixed priority.
No such phase exists. `Transfer::run` calls `order_candidates`, which sorts the
manifest's locations by two things -- recorded throughput and recorded time to
first byte, both read from the measurement cache -- and then tries them in order
with failover. `Source::probe` is called once, against the candidate already
chosen, to learn what to resume against. It is never called to score.

So five of the seven scoring inputs are read by nothing: reachability, range
support, immutable identity, egress cost and politeness headroom. `Limits::
probed_candidates` bounds a phase that does not run, and `source.probe` and
`source.selected` are unemitted for the same reason rather than for want of a
second source.

It is also not buildable as written while transfers are sequential. contracts
says candidates are probed in parallel, and nothing in this build runs two
requests at once, which is the finding recorded earlier in this file. A
sequential probe of four candidates would add four round trips before every
transfer to produce a ranking that a parallel probe was supposed to make free.

`probed_candidates` therefore stays unread, and it is carried forward with the
concurrency work rather than with the source work, because probed in parallel
and transfers in flight are the same missing capability.

Sources: contracts.md Source selection, Limits; `crates/engine/src/tuning.rs`
`order_candidates`; audit.md F19.

## Phase 7. F8: which events a run now emits, and why the rest do not

Question: Six of the thirty-four contracted events were emitted by no production
code. Say which are now emitted and which genuinely belong to a later phase.

One is closed by this phase's credential work. `credential.required` fires when
a source refuses for want of authorization and the policy is re-asked as
required, asserted from the real binary rather than from a policy in isolation.

`credential.offer` and `credential.declined` are not closed. Both are produced
only by `Policy::offer_credential`, which has no production caller and cannot
have one until a run can score a credentialed source against a reachable
alternative. That is the probe phase, and it does not exist. They are carried
forward with it.

Two are closed by the container path. `listing.start` and `listing.end` fire
around an object store listing.

One is not closed and is not a later phase's. `listing.skipped` should fire for
every link an index holds that points outside the prefix, which contracts says
are ignored and counted in the result. `crates/sources/src/index.rs` drops them
silently in `relative`, which is a silent degradation of exactly the kind
standards.md forbids. Reporting them needs the count to cross the Source seam,
and `Source::list` returns a list of entries with nowhere to put it. That is a
seam widening rather than a line of code, and it is named here rather than done
quietly.

`source.probe` and `source.selected` wait on the probe phase, per the record
above. `resolve.alias` belongs to phase 8: it needs a moving alias, and nothing
resolves one.

So of the six, one is closed, two more were closed by the container listing, and
three of the remaining five are blocked on one missing capability rather than on
five separate pieces of work.

Sources: contracts.md Events, Directory listing; standards.md Failing;
audit.md F8.

## Phase 7 gate

Question: Does phase 7 meet its exit criterion, and what remains unproven.

The criterion, from roadmap.md:135: a new adapter can be added by implementing
the seam and passing the shared suite, with no change to the engine.

The answer is yes for behavior and no for selection, and the distinction is the
finding.

**What holds.** `ObjectStoreSource` was written entirely against
`fetchloom_engine::seam::source::Source`. `crates/engine` did not have to learn
what an object store is: no engine type names it, no engine branch tests for it,
and the one engine file the work touched is `transfer.rs`, where the resume
decision now names missing range support as the reason it restarts instead of
blaming the identity. That is the range-degradation rule from contracts.md,
generic to every source, and it would have been equally wrong before the adapter
existed.

The shared suite is real and it can fail. `crates/engine/src/adapter.rs` is
generic over the seam and names no concrete adapter anywhere in its 484 lines.
It runs against three adapters of genuinely different kinds -- the local
filesystem, HTTP, and object storage -- and against two deliberately wrong ones:
a source that claims range support and serves the whole object, and a source
that invents an identity it was never given. Both are caught by name.

**What does not hold.** Adapter selection is not behind the seam. The
composition root decides which adapter serves a reference with one hardcoded
rule -- `is_container`, which is `is_remote(reference) && reference.ends_with('/')`
-- and `crates/cli/src/run.rs` names `HttpSource` and `ObjectStoreSource`
literally at five sites. A third adapter cannot be added by implementing the
seam alone: someone must edit `run.rs` to say when it applies. The seam has no
method by which an adapter states which references it serves, and nothing in
contracts.md says it should, so this is reported rather than invented.

That is a smaller defect than it sounds and a real one. Adapter *behavior* is
behind the seam and is judged by one suite, which is what the phase set out to
prove. Adapter *dispatch* is not, and the next adapter will pay for it.

**Exit criteria, with evidence.**

Every adapter satisfies the same contract test suite, including its degraded
trust behavior. `crates/sources/tests/adapter_suite.rs`, three real adapters and
two wrong ones, nine checks each, degraded trust asserted through `rung_for`
rather than through a second implementation of the ladder.

No adapter emits a secret in any output stream under fault injection.
`crates/sources/tests/secrets.rs` fixes a value and searches the whole error, not
the field that was already right. It found five leaks that a green suite had not:
an unreachable scheme, an unfollowable redirect target, an unrecognized index, an
object store listing refusal and a listing past the entry bound. Every failure
already recorded its location through `SafeUrl`, and every one of them
interpolated the raw location into the message beside it, so a signed URL's
signature reached stderr and the event stream in full while the field one line
away read `[redacted]`. All five are closed. The userinfo component, the value of
every query parameter including one under a name no list would carry, and a
credential dropped on cross-host redirect are each covered.

Sources without range support degrade honestly and say so.
`a_source_without_range_support_names_that_as_the_reason_it_restarts` in
`crates/cli/tests/transfer.rs`, which failed before the change with the run
blaming the identity.

The credential flows work for a first-time user. A run against a source that
refuses for want of authorization exits 40 with `policy.credential_missing`,
emits `credential.required`, and prints the provider's numbered steps, asserted
from the real binary in `crates/cli/tests/credential_and_terms.rs`. A declined
optional credential is not re-offered in the same run. A manifest recording
`requires_acceptance` moves no bytes without `--yes`, proven by asserting the
test server received nothing.

**What is carried forward.**

Concurrent transfer of independent artifacts, which `--concurrency` and
`--per-host` are contracted to bound and bound nothing. This blocks phase 6's
performance half and the probe phase both.

The probe phase, and with it `probed_candidates`, `source.probe`,
`source.selected`, and five of the seven scoring inputs.

`listing.skipped`, which needs the Source seam to carry a count of what a listing
ignored.

The optional credential offer, and with it `credential.offer` and
`credential.declined`. The flow is implemented and unit tested and no run can
reach it, because reaching it means comparing two candidates.

Adapter dispatch, which is not behind the seam.

SigV4 request signing, which is the second credential shape. The phase that adds
it widens `Credential` on purpose, and that widening is the real test of whether
this seam holds.

The provider-native helper tier, which is asked and answers nothing because the
one adapter built has no helper to call.

A credential is resolved once per transfer against the first candidate's host,
so a failover to a second host sends none. That is correct under contracts --
a credential is bound to the host it was resolved for -- and it means a manifest
listing private sources on two hosts can authenticate only the first.

## Phase 7. The verification run, and a baseline that was not re-recorded

`cargo xtask verify` in full, once, at the end of the phase.

```
pass format 0.7 s
pass lint x86_64-pc-windows-msvc 18.8 s
pass lint x86_64-unknown-linux-musl 12.5 s
pass build 0.3 s
pass compile aarch64-pc-windows-msvc 17.1 s
pass test x86_64-pc-windows-msvc 247.4 s
pass comments 0.1 s
pass network 15.6 s
pass benchmark 162.9 s
pass linux image 10.0 s
pass linux suite 1482.2 s
pass linux offline prepare 4.1 s
pass linux offline apply 1.2 s
13 of 13 steps passed, 0 skipped, 4 degradations
```

The four degradations are the ones every run on this machine reports: no ReFS,
small volume or case-sensitive directory without an elevated shell and Hyper-V;
the emulated aarch64 Linux lane runs only behind `--arm`; Windows on ARM compiles
and is not run; and `arm64ec` is refused by the cryptography provider.

The benchmark step gated and passed, which is the statement that matters: step 1
injected 100 ms of latency into every request of the many-hosts regime and moved
no deterministic metric. That regime still reads 40 requests, 6,292,608 bytes
written and 325 file operations, byte for byte what the baseline holds.

The timing baseline was not re-recorded, and the attempt to re-record it is worth
writing down. many-hosts now takes about 12,700 ms where the baseline holds
9,302 ms, and that difference is real: it is the 4,000 ms of injected latency the
regime now pays. But a baseline recorded straight after a thirty-minute
verification run, with the container lane's images and page cache still warm, put
every regime up by half: no-op at 30 ms against a recorded 9.6, cold-cache at
1,877 against 1,537, many-small-files at 11,494 against 7,162. A second run after
a settle was worse rather than better. standards.md is explicit that a timing
measurement that moves while the deterministic metrics are unchanged is evidence
about the machine and is investigated rather than silenced by re-recording, so
the re-record was reverted and the baseline still holds the old numbers.

What that costs is one stale timing number, for a regime whose timing never
gates. What re-recording would have cost is a baseline claiming this machine is
half as fast as it is, which every later phase would then measure against.

The three-configuration comparison in the delay injection record above is
unaffected, because its point was never the absolute numbers. Defaults, a fixed
one and one, and a fixed eight and eight were measured in the same conditions
within minutes of each other, and they agreed within 0.4 percent. A noisy machine
makes all three noisy together.

## Phase 7.5. The machine was loaded, and the injector does not leak

Question: The phase 7 baseline re-record put every regime up by half, four of
which make no network request at all. Either the delay injector reaches paths it
has no business reaching, which would make every number phase 7 measured suspect,
or the machine was busy. Reverting the re-record was right and leaving the cause
undetermined was not.

One full run on a quiet machine separates them, and it did.

| Regime | Baseline | Quiet | |
|---|---|---|---|
| no-op | 9.6 ms | 13.3 ms | |
| cold-cache | 1537 ms | 1169 ms | zero requests |
| warm-cache | 247 ms | 208 ms | zero requests |
| cold-transfer | 122 ms | 100 ms | |
| interrupted-transfer | 318 ms | 466 ms | |
| many-small-files | 7162 ms | 6776 ms | zero requests |
| one-large-file | 1115 ms | 3563 ms | zero requests |
| many-hosts | 9302 ms | 12669 ms | pays 4000 ms of injected latency |

No deterministic metric moved. Three of the four regimes that issue no request
came in below the baseline, and a wait charged per request cannot make a regime
issuing none of them faster. So the injector does not leak, and the phase 7
uniform inflation was the machine.

one-large-file is the one row that reads like a leak and is not. Measured alone
on the same quiet machine it takes 679 ms and 718 ms, both under the 1115 ms
baseline. Its 3563 ms is where it sits in the sequence: it writes 268 MiB
straight after many-small-files has filled the page cache and paid a scanner
ratio of 37 to 47 times. That is a property of running eight regimes back to
back, which the baseline was also recorded under, and it is why a timing number
is published and never gated.

## Phase 7.5. Independent artifacts transfer at once, and what that broke

Question: `--concurrency` and `--per-host` are contracted as in-flight transfer
ceilings and bounded nothing, because no run issued two requests at once. Phase 6
built a controller correct in every unit test and attached it to a loop with one
transfer in it, which is why three configurations whose per-host ceiling differs
eight times over agreed within 0.4 percent.

`fetchloom_engine::flights::Flights` holds a run inside both ceilings. It carries
one controller per host, shared by every transfer to that host, so the count the
controller moves is a count something obeys. It admits the first pending item
whose host has room rather than the next one in order, so a worker never idles
against a busy host while another host's work waits. The global ceiling is the
worker count and the per-host ceiling is what the controller permits at that
moment, which is the pair contracts.md Flags names.

Three decisions inside it that contracts does not state.

The failure reported is the first in index order, whichever finished first. A run
one transfer at a time always reports the failure of the earliest artifact that
fails, because everything before it had already succeeded. Reporting whichever
thread lost first would make the error a property of the clock, and
contracts.md Determinism is a promise about what a run produces.

Nothing at an index above the first failure is attempted, and everything below it
still runs. That is the same set a sequential run reaches, so concurrency changes
no artifact's fate. contracts.md Partial success says objects that verified are
kept in the cache, and they are.

An artifact no host serves is admitted under a per-host count of one. A local
file is not a transfer, so the network ceiling has no business widening it, and
the global ceiling still bounds the run as a whole.

Two real defects were behind this, both of which a sequential run could never
reach and neither of which any existing test could have found.

`Cache::append_to_pack` read the end of this process's pack to learn the offset
it was about to write at, then wrote. Two threads read the same offset, both
appended, and the second recorded an entry pointing into the first one's bytes.
An object then read back as different bytes than it was published with. This is
the corruption the whole project exists to prevent, it survived the eight racing
writers and the thousand-kill loop because both of those race across processes
and each process has its own pack, and it was found by the two-host determinism
test producing two artifacts with one digest.

`record::write` named the file it writes through by process id alone, so two
writers in one process wrote the same temporary name and one of them renamed a
file the other was still writing. `Cache::scratch_path` had the same shape. Both
are now numbered per writer.

The lease already held the single-writer claim per digest, but a second wanter
that had passed the `contains` check before the first committed went on to
transfer the object again after the wait. The check is now taken again after the
claim, which is what contracts.md's wait and reuse says: the second wanter waits
and never starts a second transfer.

Sources: contracts.md Flags, Determinism, Partial success, Cancellation;
`crates/engine/src/flights.rs`; `crates/cache/src/pack.rs`;
`crates/cli/tests/concurrent.rs`; `crates/cache/tests/concurrency.rs`.

## Phase 7.5. The probe phase, and the four readings it needed

Question: contracts.md Source selection describes probing candidates in parallel
up to the probe limit and scoring them on seven inputs in fixed priority. No such
phase existed: `order_candidates` sorted on two recorded measurements and the run
tried the result in order. Five of the seven inputs were read by nothing,
`probed_candidates` bounded nothing, and `source.probe`, `source.selected`,
`credential.offer` and `credential.declined` were emitted by no production code.

`fetchloom_engine::candidate` scores, and `Transfer::select` probes. The order is
the contract's order and nothing was added to it or moved within it.

Four things contracts does not state, decided here.

A source that stated nothing about cost is neither preferred nor refused. Cost
separates two candidates only when both stated it; otherwise neither scores and
selection falls through to politeness headroom. The other absent-input rule
contracts does write down, that a host with no measurement never goes ahead of
one with a measurement, does not transfer: applying it to cost would rank a
source known to charge the requester ahead of one that said nothing, and reading
silence as evidence in either direction is a claim contracts does not make. This
one was put to the user rather than chosen.

A run with one candidate spends no probe on it. contracts says the chosen source
and the reason are recorded, and they are, with the reason that the manifest
named one source. What it does not say is that a request must be spent deciding
between one thing. Probing anyway would double the requests of every
single-source fetch and decide nothing, and the deterministic request counts the
benchmark gates would have moved for no gain.

The probe limit bounds how many candidates are probed, not how many probes run at
once. Candidates past the limit keep their manifest order behind the probed ones
and are still available to failover. `Limits::probed_candidates` already carried
the docstring "Most candidate sources probed in parallel", which reads the same
way.

The optional credential offer is projected from the two hosts, over the length
the taken candidate stated. A probe refused for want of a credential says nothing
about the object, so there is nothing to compare on ranges or on identity, and
under the fixed scoring a candidate that did not answer can never score better
than one that did. The only difference there is to measure is between what this
run has recorded about the two hosts. With no measurement behind either, or no
stated length, nothing is projected and nothing is offered, which is what
contracts means by no prompt and no message below the threshold.

Two consequences worth naming.

A source that answers nothing is no longer failed over to. It is scored
unreachable and the reachable candidate is taken, which is what the fixed
priority says and which spends fewer requests than trying a dead source first.
`source.failover` and its degradation are still reached, by a source that answers
the probe and then serves bytes that do not hash to what was stated. The two
tests that asserted failover were rewritten around that, because they had been
asserting the sequential order rather than the contract.

A non-interactive run now emits `credential.offer` before `credential.declined`.
contracts says an optional credential is reported as an unused opportunity and
the alternative is used. Reporting only the decline says an opportunity was
refused without ever saying one existed, which is not a report. The run still
never blocks, and the test proving that drives a prompter that panics if asked.

The seam bound moved. `Transfer` now requires `S: Source + Sync`, because probing
in parallel means the adapter is used from more than one thread. That is a bound
at the use site rather than a method on the seam, so no adapter implements
anything new, and every adapter this build has already satisfies it. It is a
requirement on implementors all the same and it is named here rather than left to
be discovered.

`Transfer::credential` is now a resolver keyed by host rather than one credential
resolved for the first candidate's host. contracts.md under Credentials forbids sending host
A's credential to host B; it does not forbid resolving host B's own when the
transfer moves to host B, and features.md's promise that credentials are scoped
to the host they were issued for is what resolving per host honors.

`Host::of_location` returned `[` for a bracketed IPv6 literal. Every measurement,
credential and in-flight count for an IPv6 host was filed under that. The
two-host benchmark regime and the two-host determinism test both serve one of
their hosts on `::1`, so both had been measuring a host named `[` since phase 6.

Sources: contracts.md Source selection, Credentials, Limits;
`crates/engine/src/candidate.rs`; `crates/cli/tests/probe.rs`;
`crates/engine/tests/candidate.rs`.

## Phase 7.5. Splitting one object, and the two numbers that decide it

Question: features.md says range requests split a single object only when the
source is immutable, supports ranges, the object is large, and measurement showed
a gain. Nothing implemented it. `repair.rs` fetches ranges, which is a different
thing: it refetches spans of an object already held, against an outboard tree.

The four conditions were written down; the two numbers behind them were not, and
this phase decides both.

The size threshold is 64 MiB, which is the outboard threshold's number. That is
already where this project draws the line between an object and a large object,
and drawing a second line at a different number would mean two answers to one
question. It is given a name of its own, `Limits::split_threshold`, because it
bounds a different decision and because a test has to be able to see it.

The gain test is the per-host concurrency this run has already recorded. The
adaptive controller raises that count only after a host answered more requests in
flight cleanly and lowers it the moment the host asks to be left alone, so a
recorded count above one is exactly the statement that a second stream to this
host is worth opening. Nothing new is measured for splitting, which is the point:
inventing a second measurement would mean a second thing to keep true. A host
with no recorded measurement, and one recorded at one, both refuse the split and
say so. The width is that recorded count bounded by what the politeness ceiling
permits at the moment, so a split never opens a stream the ceiling would not.

The spans are written in the order they cover the object, because both digests
are taken as the bytes arrive and the interop digest is a SHA-256 that cannot be
taken out of order at all. Each span streams into a channel bounded at two
buffers, and the writer drains them in order and hands the buffers back, so
memory is bounded at the span count times two buffers and no buffer is allocated
per chunk. A span that runs ahead of the writer blocks, which is the backpressure
standards asks for rather than a queue that grows.

A source that serves one span under a different identity than another fails with
`source.identity_changed`. The immutability condition is what makes a split safe,
and checking it once at the probe would not catch a source that changed under the
transfer.

Over plain HTTPS no object is ever split, and that is correct rather than a gap.
contracts.md's second resume rung is an immutable content address or a provider
version identity; an entity tag is a strong validator and is the third rung. So
splitting reaches only the object storage adapter today, and a large object from
an ordinary web server records a `degrade` saying the source states no identity
that cannot change under the same name.

`WriteRate` was reading the socket, not the volume. It judged whichever length
`read` happened to hand over, so on a loaded machine a 16 KiB write timed against
a faster 16 KiB write read as a collapse, drove the host's count down, and made
a phase 6 test fail about one run in three. It now gathers a whole buffer before
judging anything. The test that proves a collapsing volume lowers concurrency was
changed with it: its writer had been slow from its second write onward, which
under the window rule is a volume that is uniformly slow rather than one that
collapsed, and it now accepts two megabytes at speed before it stalls.

Sources: contracts.md Splitting one object, Limits; `crates/engine/src/split.rs`;
`crates/cli/tests/ranged.rs`; `crates/engine/tests/split.rs`.

## Phase 7.5. Two counts the seam did not carry, and dispatch that named adapters

contracts.md under Configuration files has always said that links pointing outside the listed prefix
are ignored and counted in the result. The count had nowhere to live: `list`
returned a bare vector of entries, so `listing.skipped` was in the event registry
with no caller anywhere in the build. `list` now returns a `Listing` carrying the
entries and the count, and a run emits the count between `listing.start` and
`listing.end`.

A link that resolves to the container itself is not outside the prefix and is not
counted. A relative escape and an absolute link to somewhere else both are. That
distinction is the whole of the counting rule and it is the only thing the two
parser tests assert.

`run.rs` decided what to do with a reference by asking `is_remote(reference) &&
reference.ends_with('/')` and then named `HttpSource` or `ObjectStoreSource` at
five sites. roadmap.md:135 says a new adapter can be added by implementing the
seam and passing the shared suite, with no change to the engine, and that was
false: a third adapter would have had to be named in `run.rs` to be reached at
all. The Source seam now carries `serves`, by which an adapter states whether it
serves a reference and whether it serves it as one object or as a container to be
listed, and `take_degradations`, which every adapter already had as an inherent
method and which the "nothing degrades silently" rule makes seam behavior rather
than an adapter's private business.

Dispatch runs through an `Adapters` registry of `AnySource`, one adapter held with
its body type forgotten behind `Box<dyn Read + Send>`. The registry is asked in
order and the first adapter that claims a reference gets it, so registration order
is the precedence rule and there is one place that states it. `Transfer` stays
generic over the seam rather than taking the erased type, so nothing about the
transfer path changed.

This is internal shape and no portable artifact carries it, so it is a code
decision and contracts.md is unchanged.

`is_remote` is gone. It answered "does this start with http", which was never the
question; the question is whether any adapter serves it. `is_served(adapters,
reference)` and `is_container(adapters, reference)` ask the registry. `FileSource`
is not registered, because a local path is read as a path rather than fetched
through the Source seam, and putting it in the registry would have made
`is_served` true for every file on the machine. Its inherent `serves` was renamed
`names_a_file` so that one name does not mean two things.

`open_for` no longer makes the work counter, because the registry is built from
that counter and the registry has to exist before a reference can be classified,
which happens before the cache is opened. The counter is made once at the top of
each command and passed in.

Proved by a third adapter that exists only in a test: `InventedSource` serves a
scheme this build ships no adapter for, is registered ahead of the two real ones,
and a manifest naming that scheme materializes through it with the right digest.
`run.rs` names it nowhere. Before dispatch went through the registry that test
failed with `reference.unresolved`, asking for a path that exists, which is
exactly the old behavior: an unrecognized scheme fell through to being treated as
a local file.

Sources: contracts.md Listing; roadmap.md:135;
`crates/engine/src/erased.rs`; `crates/engine/src/seam/source.rs`;
`crates/cli/tests/adapters.rs`; `crates/cli/tests/contract.rs`;
`crates/sources/src/index.rs`.

## Phase 7.5. Per-candidate credentials, and the sixth leak site there was not

contracts.md under Credentials forbids sending host A's credential to host B. That is the
drop-on-redirect rule and it stands. It does not forbid resolving host B's own
credential when a transfer moves to host B, and a run that failed over to a
second candidate without doing so would fail on a host it holds a credential for.

Resolution was already per candidate: `Transfer::credential_for` takes the
location it is about to request and asks the host of that location, at the moment
the request is built, so there is no point at which one host's credential is in
hand while another host's request is being made. Nothing was rebuilt for this.
What was missing was the assertion.

A run now fails over from a host it holds a secret for to a host it holds nothing
for, under a source that answers the probe and then serves wrong bytes, and the
test asserts four things: that the first host did receive the secret, so that the
test can fail at all; that no request to the second host carried it in any header;
that it reached neither the event stream, the result stream, nor the progress
stream; and that it reached no receipt.

Phase 7 found five leak sites, each a redacted field one line from a raw
interpolation. There is no sixth of that shape: `Secret::expose` is called at
exactly one place in the build, the request builder in `crates/sources/src/http.rs`, where it becomes
the `Authorization` header of a request already bound to a host. Every other path
carries the redacted form.

Sources: contracts.md under Credentials; `crates/cli/tests/credential_and_terms.rs`;
`crates/engine/src/transfer.rs`.

## Phase 7.5. The count rises while throughput improves, which it did not before

features.md under Adaptive performance says the connection count per host rises while throughput improves
and falls when it does not. The controller rose on a clean answer and fell on a
failure, and nothing anywhere compared what a host delivered at one count against
what it delivered at another. The first half of that sentence was false.

The controller now gathers what the host delivered at the count it is currently
permitted, and a clean answer is judged against it. Below a window of bytes at the
current count nothing has been measured, so a clean answer rises exactly as before:
the second stream is what measures whether a second stream helps, and refusing to
open it would mean never measuring anything. Once a window has been delivered, a
clean answer rises only if the rate at this count beats the rate at the count
below, and otherwise gives up this count for the run, falls back one, and never
reaches it again.

The rate compared is the host's, not one transfer's. Per-transfer rate falls as
the count rises even when the host is delivering more in total, so judging on it
would cap every host at two. Bytes and elapsed time from every transfer that
completed at a count are summed, which is the aggregate the sentence is about.

A count given up is given up for the run and not for the recorded measurement:
what is written to the cache is the count the run settled at, which is what the
next run starts from. A host whose capacity changed between runs starts at the old
count and finds its own ceiling again from there.

Rate limits and failures still halve and step down without consulting throughput.
A host asking to be left alone is not a measurement of capacity and is not treated
as one.

Sources: features.md under Adaptive performance; `crates/engine/src/tuning.rs`;
`crates/engine/tests/tuning.rs`.

## Phase 7.5. The regime matrix with real concurrency, and what it did not prove

roadmap.md:123 says phase 6 is done when default settings beat hand-tuned fixed
settings across the regime matrix. The matrix was re-run with concurrency real,
defaults against `--concurrency 4 --per-host 4`, which is what a competent person
picks when told to fix the numbers: the politeness ceiling for a host and one
transfer per core on a small machine.

Defaults do not beat hand-tuned settings. They tie with them. That is the answer
and roadmap.md:123 is not closed by it.

Six of the eight regimes issue no network request at all, so no concurrency
setting can change what they do, and on this machine their wall times swing wider
than any setting could move them. cold-cache measured 979, 1435, 1374 and 1008 ms
in four consecutive runs alternating between the two settings, with the two
fastest and the two slowest split one each across them. Nothing can be concluded
from those six rows and nothing is.

cold-transfer and many-hosts are the two that reach a network. cold-transfer moves
one object, where there is nothing to hold in flight. That leaves many-hosts, and
on it defaults measured 9694 ms against 9654 ms hand-tuned at four, 9706 ms at
two, 9742 ms with a global of eight, and 11058 ms with the per-host ceiling fixed
at one. Defaults land inside the noise of every plausible fixed setting and beat
only the implausible one.

The reason is in the regime rather than in the controller. Half of many-hosts'
servers answer 429, and a host that asks to be left alone has its count halved to
one, so half the regime runs at one transfer in flight by contract and no ceiling
above one changes it. The recorded measurements from a run say exactly this: the
clean host settles at four, the rate limited host at one. The regime was not
adjusted to make the number better.

The many-hosts row on benchmarks.md was re-attributed. It said the run issues its
forty requests one after another, which was true when it was written and is not
now. It says instead that the requests are held several in flight per host, that
the injected latency therefore costs a fraction of the four seconds it would cost
serially, and that most of the wall is backoff the rate limited host asks for and
this run waits out one request at a time.

The published numbers were re-recorded on a quiet machine. The many-hosts ratio
moved from 22.66x to 4.58x, and almost none of that is Fetchloom: its own number
moved 8581 to 9672 ms, and curl moved 379 to 2114 ms on the same regime. The
ratio fell because the alternative got slower, which is a different claim from the
code getting faster and is published as such.

Sources: roadmap.md:123; `docs/benchmarks.md`; `xtask/src/bench.rs`;
`benchmarks/x86_64-pc-windows-msvc.json`.

## Phase 7.5. features.md reconciled against the binary

features.md describes the product to a reader in the present tense, and a reader
had no way to tell which sentences the binary honors. Every claim was walked
against the build. Where a claim is true it stands unmarked; where it is not, the
sentence now says so and names the phase that owns it. The document states that
convention at the top, so an unmarked sentence is a promise about today.

Five of the six sentences named at the start of this phase are now true and were
made true by it: independent artifacts transfer concurrently inside both limits;
a single object splits only under all four conditions; several sources are probed
in parallel and one is picked without racing; the count per host rises while
throughput improves; and the credential offer names the two options with the
measured difference, which it had been able to compute since the probe phase and
had no caller for before it.

The sixth was corrected rather than built. Credential sources are environment
variables and platform credential stores. Provider-native helpers are not there,
and phase 7 owns them.

Three more claims failed the walk and were corrected. Protocol choice is not
measured per host and cannot be: the HTTP client this binary is built on speaks
HTTP/1.1 and offers no second protocol, so there is nothing to choose between and
no phase owns making one. The post-run hint, and the `--no-hints` flag that turns
it off, are in contracts.md and in no phase; the surface tests assert that the
flag and the setting are refused rather than accepted and ignored, so the build is
consistent with itself and the document was the thing that was wrong. The live
display degrades to the plain view and says so, and `watch` does not exist, so the
sentence promising a run rendered from another terminal was corrected too.

Reference forms, listing formats, `init`, metadata absorption, and `doctor` were
found overclaimed in the same way and marked with the phase that owns each. The
binary was already honest about all of them at the point of refusal; only the
document was not.

Deliberately deferred with the reason recorded: SigV4 request signing, the
provider-native helper tier, F21's measurement on a cloning volume, and F26's
million-object measurement.

Sources: `docs/features.md`; `docs/roadmap.md`; `crates/cli/src/terminal.rs`;
`crates/cli/tests/contract.rs`; `crates/cli/tests/precedence.rs`.

## Phase 7.5 gate

Question: Are the six sentences this phase named true, and what did closing them
leave still open.

**The six, item by item.**

*features.md under Transfer, independent artifacts transfer concurrently inside global and
per-host limits.* True. `crates/engine/src/flights.rs` is the scheduler and
`crates/cli/tests/concurrent.rs` proves it three ways: eight artifacts across
eight hosts complete in materially less than eight times one artifact's time; a
per-host ceiling of one and of four measurably differ against one host; and no
setting changes the digests or the tree digest a run produces. Ordering is
deterministic under it, which was the first test written and the one that had to
fail first: outputs stop at the first failure in manifest order whichever thread
finished first, asserted in `crates/engine/tests/flights.rs`.

*features.md under Transfer, a single object splits only under four conditions.*
True. `crates/engine/src/split.rs` checks size, immutable identity, range
support, and a measured width, in that order, and `crates/engine/tests/split.rs`
asserts each refusal by name. `crates/cli/tests/ranged.rs` drives a real object
store: four ranges at once, one digest whether split or whole, and a `degrade`
naming the condition that failed when it was not split.

*features.md under Transfer, several sources are probed cheaply in parallel and one is
picked.* True, and not raced. `crates/engine/src/candidate.rs` holds the fixed
scoring order from contracts.md under Source selection, ties break by manifest order, and at most
`probed_candidates` are asked. `crates/cli/tests/probe.rs` asserts that a losing
candidate is never asked for bytes, that one source is not probed at all, and
that the reason is recorded in the receipt.

*features.md under Adaptive performance, the count per host rises while throughput improves.* True as of
this phase. The controller sums what a host delivered at the count it permits and
raises only when that beats the count below, giving up a count that did not help
for the rest of the run. Below a window of bytes nothing has been measured and a
clean answer rises as before, because the second stream is what measures whether
a second stream helps. `crates/engine/tests/tuning.rs`.

*features.md under Credentials and access, the offer names the two options with the measured difference.*
True. It could compute the difference before this phase and had no caller;
scoring two candidates gave it one. `crates/cli/tests/credential_and_terms.rs`
asserts a real run offers and, being unable to ask, declines and says so.

*features.md under Credentials and access, provider-native helpers.* Not built, and the sentence is
corrected rather than the tier built. Phase 7 owns it.

**Three more the walk found.** Protocol choice is not measured per host and
cannot be on an HTTP/1.1-only client; the post-run hint and its flag are
contracted, in no phase, and asserted absent; the live display degrades to plain
and `watch` does not exist. All three sentences are corrected.

**What closing them broke, and what that says.** Concurrency reached three
defects nothing else could. Two appends to one process's pack read the same
end-of-pack offset, so one entry pointed into another's bytes -- silent
corruption in the thing this project exists to prevent, which survived both the
eight-racing-writers test and the thousand-kill loop because those race across
processes and each process has its own pack. Every measurement, credential and
in-flight count for a bracketed IPv6 host had been filed under `[` since phase 6.
And `WriteRate` was judging whichever length the socket handed over, reading
socket jitter as a disk collapse about one run in three. None of the three is a
concurrency bug. Concurrency is what made them observable.

A fourth surfaced only in the container lane, on the last verify of the phase. A
volume that collapsed under a transfer lowered the count, and then the same
transfer's own success raised it straight back, so on Linux -- where the copy
does fewer, larger writes and the collapse is detected once rather than several
times -- the disk's signal was erased by the transfer that produced it. A
transfer that lowered the count no longer raises it by succeeding.

**Answering the question directly.** Are `--concurrency` and `--per-host`
bounding real in-flight transfers? Yes. Does a per-host ceiling of one measurably
differ from four? Yes: on the many-hosts regime, medians of five, one measured
11058 ms against 9694 ms settled and 9742 ms fixed at four, a 14 percent
difference. That is a real separation and it is smaller than it should be,
because half that regime's servers answer with a rate limit and a host asking to
be left alone runs at one transfer in flight whatever the ceiling says.

**What is not closed.** roadmap.md:123 asks that default settings beat hand-tuned
fixed settings across the regime matrix. They tie. Six of the eight regimes issue
no request and cannot answer the question, one moves a single object, and on the
one that remains defaults land inside the noise of every plausible fixed setting.
This phase does not close phase 6's performance half, and says so rather than
closing it.

Sources: `docs/features.md`; `docs/roadmap.md`; `crates/engine/src/flights.rs`;
`crates/engine/src/candidate.rs`; `crates/engine/src/split.rs`;
`crates/engine/src/erased.rs`; `docs/benchmarks.md`.

## Phase 7.5. The volume's signal was erased by the transfer that produced it

The container lane failed one test on the phase's first full verify:
`a_volume_that_collapses_mid_transfer_lowers_concurrency_and_moves_the_same_bytes`
passed on Windows and failed on Linux, and the failure was real rather than
flaky.

A collapse lowers the count from inside the copy. The transfer then succeeds, and
a first-attempt success is a clean answer, which raises the count by one. On
Windows the copy makes many small writes, the collapse is judged several times,
and the count falls far enough that one clean answer does not undo it. On Linux
the copy makes fewer, larger writes, the collapse is judged once, and the same
transfer that lowered the count raised it straight back. The assertion is about
the count after the transfer, so only Linux could see it.

Both platforms were wrong; only one of them was wrong loudly. A transfer that
lowered the count no longer raises it by succeeding: the controller remembers
that it was held down and the next clean answer clears that memory instead of
adding one. The rule applies to a rate limit as well, though nothing reaches it
there, because a retried transfer never reports a clean answer in the first
place.

Sources: `crates/engine/src/tuning.rs`; `crates/engine/tests/tuning.rs`;
`crates/cli/tests/transfer.rs`.

## Phase 8. Five commands move here, and what phase 9 keeps

Question: roadmap.md:150 lists `doctor`, `why`, the live display mode and `watch`
under phase 9, Ship. Phase 8's own measure is that every feature exists. Which
phase owns them.

Options: leave them in phase 9 as written; move them into phase 8; split them.

Chosen: move all five, hints included.

Ship is distribution. Packaging targets, install paths, signing, notarization,
offline signature verification, package manager entries, and the compatibility
freeze are all facts about how a binary reaches a machine, and none of them is a
thing the binary does once it is there. `doctor`, `why`, the live view, `watch`
and the post-run hint are the second kind. They were listed under Ship because
Ship was where the command surface was finished, not because any of them is
distribution, and phase 8 is now the phase whose exit criterion is that the
surface is finished.

The live view has a second reason. contracts.md's Display modes make it a
consumer of the event stream with no other input, and phase 9's Prove clause
leans on that property. A property is cheaper to establish while the events it
reads are still being added than to retrofit onto a renderer written after them.

roadmap.md is amended: phase 8 gains `init`, the metadata readers, the remaining
reference forms, `doctor`, `why`, the live display mode, `watch`, and hints.
Phase 9 keeps static binaries, signing, notarization, checksums and signed
release metadata, package manager entries, no-root install, shell completion
verification on release artifacts, and the 1.0 freeze.

Costs: phase 9's Prove clause about display modes is now a phase 8 obligation
tested a phase early, and phase 9 re-runs it against release artifacts rather
than writing it.

Sources: docs/roadmap.md; docs/contracts.md Display modes, Hints, Command
surface.

## Phase 8. Where `init` writes

Question: contracts.md under Command surface says `init` infers and writes a manifest and never
says to what. `plan` has an explicit rule and `init` has none.

Options: standard output only, like `plan`; a file only, with a default name; a
file, with standard output reachable.

Chosen: standard output by default, and `--output <path>` writes a file instead.

A manifest is the result of the command, which is the rule contracts.md already
fixes for `plan`, so standard output is the default and nothing else is written
there. Piping it into a file is what a person does the first time and reading it
before saving it is what they do the second, so a default that prints costs
nothing and a default that writes a file costs a surprise.

`--output` is one destination argument rather than a second code path: the
emitter writes to a stream, and the stream is standard output or a file. It is
the name `get` and `apply` already use for where a result is placed.

A file that already exists is not overwritten. `init --output` onto an existing
path fails with `destination.unrepresentable` naming the path, and `--force`
overwrites, which is the meaning `--force` already carries. A manifest is a thing
a person edits after generating, and silently replacing an edited one is the
class of loss this project refuses everywhere else.

Costs: two destinations means two tests for one emitter rather than one.

Sources: docs/contracts.md Plan, Command surface, Flags; docs/standards.md One
thing.

## Phase 8. The second credential shape, and where SigV4's three values come from

Question: phase 7 carried SigV4 forward as the deliberate second credential
shape and named `Credential { host, origin, value: Secret<String> }` as the type
that has to widen. contracts.md names one credential variable,
`FETCHLOOM_TOKEN_<HOST>`, and says nothing about an access key, a secret key, or
a region.

Options: Fetchloom-namespaced host-scoped variables only; the AWS variables only;
both, in the lookup order contracts already fixes.

Chosen: both, mapped onto the three tiers that already exist.

`Credential` becomes a second shape rather than a wider first one. `Bearer` holds
the one opaque value that is sent. `Signing` holds an access key, a secret key,
an optional session token, and a region, and the secret is never sent: it derives
a signing key that signs a canonical request. Packing `key:secret` into the one
string is refused for the reason phase 7 gave, and widening `Bearer` with three
optional fields would be the same defect wearing a struct.

Placement follows contracts.md's existing lookup order rather than inventing one.
`FETCHLOOM_ACCESS_KEY_<HOST>`, `FETCHLOOM_SECRET_KEY_<HOST>`,
`FETCHLOOM_SESSION_TOKEN_<HOST>` and `FETCHLOOM_REGION_<HOST>` are the first
tier, host-scoped by exactly the rule `FETCHLOOM_TOKEN_<HOST>` uses, so an
explicit per-host credential wins. The platform credential store is the second
tier, unchanged. `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
`AWS_SESSION_TOKEN` and `AWS_REGION`, and the profile in the shared credentials
file, are the third tier, which is the provider-native helper contracts.md under Credentials
already names and which has answered nothing until now because the one adapter
built had no helper to call.

That is not two ways of doing one thing. The tiers are different mechanisms with
a fixed order, which is what the contract describes, and the third tier is a
provider's own convention rather than a second spelling of the first. It is also
what a person with a working AWS setup already has, and asking them to restate it
under a new name to use one tool is the kind of setup step contracts.md's Never
present setup before it is needed exists to prevent.

An access key found without a region fails with `policy.credential_invalid`
naming the region as what is missing, because a signature is computed over a
region and guessing one produces a signature that fails at the source with an
error the user cannot act on.

HMAC-SHA256 is implemented here rather than taken as a dependency.
standards.md prefers no dependency over a small one, HMAC is a fixed construction
of the hash already in the tree, and RFC 4231 publishes the test vectors that
make it verifiable rather than trusted.

Costs: four environment variables and one file format enter the surface, and the
provider helper tier now has a platform-independent implementation where phase 7
expected a platform-specific one.

Sources: docs/contracts.md Credentials, Environment; docs/standards.md Language;
`crates/engine/src/credential.rs`; the phase 7 record on bearer only.

## Phase 8. Source priority, and how a bare name resolves

Question: contracts.md under Selection resolves a bare name and a namespaced release through
"configured source priority", and no configuration key for it exists anywhere in
contracts.md.

Options: an ordered list of base locations; a named registry table with explicit
priorities; leave both forms unbuilt.

Chosen: an ordered list of base locations, `sources`, in project or user
configuration.

```toml
sources = ["https://lab.edu/data/", "hf:datasets/acme/"]
```

A bare name is appended to each base in order and the first that resolves wins. A
namespaced release appends the same way, so `acme/imagenet@2012` against a base
is one concatenation rather than a second grammar. Nothing is guessed: a name
matching no base fails `reference.unresolved` naming how many bases were tried,
and a run with no `sources` configured fails naming that none is configured, so
the failure tells a first-time user what to do rather than that the name is
wrong.

A list is chosen over a named table because a name buys one thing, which is that
`why` can say which entry answered, and `why` can say the base itself instead. A
table buys per-source options that nothing needs yet, and contracts.md's Unknown
keys are an error rule means adding the table later is a change and adding
options to a list later is also a change, so nothing is bought by deciding now.

Resolution order is contracts.md's own and does not move: an explicit scheme
first, then a local path if it exists, then this list. A bare name that is also
a directory in the working directory is that directory, which is what a person
means when they type it.

Costs: a base that is a prefix of another resolves the shorter one first, which
is order-dependent and is exactly what an ordered list is for.

Sources: docs/contracts.md Reference grammar, Configuration files.

## Phase 8. What a log level is, and what `--verbose` raises

Question: contracts.md's Flags table gives `--verbose` the meaning "raise log
level, repeatable" and its Environment table gives `FETCHLOOM_LOG` the meaning
"log level". Neither says what a level is. No level exists in the binary and
`FETCHLOOM_LOG` is read by nothing.

Options: three levels rendering events already contracted; four levels, the
fourth carrying lines no event backs; a bare verbosity count with no names.

Chosen: three named levels, and a level decides only which already-contracted
events are rendered to standard error.

`error`, `info`, `debug`. `info` is the default. `error` renders the `error` and
`degrade` payloads. `info` renders those plus the run's own start, end and result.
`debug` renders every event the stream carries, one line each. `--verbose` steps
up one level and is repeatable; a step past `debug` is clamped and the clamp is
reported, which is the rule `--threads` already follows. `FETCHLOOM_LOG` takes a
level name and sits at the environment precedence level like every other setting.

The decisive constraint is standards.md's rule that log level controls verbosity
and never which events exist, and contracts.md's rule that the live view can
display nothing the event stream does not carry. A fourth level whose lines have
no event behind them would be a second observability channel and would make the
live view structurally unable to show what a log can, which is the property step
7 has to prove. So a level is a filter over the one stream and never a source of
its own.

`--events` output is byte-identical at every level, which is a test.

Costs: someone wanting socket-level tracing does not get it from this binary, and
gets it from the event stream or not at all.

Sources: docs/contracts.md Flags, Environment, Display modes; docs/standards.md
Observability.

## Phase 8. Protocol choice, re-checked rather than recalled

Question: features.md says this build speaks HTTP/1.1 because the client offers
nothing else, and marks measuring protocol choice per host as unbuilt with no
phase owning it. CLAUDE.md requires that claim to be confirmed against current
documentation rather than carried.

Checked against ureq 3.4.0, the pinned version. Its documentation describes body
transfer in terms of HTTP/1.1's two mechanisms and names no other version;
`ConfigBuilder` exposes no protocol selection among its methods; HTTP/2 appears
nowhere in the crate's API or prose.

The claim holds. The marker stays and no phase owns it, which is honest: there is
one protocol to speak and nothing to measure. It becomes a decision again only if
the client changes, which is a dependency change with its own justification.

Checked at the same time and worth recording, because it was assumed and never
verified: contracts.md's `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY` are honored.
`Agent::config_builder()` starts from `Config::default()`, whose `proxy` field is
`Proxy::try_from_env()`, which reads `ALL_PROXY`, `HTTPS_PROXY` and `HTTP_PROXY`
and applies `NO_PROXY`. No Fetchloom code names them, which is why nobody had
confirmed they work.

Sources: https://docs.rs/ureq/3.4.0/ureq/, its `config` and `Proxy` pages;
docs/features.md Adaptive performance; docs/contracts.md Environment.

## Phase 8. Which metadata formats map onto the manifest, and which cannot

Question: roadmap.md:141 asks which of the seven formats features.md names map
cleanly onto the manifest model and which cannot be represented honestly. Each
was read against its own specification rather than from memory.

The manifest model an artifact has to fit is `id`, ordered `sources`, `size`,
`digest` holding a BLAKE3 and a SHA-256, `media_type`, `archive.format`, `select`
and `layout`. Two things decide representability: whether the document states a
digest in one of those two algorithms, and whether it states a location the bytes
can be fetched from.

**Represented.**

*Checksum sidecars.* `<hex>  <path>` per line, which is the `sha256sum`,
`shasum` and `sha256` sidecar convention. A 64-character hexadecimal digest is
SHA-256 and becomes `digest.sha256`; the path becomes the artifact id and
resolves against wherever the sidecar itself was read. A 32-character digest is
MD5 and a 40-character one is SHA-1, and both are refused by name.

*Croissant.* A `FileObject` carries `contentUrl`, `contentSize`, `encodingFormat`
and `sha256`, which are `sources`, `size`, `media_type` and `digest.sha256`
exactly. A `FileSet` whose `containedIn` names one `FileObject` is that object's
artifact with the `FileSet`'s `includes` as `select` and its `excludes` applied
after, which is contracts.md's own selection rule. A `FileSet` whose `containedIn`
names several objects, or another `FileSet`, is refused: it is one logical
collection drawn from several archives, and the manifest's selection belongs to
one artifact.

*Frictionless data packages.* `resources[].path` or `url`, `bytes`, `mediatype`
and `hash`. The specification's default for a bare `hash` value is MD5 and other
algorithms are indicated by a lower-case prefix, so `sha256:` is read and a bare
value, `md5:` and `sha1:` are refused by name. `name`, `version` and
`licenses[].name` become `name`, `release` and `license.spdx`.

*pooch registries.* `<path> <hash>` per line, `#` comments. A bare value is
SHA-256 and a prefixed one is read only when the prefix is `sha256`. The base URL
is not in the registry file at all -- pooch holds it in the calling code -- so
entries resolve against where the registry was read, which is stated rather than
guessed.

*BagIt.* `manifest-sha256.txt` is `<hex> <filepath>` and `fetch.txt` is
`<url> <length> <filepath>`, which together give a source, a size and a digest per
payload file. A length of `-` is unspecified and the `size` is omitted rather
than estimated. A bag with no `fetch.txt` is a local bag and its payload resolves
under `data/`.

**Refused, by name, with a test for the refusal.**

*Torrent piece hashes.* In version 1 these are SHA-1 over fixed-size pieces that
span file boundaries, so a piece hash proves a piece and there is no per-file
digest at all. In version 2 the per-file `pieces root` is the root of a merkle
tree of 16 KiB blocks under SHA-256, which is a different value from the SHA-256
of the file and cannot be compared to one. Neither is the content digest and
neither is the interop digest. A torrent also names a swarm rather than a
location bytes can be fetched from. Two independent reasons, and approximating
either would be recording a number under a name that does not mean it.

*DVC files.* `outs[].hash` is MD5 and nothing else for a local or SSH output, and
an ETag for an HTTP, S3 or Azure one -- and contracts.md's Verification and
repair rule is that an ETag is supporting evidence and never content identity.
The remote is a name resolved through DVC's own configuration rather than a URL
in the file, so the document states neither a usable digest nor a location.

**What this costs, stated rather than hidden.** BagIt's own specification
recommends SHA-512 by default, so a bag published with `manifest-sha512.txt` and
no SHA-256 manifest is refused even though it carries a strong per-file digest.
The manifest model carries two algorithms and adding a third is a change to
identity, which is not a thing an ingest reader gets to make. The refusal names
the algorithm the bag used and the two the manifest carries.

features.md is corrected in the same change. It claimed all seven were absorbed;
five are, two are refused by name, and the sentence now says which.

Sources: https://docs.mlcommons.org/croissant/docs/croissant-spec.html;
https://datapackage.org/standard/data-resource/;
https://www.fatiando.org/pooch/latest/registry-files.html;
https://www.rfc-editor.org/rfc/rfc8493.html;
https://www.bittorrent.org/beps/bep_0052.html;
https://doc.dvc.org/user-guide/project-structure/dvc-files;
docs/contracts.md Manifest, Identity, Selection.

## Phase 8. A manifest's SHA-256 was never compared to anything

Question: five of the five representable metadata formats state a SHA-256 and
none states a BLAKE3. What happens today when a manifest states a SHA-256.

Nothing happens. `ingest_artifact` in `crates/cli/src/run/dataset.rs` takes the expected digest from
`artifact.digest.and_then(|claims| claims.blake3)` and falls back to the lock. The
`sha256` field of a manifest is parsed, is written back out, and is compared
against no bytes anywhere. A publisher can state a SHA-256 and Fetchloom will
serve bytes that disagree with it without a word.

`LockedArtifact::check` in `crates/engine/src/lock.rs` does compare a lock's
interop digest and fails `integrity.mismatch` on a difference, so the gap is the
manifest alone. It is not a phase 8 regression; it has been true since manifests
existed, and it was invisible because nothing in the tree ever wrote a manifest
with a SHA-256 and no BLAKE3 until this phase did.

Fixed: a manifest's `sha256` is compared to the interop digest of the bytes, and
a difference fails `integrity.mismatch` naming both values. This is not new
behavior being invented, it is contracts.md's Identity line -- the interop digest
is recorded for matching publisher claims -- finally having a caller.

**The trust table is amended, and this is the sharper half.** contracts.md
defined `verified` as the content digest matching a digest supplied before the
run, and the content digest is BLAKE3 by definition. Read literally, a manifest
supplying only a SHA-256 can never produce anything better than `tofu`, which
would make every Croissant, Frictionless, BagIt and pooch dataset in the world
permanently first-use no matter how firmly its publisher stated a digest. That is
not a mechanical definition doing its job, it is one algorithm's name having been
written where the argument wanted the idea of a prior claim.

`verified` now reads: a digest computed from the bytes matched a digest of the
same algorithm supplied by the manifest or the lock before this run. A publisher's
SHA-256, checked against the SHA-256 of the bytes this run read, is exactly the
evidence the class describes, and SHA-256 is not the weaker hash. The content
digest remains the cache key and the resume authority and nothing about identity
moves.

The `unverified` and `tofu` definitions do not change. A manifest stating neither
digest is still `tofu`, and a claim stated in an algorithm the manifest model does
not carry is still a refusal at read time rather than a weaker class at run time.

Costs: the trust table now names two algorithms where it named one, and a reader
has to know that either satisfies it. That is one sentence against a class that
would otherwise be unreachable for most published data.

Sources: docs/contracts.md Identity, Trust classes; `crates/cli/src/run.rs`;
`crates/engine/src/lock.rs`.

## Phase 8. How inference walks a listing, an API, and a directory

Question: roadmap.md:141 asks how inference handles listings, APIs, and local
directories.

There is one walk and three things it can be pointed at, because a listing, a
provider API and a directory all answer the same question: which entries are at
or below this reference, and what does the source state about each. `Source::list`
is already that question and already returns `Listing { entries, skipped }`, so
inference asks the seam and never learns what kind of thing answered.

Each entry becomes one artifact. The id is the entry's path relative to the
prefix, which is what makes the manifest reproduce the tree the prefix holds. The
source is the entry's location. The size is what the listing stated and is
omitted when it stated none.

A local directory is walked by the same code through the local adapter rather
than by a second walker. Its entries' locations are their paths relative to the
directory, so the manifest holds no absolute path and is publishable by putting
it beside the data, which is the use features.md names. `resolve_source_path`
already resolves a manifest's relative source against the manifest's own
location, so the round trip is the existing resolution path rather than a new one.

Ordering is by raw path bytes, so two runs over one directory emit byte-identical
manifests and the manifest digest of an unchanged directory does not move.

`listing.skipped` is emitted by inference exactly as it is by a fetch, because a
link outside the prefix is ignored and counted whichever command did the ignoring.

The entry count is bounded by the listing limit already in Limits, and exceeding
it fails `resource.limit` rather than emitting a truncated manifest. A manifest
that silently held some of a dataset would be worse than none.

Sources: docs/roadmap.md; docs/contracts.md Directory listing, Manifest;
`crates/engine/src/seam/source.rs`; `crates/cli/src/run.rs`.

## Phase 8. What inference records when a source states no digest

Question: roadmap.md:141's third decision, and the one contracts.md under Plan makes
sharp: a field is never estimated into a number, and zero is a number.

Options: record no digest and let the manifest force a weak class; synthesize a
digest from what a metadata request states; read the bytes and record what they
hash to.

Chosen: read the bytes and record what they hash to, and record nothing at all
when the bytes were not read.

A digest is never taken from a metadata request. An entity tag is not a digest,
an S3 ETag is the MD5 of one upload part arrangement rather than of the content,
and a length is not evidence about bytes. contracts.md already rules that these
are supporting evidence and never content identity, and writing one into a
`digest` field would be laundering a validator into an identity, which is the
single thing this project exists to refuse.

So inference transfers. `init` over a location with no stated digest fetches each
object through the same path `get` uses, hashes it in the same pass, and records
both the BLAKE3 and the SHA-256 it observed. The objects land in the cache, so the
`get` that follows moves no bytes. features.md's promise that init emits a
manifest with digests filled in is met by having observed them.

What that is worth is stated honestly and is not inflated. The init run itself is
`tofu`: it had no prior digest and it recorded its own first observation, which is
exactly the class definition, and it writes a witness like any other run that
transferred and verified bytes in full. It is not `verified`, because nothing
supplied a digest before it. The manifest it emits is a first-use pin, not a
publisher's attestation, and the difference is the whole reason the class exists.

A later run against that manifest is `verified`, because a digest was supplied
before it. That is not the class being inflated by a trick; it is what pinning is
for, and it is the same mechanism as a lock.

An object inference could not read records no digest and no size, and the run
emits `degrade` naming the artifact and why the bytes were not read. Under
`--offline` a network location is not read at all and the run fails
`policy.offline` before anything is written, because a manifest holding some
digests and not others would be a document whose reader cannot tell which of its
silences mean unknown.

An artifact carrying no digest forces a weak trust class on every run that uses
it, which contracts.md's Manifest rules already state. Nothing here makes that
softer.

Sources: docs/contracts.md Manifest, Trust classes, Verification and repair,
Plan; docs/roadmap.md.

## Phase 8 gate

Question: Does phase 8 meet its exit criterion, and what remains unproven.

The criterion is not roadmap.md's old line. It is this: features.md contains no
unbuilt marker except for distribution, and every unmarked sentence is true of
the binary.

**The answer is yes, with two markers rather than one, and both were ruled
before the phase began.** Distribution is marked and is phase 9's. Protocol
choice is marked and no phase owns it. Everything else features.md says, the
binary does.

**The three decisions the roadmap asked for.**

Which metadata formats map onto the manifest model. Five do and are read:
checksum sidecars, Croissant, Frictionless data packages, pooch registries, and
BagIt payload manifests. Two do not and are refused by name, each with a test
for the refusal. Torrent piece hashes are refused for two independent reasons --
version 1 hashes pieces that span file boundaries under SHA-1, version 2's
per-file root is a merkle root over 16 KiB blocks and is not the SHA-256 of the
file, and either way a torrent names a swarm rather than a location. DVC files
are refused because `outs[].hash` is MD5 or an ETag, and contracts already rules
that an ETag is supporting evidence and never content identity. Every format was
read against its own current specification during this phase rather than from
memory.

How inference handles listings, APIs, and local directories. One walk, three
things it can be pointed at, because all three answer the same question and
`Source::list` already is that question. A local directory goes through the same
code by way of the local adapter, and its manifest holds no absolute path, so it
is publishable by putting it beside the data.

What inference records when a source supplies no digest. It reads the bytes and
records what they hashed to, and records nothing at all when it did not read
them. No digest is ever taken from a metadata request, because an entity tag is
not a digest and writing one into a digest field would launder a validator into
an identity. The init run is therefore `tofu` and says so: it had no prior digest
and recorded its own first observation, which is exactly the class definition.

**The four rulings, carried out.**

The five interface features moved to this phase and are built. SigV4 is built.
Protocol choice stayed unbuilt and its claim was re-verified against ureq 3.4.0's
own documentation rather than carried on phase 7.5's word: the crate exposes no
protocol selection and names HTTP/2 nowhere. Phase 6's performance half is still
open and is carried to the audit, unchanged and unclaimed.

**Exit criteria, with evidence.**

Round-trip from each supported metadata format to a manifest.
`crates/engine/tests/metadata.rs` holds what no single format's tests could: that
no reader ever emits an artifact naming no source, that each refuses a digest it
cannot represent by name, that each is deterministic over one document, and that
each is bounded. The first of those found a real defect -- a Croissant
`FileObject` carrying only a `containedIn` produced an artifact with an empty
source list, which would have resolved to nothing at fetch time.

Inference over a directory reproduces that directory exactly.
`init_over_a_directory_reproduces_that_directory_exactly` in
`crates/cli/tests/inference.rs` walks a three-level tree, emits a manifest, fetches
it back, and compares every file's bytes.

Formats that cannot be represented fail with a named reason.
`every_reader_refuses_a_digest_it_cannot_represent_by_name`, plus the per-format
refusals in each reader's own tests.

A dataset with published metadata is fetched with no hand-written manifest.
`a_metadata_document_resolves_to_the_files_it_describes`.

A local directory becomes a publishable manifest in one command.
`init_writes_the_manifest_to_standard_output_by_default` and the round-trip above.

Adding two real provider adapters changed no engine code. Stated plainly below,
because it is the phase 7 criterion tested a third time.

`doctor` leaves the state it inspects byte-identical.
`crates/cli/tests/diagnose.rs` snapshots every path and every file's bytes before
and after, against a healthy cache, a cache whose format does not match, a cache
directory that is not there, and a read-only one. The property was confirmed
capable of failing by making `doctor` write one extra file.

Identical runs under every display mode produce identical results, exit codes and
event streams. `crates/cli/tests/display.rs`. The streams are compared with one
event excluded, and the exclusion is itself contracted: a mode the terminal
cannot carry is forced, and forcing it silently is forbidden, so the run that
asked for the live view under a pipe emits a `degrade` the other two do not. That
degradation has its own test.

**The live view reads only events, and this is the answer to the thing worth
watching for.** It is not "inspected the code and it only uses events". The
renderer is `crates/view`, a crate whose sole dependency is the engine's event
types. Two tests read the crate's own manifest and its own source:
`the_view_depends_on_the_event_types_and_on_nothing_else` fails if any other
dependency appears, and `the_view_reaches_no_filesystem_no_network_and_no_process`
fails if `std::fs`, `std::net`, `std::process`, `std::env`, a file open, a
`TcpStream` or an `include_str!` appears anywhere in it. Both were confirmed to
fail by adding `fetchloom-cache` as a dependency and a `std::fs::read_to_string`
call, and both named the breach in their failure message. The dependency graph is
what enforces the property; the tests are what make a breach loud.

**Answering the three questions directly.**

*Does features.md still say "not built" anywhere except distribution?* Yes,
once. Protocol choice per host, and no phase owns it, because there is one
protocol to speak and nothing to measure. That was ruled before the phase and the
claim behind it was re-checked against the client's current documentation this
phase rather than carried.

*Are all twelve contracted commands present and acting?* Yes.
`the_surface_holds_every_command_the_contract_names` asserts the twelve are in the
help, and `every_contracted_command_acts_rather_than_being_a_usage_error` asserts
the four new ones act rather than exiting 2.

*Did adding two real provider adapters require any change to the engine?* No.
`git diff --stat -- crates/engine/` across the commit that added
`HuggingFaceSource` and `ZenodoSource` is empty. Both are written entirely against
`fetchloom_engine::seam::source::Source` and reach a run through the `serves`
dispatch phase 7.5 added, so `run.rs` names them only in the list of adapters it
constructs. That is roadmap.md:135 tested a third time and for the first time with
two real adapters rather than one and a stub.

The one engine change this phase makes to `Credential` was made deliberately and
separately, for the second credential shape, which is the widening phase 7 said
would be the real test of whether the seam holds. It did not come from the
adapters; it came from the credential model, and the adapters would have been
identical without it.

**Defects this phase found in work that already existed.**

A plain artifact landed under the basename of whichever source served it, so a
manifest could not name two artifacts whose sources shared one, and no nested
tree could round-trip through a manifest. An artifact now lands under its own
identifier, validated by exactly the rules an archive member passes.

A manifest's `sha256` was compared to nothing. A publisher could state one and
the run would serve bytes that disagreed with it without a word. It has been true
since manifests existed and was invisible because nothing in the tree wrote a
manifest with a SHA-256 and no BLAKE3 until this phase did.

`fetchloom <command> --help` exited 2, because clap's help display was treated as
a usage error.

The `watch` positional argument and the global `--events` flag shared one clap
argument id, so `watch <path>` bound to `--events` and the run wrote its event
stream to the path instead of reading it -- exiting 0 having overwritten the file
it was asked to display. Found by a test asserting that watching a stream that is
not there is a usage error.

The event stream carried no host, so the per-host throughput features.md promises
could not have been shown by a view that reads only events.

No free-space primitive existed on either platform, so contracts' rule that
insufficient space fails before transfer begins had never been implementable, and
`doctor` could not have checked disk.

**What is carried forward.**

Phase 6's performance half. Defaults tie with hand-tuned settings; six of eight
regimes issue no request. That is a benchmark harness problem, it belongs to the
audit, and building features did not close it.

F21 and F26 from the audit, which still need a cloning volume and a
million-object measurement this matrix does not have.

`eight_artifacts_behind_a_charged_wait_finish_in_far_less_than_eight_one_at_a_time`
fails under whole-suite parallelism and passes alone. It is a wall-clock
assertion competing with every other test binary for the machine, which is the
class of measurement standards.md says is a property of the machine as much as
the code. It is reported rather than silenced.

FTP listing was contracted and never built. contracts.md no longer names it,
because a contract that describes something nothing implements is the defect the
last audit existed to find.

Naming a certificate authority or a proxy in configuration. TLS verification is
on and not configurable, which is a stronger promise than the one that was
written, and proxies are read from the standard variables by the client.

Sources: `docs/features.md`; `docs/contracts.md`; `crates/view`;
`crates/engine/src/metadata/`; `crates/sources/src/provider.rs`;
`crates/sources/src/signing.rs`; `crates/cli/tests/diagnose.rs`;
`crates/cli/tests/display.rs`; `crates/cli/tests/inference.rs`.

## Phase 8. The binary grew ten percent, and what the ten percent is

Question: the deterministic gate fails a binary-size regression above five
percent. Phase 8's verify reported 7,587,840 bytes growing to 8,315,904, which is
9.6 percent.

The cause is not a dependency. `git diff 2d5772f..HEAD` over every `Cargo.toml`
adds one crate to the workspace, `fetchloom-view`, which is this repository's own
and holds the live view. `serde_json` and `sha2` appear as new lines in a crate's
manifest and were already workspace dependencies linked into the binary through
the engine, so neither adds a byte that was not there.

The cause is code. Fifteen new production files hold 4,866 lines: five metadata
readers, two provider adapters, SigV4 signing, `doctor`, `why`, inference, the
resolution order, hints, the log level, and the live view. That is the largest
single addition of surface any phase has made, and the phase's whole purpose was
to add exactly this.

So the baseline moves, and the reason is recorded here rather than the number
being quietly re-recorded. That distinction is the one standards.md draws: a
timing measurement that moves while the deterministic metrics hold still is
evidence about the machine and is investigated. A deterministic metric that moves
because the binary genuinely does more is evidence about the change, and the
honest response is to say what the change was. Phase 4 did the same thing when
the binary grew ten percent and nineteen kilobytes of it was the TOML parser.

What would make this the wrong call: growth with no new capability behind it, or
growth from a dependency pulled in for one call site. Neither is the case. If a
later phase finds that a reader or an adapter costs more than it is worth, the
measurement to make is per-feature size, which this harness does not take and
which the audit is the place to add.

Costs: the five-percent gate cannot catch a regression that hides inside a phase
that legitimately grows the binary. That is a real gap and it belongs to the
audit, not to a phase whose job was to add the surface.

Sources: `docs/standards.md` Measure; `docs/benchmarks.md`; the phase 4 record on
the TOML parser.

## Audit. One definition of the `bytes` a run reports

Question: `RunResult.bytes` documents itself as "how many bytes those entries
hold", and four of the five places that build a `RunResult` computed it that way.
The fifth, the single-object path, reported the size of the object the run
fetched, and the manifest path reported the sum of its artifacts' object sizes.

For anything that is not extracted those are the same number, which is why the
disagreement survived. For an extracted archive they are not. `get` on a 105-byte
gzip holding six bytes reported `"entries":1,"bytes":105`, and the reference
documents held two transcripts of the same tree digest reporting 234 bytes in one
place and 233 in the other.

contracts.md does not define the field, so this is not a contract change. It is
the field being made to mean what its own docstring says and what four of its six
call sites already computed: the sum of the sizes of the entries the run reports.
The alternative, defining it as the transferred object size, would have needed a
second field for the tree total and would have made `entries` and `bytes` describe
different things in the same object.

Costs: the number moves for every extracted archive, so every transcript in
`docs/reference/` that showed one was regenerated from a real run. Anything that
had parsed `bytes` as a download size now reads a materialized size instead;
nothing in this repository did.

Proof: `crates/cli/tests/surface.rs` asserts, over three archive shapes and over a
manifest, that the reported count is the total length of the files the run left in
the destination. Both tests fail on the code as it stood, one at 105 against 6 and
one at 20 against a different total.

Sources: `crates/cli/src/run/context.rs` for the field; `crates/cli/src/run/`
`object.rs`, `dataset.rs`, `container.rs` and `local.rs` for the six call sites.

## Audit. Docstrings are removed, and what carries their weight instead

Question: the tree carried 4,889 `///` lines across 225 files, and the lint policy
denied `missing_docs`, `missing_errors_doc` and `missing_panics_doc`, so no public
item could exist without one. The owner asked for them all gone.

Chosen: gone, and the three lints with them. `missing_errors_doc` and
`missing_panics_doc` come from `clippy::pedantic`, which is denied at the
workspace level, so each is now an explicit `allow` in the workspace lint table
rather than a silent omission: the table states the position instead of leaving a
reader to infer it.

What carries the weight instead. standards.md already said that a function needing
a comment to be understood is renamed or split, and that rationale belongs here
rather than beside the code. A docstring that only restated a signature was the
same duplication the No comments rule forbids, kept alive by a lint. Behavior is
stated in contracts.md, reasons are stated here, and both are read once rather
than once per call site. The `//!` module header stays, because it names what a
file is rather than what an item does and it is the map of a tree that is now
thirteen modules in one crate where it used to be one file.

Costs, stated plainly rather than argued away. `cargo doc` now produces a bare
API listing with no prose, so the crates are not readable from rustdoc. A caller
who wants to know which errors a `Result` can carry reads the body or the tests
rather than an `# Errors` section, and nothing mechanical will remind an author to
say so. The decisions.md record at the phase 0 gate, which lists `missing_docs`
denied among the standards the lint policy enforces, describes a policy this
change reverses; it is left as written because it was true when it was written.

What would make this the wrong call: a second person joining who has to learn the
tree from the outside. The answer then is contracts.md and the tests, and if that
is not enough the docstrings come back with the lints that required them.

Sources: `docs/standards.md` under Docstrings and under No comments; the
`[workspace.lints]` table in `Cargo.toml`.

## Audit. What the seams cannot say, and the two answers a run was giving instead

Question: the audit asked of all six seams what each cannot report. Two were
marked worth acting on: `Source::Listing` cannot say a listing was clipped by a
bound, and `Platform` cannot report its own degradation although `preallocate`
promised one.

Neither turned out to need a wider seam. Both turned out to be a run telling the
user something untrue, which is the defect the question was pointing at.

`Platform`. The promise of a `degrade` from `preallocate` lived only in a
docstring, and every docstring in this tree has now been removed. contracts.md
never made that promise and the trait never carried an observer. There is nothing
left to widen and nothing left that lies.

`Source::Listing`. A listing is never silently clipped: past the entry bound the
adapter fails `resource.limit` naming a narrower prefix, and past the size bound
the HTTP client refuses to read further. But the size refusal was classified as
`network.refused`, retryable, layer transfer, telling the user to try the source
again — advice that can never work, because the index will be that size on every
attempt. It is now `resource.limit`, not retryable, naming a narrower prefix, the
same answer the entry bound already gave. So the seam does not need a new field:
what it could not say, it never had to, because the condition is terminal and an
error already carries it.

The same question asked of the document reader found the sharper case.
`read_document` took `manifest_size` bytes with `Read::take`, which truncates in
silence, and handed the prefix to the parser. A 17 MiB Croissant document failed
with `manifest.invalid` and the next action "correct the JSON, because EOF while
parsing a string at line 1 column 16777216" — the run telling the user their
document is malformed when the run is the thing that cut it. It now reads one
byte past the bound and fails `resource.limit` if that byte is there, on the
local path as well as the remote one, because contracts.md states the bound
without qualifying where the document came from.

Costs: a document exactly at the bound now costs one extra byte read. The four
seams the audit did not mark are unchanged and their observations stand as
written.

Proof: `crates/sources/tests/http.rs`
`an_index_larger_than_the_bound_fails_rather_than_listing_what_fitted` and
`crates/cli/tests/inference.rs`
`a_metadata_document_past_the_bound_is_refused_rather_than_read_in_part`, each
run against the code as it stood and each failing there for the reason above.

Sources: audit2.md F81; contracts.md under Limits; `crates/sources/src/http.rs`
`classify`; `crates/cli/src/resolve.rs` `read_document`.

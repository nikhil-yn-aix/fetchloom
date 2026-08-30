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
and on any 3xx, in both its forms, and it replaces the computed backoff rather
than adding to it. A `Retry-After` longer than the sixty second ceiling is not
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

What phase 4 inherits. The speculative ingest write. The four-files-per-object
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
separated by backslashes. Those are still refused, and that is the decision
rather than an oversight. The zip specification says in 4.4.17 that all slashes
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

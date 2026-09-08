# Contributing

## How code is written here

These are not preferences. A change that breaks one of these does not land.

**Contracts come first.** If [docs/contracts.md](docs/contracts.md) states a behavior, implement exactly that. If it is silent on something you need, ask rather than choosing. Never invent a flag, field, event or error kind that is not written down.

**Tests before implementation, and they must fail for the right reason first.** A test that cannot fail is worse than no test. Assert a stated contract, not an implementation detail.

**A test states what it needs from the machine, and never passes quietly without it.** A test that depends on not being root, on a second volume, on a second user, on symbolic links being permitted, on mode bits being honored, or on more than one processor either gets it or declines by name with `fetchloom_faults::decline!("what it needed")`. The declination is printed and recorded, and `cargo xtask verify` reports it beside the lanes it declined, so a suite that proved less than it looks like says so. A lane that promised the environment fails instead, which is what `FETCHLOOM_VERIFY_VOLUMES` already does for the volume matrix.

There is no third state. A test either proves something or says by name what it could not get; it never runs, proves nothing, and is counted as a pass. The shape that used to do exactly that was a loop over an environment-supplied list, which iterates zero times and passes in silence when the list is empty, so ask for the list with `fetchloom_faults::require!` rather than looping over it:

```rust
for volume in fetchloom_faults::require!(volumes(Property::Network), "a network-backed volume") {
```

`require!` yields the list when it holds something and otherwise declines by name and returns. It takes anything that can be absent: a `Vec`, an `Option`, a `bool`. `xtask`'s own test `every_volume_a_test_asks_for_is_declined_by_name_when_the_machine_has_none` walks the test tree and fails on a bare loop over one of those lists, so the silent shape cannot come back.

The gate's summary counts declinations apart from steps, because how many tests proved something and how many merely ran are two numbers and conflating them is the failure this rule exists to prevent.


**No test sleeps.** A wait is on the condition being waited for, with a bound, or it is not a wait: a test that sleeps and then asserts passes on an idle machine and fails on a loaded one, which is the same thing as not testing at all. A wait that cannot be written as a condition is a design problem in the code under test and belongs on the list rather than in a sleep.

**No comments.** If code needs explaining, the names are wrong. The one exception is `// SAFETY:` on an unsafe block, which a lint requires and another lint refuses where it does not belong.

**No version fields, no compatibility code, no second way of doing anything that already exists.**

**Nothing degrades silently.** Every fallback emits a `degrade` event naming what was requested, what was used, and why.

**Both platforms or it is not done.** Windows and Linux.

**Verify, do not trust.** Confirm every library API against current documentation before using it. Do not rely on recall for a signature, a default, or a platform behavior. Do not trust the docs or a previous session's claim over what the code and tests actually do. If a doc contradicts reality, say so rather than coding around it.

**Run everything you claim.** A suite you did not execute has not passed. Paste the real output.

**State uncertainty plainly.** A guess labeled as a guess is useful. A guess presented as fact is a defect.

## What the compiler enforces

Workspace lints, all deny: `unsafe_code`, `unused_crate_dependencies`, clippy `all` and `pedantic`, `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, `allow_attributes`, `allow_attributes_without_reason`, `missing_errors_doc`, `missing_panics_doc`.

Only `platform` lifts `unsafe_code`, per item, with a stated reason.

`allow_attributes` forces `#[expect]` over `#[allow]`, so a stale exception becomes a compile error once it is no longer needed instead of surviving forever. `allow_attributes_without_reason` makes the reason mandatory. Every exception is therefore a written invariant that expires on its own.

`cargo deny` runs as a verify step with an allow list naming every crate permitted in the graph, direct and transitive, each with a reason. A new dependency fails the gate until a human edits that list.

## The gate

The gate is eight lanes. A lane runs where it is native or it does not run.

| Lane | What it proves | Machine |
|---|---|---|
| `checks` | Format, and the dependency graph against the allow list, the advisories, the licences and the registries. | any |
| `windows` | Lint with warnings denied, build, the suite, and a build at the stated MSRV, on `x86_64-pc-windows-msvc`. | Windows x86_64 |
| `windows-arm` | The same on `aarch64-pc-windows-msvc`, run rather than compiled. | Windows aarch64 |
| `linux` | The same on `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl`, both linted, both tested. | Linux x86_64 |
| `linux-arm` | The same pair on `aarch64`. | Linux aarch64 |
| `network` | Real archives fetched from real servers, over every target native to the machine. | any |
| `offline` | A plan and a bundle prepared connected, then applied inside a network namespace holding no interface. | Linux |
| `benchmark` | Measurement, and the five percent comparison gate over the deterministic counters. | Linux x86_64 and Windows x86_64, in CI |

The Linux platform lanes build the volume matrix first, which attaches loop devices and mounts btrfs, xfs, vfat, a small volume, a read-only volume, a second volume and a FUSE mount, so the suite runs against real filesystems rather than only the one the workspace is on. The Windows lanes build their own: a ReFS DevDrive for block cloning and case sensitivity, a 32 MB volume and a 64 MB volume, each a VHD attached by the volume script.

```
cargo xtask verify
```

Runs every lane this machine can prove and declines the rest by name, with a NOT VERIFIED line naming the machine each one needs. A declined lane is never counted as a pass.

```
cargo xtask verify --lane <name>              one lane, and an error if this machine is not the one it needs
cargo xtask verify --lane <name> --provision  install the targets, toolchain, system packages and tools it needs
cargo xtask verify --fast                     format, dependencies, lint and build
cargo xtask verify --install-hook             writes .git/hooks/pre-push running --fast
```

`--provision` with no lane provisions every lane this machine can run.

## What CI runs

`.github/workflows/verify.yml` runs `checks`, `linux`, `linux-arm`, `windows` and `windows-arm` on every push and every pull request, each on a runner that is natively that platform. Nothing is emulated and nothing is containerised. The matrix does not fail fast, because "does this break everywhere or only on Windows" is the question it exists to answer.

`.github/workflows/hosts.yml` runs `network` on all four platforms and `offline` on Linux, on a push to `main` and once a day. They are not on pull requests, because a lane that reaches ftp.gnu.org and files.pythonhosted.org on every push from every branch is impolite to hosts that owe this project nothing, and a third party being down is not a reason to redden a contributor's pull request.

`benchmark` runs on `ubuntu-24.04` and `windows-2025`, and gates the deterministic counters only. Those counters were measured identical across twenty consecutive runs and across two volumes, where wall time on the same regime spreads between 1.6x and 2.5x, so a five percent band is a gate on one and decoration on the other. `MetricKind::Timing` never gates and a test asserts it. The harness passes `--deterministic-io`, because the per host measurement a run records otherwise makes `file-operations` a function of the run rather than of the code.

A baseline is per target and is recorded on the runner that gates it, because a counter can depend on what the volume underneath can do. A lane with no baseline records one and passes, which is what the first run on a new target does.

A workflow step is `cargo xtask verify --lane <name>` and nothing else. No cargo invocation, target triple, test name or lint flag is written in YAML, so rewriting what a lane does never touches a workflow file. `xtask/src/verify.rs` has a test that every lane a workflow names exists and that every lane runs somewhere.

Before opening a pull request, run `cargo xtask verify` and read what it declined. The lanes it declined are the lanes CI will run, and they will run whether or not you looked.

## The three tiers

Every test target is in exactly one tier, and a test in `xtask/src/verify.rs` refuses a target that is in no tier.

| Tier | Budget | Measured | What it may touch |
|---|---|---|---|
| `unit` | milliseconds per target | 8 to 17 s | Memory. No filesystem, no network, no process |
| `integration` | seconds per target | 78 to 157 s | A temporary directory, a local server, a spawned binary |
| `system` | minutes | 741 s and up | The whole pipeline: real archives, real volumes, a thousand kills |

The measured column is two runs of this machine, which is the machine [What this
machine costs](#what-this-machine-costs) describes, and the spread between them
is that machine rather than the suite. The developer loop is the first two, so it
is one to three minutes where the whole suite is fourteen or more.

```
cargo xtask verify --tier unit          milliseconds, and what a change to a rule breaks first
cargo xtask verify --tier integration   seconds
cargo xtask verify --tier system        minutes, and what CI runs on every platform
```

`cargo xtask verify --fast` runs unit and integration after format, dependencies, lint and build, and is what the pre-push hook installs. The platform lanes run the whole workspace, because a lane exists to prove a platform rather than to be quick.

The tier is the cargo test target rather than the file, because a target is what cargo can be told to run. A cheap file inside an expensive target therefore inherits that target's tier.

```
cargo xtask bench              measure
cargo xtask bench --compare    fail on a regression above five percent
cargo xtask surface            report what crosses a crate boundary
```

## Prerequisites

Rust 1.98.0, pinned in `rust-toolchain.toml`, which is the only place a version is named. Nothing else states one.

Everything else a lane needs, `cargo xtask verify --provision` installs: the target triples, the 1.89.0 toolchain the MSRV step builds at, `cargo-deny`, and on Linux the musl C toolchain and the filesystem tools the volume matrix formats with.

The Linux volume matrix needs passwordless `sudo`, because attaching a loop device and mounting a filesystem is root's work. Without it that lane degrades and the suite runs against one volume. The Windows volume script needs an elevated shell, because attaching a VHD is an administrator's work. It needs no hypervisor.

Docker is not needed and is not used. It was, so that a Windows laptop could reach Linux and emulate aarch64; CI runs both natively now and the container plumbing is gone.

## What this machine costs

Worth knowing before you blame the code.

Windows Defender inspects every write. It has been measured at 3.5 GB resident during a run, and the many small files regime has reported cost ratios of 47x, 49x, 57x and 173x across four identical runs. If you are benchmarking, an exclusion on the working tree and the cache changes the numbers substantially. Fetchloom reports the ratio and never works around it.

Run one crate's suite at a time in the foreground if memory is tight. The kill test that usually dies under pressure is the process alive when the machine runs short, not the one that made it short: measured, its whole process tree peaked at 65.7 MB.

## Reporting a change

Keep it short. Use `file:line` rather than pasting code.

```
Done: <one line>
Decisions: <any choice not already in the docs, with the reason>
Tests: <what was added, and the real pass or fail output>
Benchmarks: <numbers, or none needed>
Uncertain: <anything you could not verify>
Blocked: <anything needing a human decision>
```

Do not restate the plan, summarize the docs, or explain at length what you are about to do.

## Done means

Tests written first, passing, asserting a stated contract.

Adversarial cases covered where they apply: failure, corruption, interruption, concurrency, hostile input.

Green on both platforms.

Benchmark numbers for anything performance relevant, with no regime regressed.

Contracts updated in the same change if behavior changed.

No `todo!`, no placeholder, no skipped test, no dead code.

## Where things live

```
crates/engine      identity, canonical forms, policy, the six seams
crates/platform    filesystem, atomic publication, locking, capability detection
crates/cache       the content addressed store
crates/sources     network adapters
crates/archive     enumeration, selection, bounded extraction
crates/view        the live renderer, which reads events and nothing else
crates/faults      fault injection and the hostile corpus
crates/cli         the command surface
xtask              verify, bench, profile, surface
docs/decisions.md  an append only log of why, never authoritative over code
```

Folder names drop the `fetchloom-` prefix their packages carry, the way ripgrep's `crates/cli` is package `grep-cli`. At eight crates the prefix in the path buys nothing.

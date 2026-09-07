# Contributing

## How code is written here

These are not preferences. A change that breaks one of these does not land.

**Contracts come first.** If [docs/contracts.md](docs/contracts.md) states a behavior, implement exactly that. If it is silent on something you need, ask rather than choosing. Never invent a flag, field, event or error kind that is not written down.

**Tests before implementation, and they must fail for the right reason first.** A test that cannot fail is worse than no test. Assert a stated contract, not an implementation detail.

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
| `benchmark` | Measurement, and the five percent comparison gate. | the machine that recorded the baseline |

The Linux platform lanes build the volume matrix first, which attaches loop devices and mounts btrfs, xfs, vfat, a small volume, a read-only volume, a second volume and a FUSE mount, so the suite runs against real filesystems rather than only the one the workspace is on. The Windows lane asks for the same and does not get it: the virtual disks need Hyper-V, which no runner offers, so that lane degrades and says which rows are unproven.

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

`benchmark` runs nowhere in CI. A baseline is recorded on one machine, no baseline exists for a fresh runner, and a run with no baseline to compare against records one and passes. That is a step that runs to look thorough, and it is not run.

A workflow step is `cargo xtask verify --lane <name>` and nothing else. No cargo invocation, target triple, test name or lint flag is written in YAML, so rewriting what a lane does never touches a workflow file. `xtask/src/verify.rs` has a test that every lane a workflow names exists and that every lane but `benchmark` runs somewhere.

Before opening a pull request, run `cargo xtask verify` and read what it declined. The lanes it declined are the lanes CI will run, and they will run whether or not you looked.

```
cargo xtask bench              measure
cargo xtask bench --compare    fail on a regression above five percent
cargo xtask surface            report what crosses a crate boundary
```

## Prerequisites

Rust 1.98.0, pinned in `rust-toolchain.toml`, which is the only place a version is named. Nothing else states one.

Everything else a lane needs, `cargo xtask verify --provision` installs: the target triples, the 1.89.0 toolchain the MSRV step builds at, `cargo-deny`, and on Linux the musl C toolchain and the filesystem tools the volume matrix formats with.

The Linux volume matrix needs passwordless `sudo`, because attaching a loop device and mounting a filesystem is root's work. Without it that lane degrades and the suite runs against one volume. The Windows volume script needs an elevated shell and Hyper-V.

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

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

```
cargo xtask verify
```

Thirteen steps: format, dependencies, lint, build, aarch64 cross compile, the test suite, a build at the stated MSRV, network, benchmarks, and the Linux container lanes. The host lints the Windows target only. Every Linux target is linted, with warnings denied, inside the container that also builds and tests it, because that is where a C toolchain for musl exists.

```
cargo xtask verify --fast        format, deps, lint, build, cross compile
cargo xtask verify --install-hook   writes .git/hooks/pre-push running --fast
```

A fast run prints a NOT VERIFIED line for every lane it declines. It never reports a skipped step as a pass.

```
cargo xtask bench              measure
cargo xtask bench --compare    fail on a regression above five percent
cargo xtask surface            report what crosses a crate boundary
```

## Prerequisites

Rust 1.98.0, pinned in `rust-toolchain.toml`. The stated MSRV is 1.89.0 and verify builds at it, so `rustup toolchain install 1.89.0` if you want that step to run.

`cargo-deny` for the dependencies step.

Docker Desktop for the Linux lanes. Without it those steps fail rather than being skipped, which is deliberate: an unproven platform should look unproven.

## What this machine costs

Worth knowing before you blame the code.

Windows Defender inspects every write. It has been measured at 3.5 GB resident during a run, and the many small files regime has reported cost ratios of 47x, 49x, 57x and 173x across four identical runs. If you are benchmarking, an exclusion on the working tree and the cache changes the numbers substantially. Fetchloom reports the ratio and never works around it.

The Docker VM peaked at 7.3 GB working set on a 16 GB machine during the container lane. That, not the test suite, is what causes memory pressure during a full verify. The kill test that usually dies is the process alive when the machine runs short, not the one that made it short: measured, its whole process tree peaked at 65.7 MB.

Run one crate's suite at a time in the foreground if memory is tight.

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

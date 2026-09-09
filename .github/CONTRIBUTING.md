# contributing

read this first, every session. it is how code is written here and none of it is
optional. a change that breaks one of these does not land.

## rules

contracts come first. if [contracts.md](../docs/contracts.md) states a behavior,
implement exactly that. if it is silent on something you need, ask rather than
choosing. never invent a flag, field, event or error kind that is not written
down.

tests are written before the implementation and must fail for the right reason
first. a test that cannot fail is worse than no test. assert a stated contract,
not an implementation detail.

a test states what it needs from the machine and never passes quietly without
it. a test that depends on not being root, on a second volume, on a second user,
on symbolic links being permitted, on mode bits being honored, or on more than
one processor either gets what it needs or declines by name with
`fetchloom_faults::decline!("what it needed")`. the declination is printed and
recorded, and `cargo xtask verify` reports it beside the lane it declined, so a
suite that proved less than it looks like says so. a lane that promised the
environment fails instead, which is what `FETCHLOOM_VERIFY_VOLUMES` does for the
volume matrix.

there is no third state. a test either proves something or says by name what it
could not get. it never runs, proves nothing, and counts as a pass. the shape
that used to do exactly that was a loop over an environment-supplied list, which
iterates zero times and passes in silence when the list is empty, so ask for the
list rather than looping over it:

```rust
for volume in fetchloom_faults::require!(volumes(Property::Network), "a network-backed volume") {
```

`require!` yields the list when it holds something and otherwise declines by
name and returns. it takes anything that can be absent: a `Vec`, an `Option`, a
`bool`. xtask's own test
`every_volume_a_test_asks_for_is_declined_by_name_when_the_machine_has_none`
walks the test tree and fails on a bare loop over one of those lists, so the
silent shape cannot come back. the gate's summary counts declinations apart from
steps, because how many tests proved something and how many merely ran are two
numbers.

no test sleeps. a wait is on the condition being waited for, with a bound, or it
is not a wait. a test that sleeps and then asserts passes on an idle machine and
fails on a loaded one. a wait that cannot be written as a condition is a design
problem in the code under test.

no comments. if code needs explaining, the names are wrong. the one exception is
`// SAFETY:` on an unsafe block, which one lint requires and another refuses
where it does not belong. that rule is mechanical rather than remembered: the
`checks` lane runs xtask's comment scanner over every `.rs` file in the
workspace and fails on any plain comment, in production, in a test, in a
fixture, in xtask, inside a macro body, after code on the same line, or written
as a block. a `// SAFETY:` line and the lines it wraps onto are the only thing
it allows, and a doc comment is not a comment. it came back twice by good
intentions before the check existed, both times as a paragraph worth keeping,
which is why the answer is never to delete the thought. move it into the name,
into a doc comment, or into contracts.md, then delete the comment.

names carry the documentation, so a name is a defect the way a bug is. files and
modules are named for what they are rather than for what they contain. no
`util`, `helpers`, `common`, `misc`, `manager`, `handler`, `base` or `types`. a
function's name says what it does and a predicate's says what is true. a test's
name says what breaks when it fails. a `mod.rs` declares and re-exports; a
`mod.rs` holding logic is a module nobody named. one vocabulary across all eight
crates: the contracts say *entry* for a thing in a tree, *member* for a thing in
an archive, *object* for bytes in the cache, *artifact* for a thing a manifest
names, and *record* for the per destination file. using one where the docs use
another is wrong even where it compiles.

no version fields, no compatibility code, no second way of doing anything that
already exists.

nothing degrades silently. every fallback emits a `degrade` event naming what
was requested, what was used, and why.

both platforms or it is not done. Windows and Linux.

verify, do not trust. confirm every library API against current documentation
before using it. do not rely on recall for a signature, a default, or a platform
behavior. do not trust the docs or a previous session's claim over what the code
and tests actually do. if a doc contradicts reality, say so rather than coding
around it.

run everything you claim. a suite you did not execute has not passed. paste the
real output.

state uncertainty plainly. a guess labeled as a guess is useful. a guess
presented as fact is a defect.

do not commit or push unless you were asked to.

## what the compiler enforces

workspace lints, all deny: `unsafe_code`, `unused_crate_dependencies`, clippy
`all` and `pedantic`, `unwrap_used`, `expect_used`, `panic`, `todo`,
`unimplemented`, `dbg_macro`, `allow_attributes`,
`allow_attributes_without_reason`, `missing_errors_doc`, `missing_panics_doc`.

only `platform` lifts `unsafe_code`, per item, with a stated reason.

`allow_attributes` forces `#[expect]` over `#[allow]`, so a stale exception
becomes a compile error once it is no longer needed instead of surviving
forever. `allow_attributes_without_reason` makes the reason mandatory. every
exception is a written invariant that expires on its own.

`cargo deny` runs as a verify step against an allow list naming every crate
permitted in the graph, direct and transitive, each with a reason. a new
dependency fails the gate until a human edits that list, and the report that
adds one states why it was taken, what the alternatives were, and what it costs
transitively.

## the gate

eight lanes. a lane runs where it is native or it does not run.

| lane | what it proves | machine |
|---|---|---|
| `checks` | format, every plain comment outside a `SAFETY` block, and the dependency graph against the allow list, the advisories, the licences and the registries | any |
| `windows` | lint with warnings denied, build, the suite, and a build at the stated MSRV, on `x86_64-pc-windows-msvc` | Windows x86_64 |
| `windows-arm` | the same on `aarch64-pc-windows-msvc`, run rather than compiled | Windows aarch64 |
| `linux` | the same on `x86_64-unknown-linux-gnu` and `x86_64-unknown-linux-musl`, both linted, both tested | Linux x86_64 |
| `linux-arm` | the same pair on `aarch64` | Linux aarch64 |
| `network` | real archives fetched from real servers, over every target native to the machine | any |
| `offline` | a plan and a bundle prepared connected, then applied inside a network namespace holding no interface | Linux |
| `benchmark` | measurement, and the comparison gate over the counters | Linux x86_64 and Windows x86_64, in CI |

the Linux platform lanes build the volume matrix first, which attaches loop
devices and mounts btrfs, xfs, vfat, a small volume, a read-only volume, a
second volume and a FUSE mount, so the suite runs against real filesystems
rather than only the one the workspace is on. the Windows lanes build their own:
a ReFS DevDrive for block cloning and case sensitivity, a 32 MB volume and a 64
MB volume, each a VHD attached by the volume script.

```
cargo xtask verify
```

runs every lane this machine can prove and declines the rest by name, with a NOT
VERIFIED line naming the machine each one needs. a declined lane is never
counted as a pass.

```
cargo xtask verify --lane <name>              one lane, and an error if this machine is not the one it needs
cargo xtask verify --lane <name> --provision  install the targets, toolchain, packages and tools it needs
cargo xtask verify --fast                     format, dependencies, lint and build
cargo xtask verify --install-hook             writes .git/hooks/pre-push running --fast
```

`--provision` with no lane provisions every lane this machine can run.

before opening a pull request, run `cargo xtask verify` and read what it
declined. those are the lanes CI will run, and they run whether or not you
looked.

## what CI runs

`workflows/verify.yml` runs `checks`, `linux`, `linux-arm`, `windows` and
`windows-arm` on every push and every pull request, each on a runner that is
natively that platform. nothing is emulated and nothing is containerised. the
matrix does not fail fast, because "does this break everywhere or only on
Windows" is the question it exists to answer.

`workflows/hosts.yml` runs `network` on all four platforms and `offline` on
Linux, on a push to `main` and once a day. they are not on pull requests,
because a lane that reaches ftp.gnu.org and files.pythonhosted.org on every push
from every branch is impolite to hosts that owe this project nothing, and a
third party being down is not a reason to redden a contributor's pull request.

`benchmark` runs on `ubuntu-24.04` and `windows-2025`. it gates two kinds of
metric and decorates the third.

`MetricKind::Deterministic` counters gate at five percent in both directions.
they were measured identical across twenty consecutive runs and across two
volumes, where wall time on the same regime spreads between 1.6x and 2.5x, so
five percent is a gate on one and would be a coin toss on the other.
`MetricKind::Timing` never gates and a test asserts it.

`MetricKind::Bounded` is peak resident set. it gates upward only, at ten percent
above the baseline recorded on the runner that gates it, because it holds within
2.8 percent across runs on one runner, differs 44 percent between the two
targets, and holding less memory is never a regression. that is what makes an
object loaded whole a red step rather than a number nobody reads.

the harness passes `--deterministic-io`, because the per host measurement a run
records otherwise makes `file-operations` a function of the run rather than of
the code. a baseline is per target and is recorded on the runner that gates it,
because a counter can depend on what the volume underneath can do. a lane with
no baseline records one and passes, which is what the first run on a new target
does.

a workflow step is `cargo xtask verify --lane <name>` and nothing else. no cargo
invocation, target triple, test name or lint flag is written in YAML, so
rewriting what a lane does never touches a workflow file. `xtask/src/verify.rs`
has a test that every lane a workflow names exists and that every lane runs
somewhere.

## the three tiers

every test target is in exactly one tier, and a test in `xtask/src/verify.rs`
refuses a target that is in no tier.

| tier | budget | measured | what it may touch |
|---|---|---|---|
| `unit` | milliseconds per target | 8 to 17 s | memory. no filesystem, no network, no process |
| `integration` | seconds per target | 78 to 157 s | a temporary directory, a local server, a spawned binary |
| `system` | minutes | 741 s and up | the whole pipeline: real archives, real volumes, a thousand kills |

the measured column is two runs of the machine [what this machine
costs](#what-this-machine-costs) describes, and the spread between them is that
machine rather than the suite. the developer loop is the first two tiers, so it
is one to three minutes where the whole suite is fourteen or more.

```
cargo xtask verify --tier unit          milliseconds, and what a change to a rule breaks first
cargo xtask verify --tier integration   seconds
cargo xtask verify --tier system        minutes, and what CI runs on every platform
```

`cargo xtask verify --fast` runs unit and integration after format,
dependencies, lint and build, and is what the pre-push hook installs. the
platform lanes run the whole workspace, because a lane exists to prove a
platform rather than to be quick.

the tier is the cargo test target rather than the file, because a target is what
cargo can be told to run. a cheap file inside an expensive target inherits that
target's tier.

every test run passes `--no-fail-fast`, because cargo otherwise stops after the
first failing binary and says nothing about the ones after it. the runner also
prints how many test binaries ran against how many it asked for, and fails the
step when the two differ whatever the exit code said, because a run that is not
the whole suite is not a result. the count comes from `cargo metadata`, and cargo
applies a `--test` filter to every package it has selected rather than to the
`-p` before it, so a target name two packages share runs in both and is counted
in both.

```
cargo xtask bench              measure
cargo xtask bench --compare    fail on a regression outside the band
cargo xtask surface            report what crosses a crate boundary
```

## prerequisites

Rust 1.98.0, pinned in `rust-toolchain.toml`, which is the only place a version
is named. nothing else states one.

everything else a lane needs, `cargo xtask verify --provision` installs: the
target triples, the 1.89.0 toolchain the MSRV step builds at, `cargo-deny`, and
on Linux the musl C toolchain and the filesystem tools the volume matrix formats
with.

the Linux volume matrix needs passwordless `sudo`, because attaching a loop
device and mounting a filesystem is root's work. without it that lane degrades
and the suite runs against one volume. the Windows volume script needs an
elevated shell, because attaching a VHD is an administrator's work. it needs no
hypervisor.

Docker is not needed and is not used. it was, so that a Windows laptop could
reach Linux and emulate aarch64. CI runs both natively now and the container
plumbing is gone.

## what this machine costs

worth knowing before you blame the code.

Windows Defender inspects every write. it has been measured at 3.5 GB resident
during a run, and the many small files regime has reported cost ratios of 47x,
49x, 57x and 173x across four identical runs. if you are benchmarking, an
exclusion on the working tree and the cache changes the numbers substantially.
fetchloom reports the ratio and never works around it.

run one crate's suite at a time in the foreground if memory is tight. the kill
test that usually dies under pressure is the process alive when the machine runs
short, not the one that made it short: measured, its whole process tree peaked
at 65.7 MB.

## reporting a change

keep it short. use `file:line` rather than pasting code.

```
Done: <one line>
Decisions: <any choice not already in the docs, with the reason>
Tests: <what was added, and the real pass or fail output>
Benchmarks: <numbers, or none needed>
Uncertain: <anything you could not verify>
Blocked: <anything needing a human decision>
```

do not restate the plan, summarize the docs, or explain at length what you are
about to do.

## done means

tests written first, passing, asserting a stated contract.

adversarial cases covered where they apply: failure, corruption, interruption,
concurrency, hostile input.

green on both platforms.

benchmark numbers for anything performance relevant, with no regime regressed.

contracts updated in the same change if behavior changed.

no `todo!`, no placeholder, no skipped test, no dead code.

## where things live

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

folder names drop the `fetchloom-` prefix their packages carry, the way
ripgrep's `crates/cli` is package `grep-cli`. at eight crates the prefix in the
path buys nothing.

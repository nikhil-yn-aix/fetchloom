# Contributing

Read `docs/standards.md` first. It is how code is written here and it is not
optional. `docs/contracts.md` states the behavior; if it is silent on something
you need, ask rather than choose. `docs/roadmap.md` says which phase is open.

## Prerequisites

The toolchain is pinned in `rust-toolchain.toml`, so `rustup` installs the right
one for you the first time you build. Everything below is what the verification
matrix needs on top of that.

| Needed for | How to get it | What is unproven without it |
|---|---|---|
| The lint and cross-compile arms | `rustup target add x86_64-unknown-linux-musl aarch64-pc-windows-msvc` | Both non-host targets |
| The dependency gate | `cargo install cargo-deny` | The allow list, the advisories, the licences, the registries |
| The rust-version gate | `rustup toolchain install 1.89.0` | That the workspace builds at the version its manifest states |
| The Linux lanes | Docker Desktop, running, with its Linux engine | Every Linux behavior in this repository |
| ReFS, a small volume, a case-sensitive directory | An elevated PowerShell, and Hyper-V for the virtual disks, which `verify/volumes-windows.ps1` needs | Block cloning, the small-volume rows, the case-sensitive rows |

A missing tool is a skipped step with a `NOT VERIFIED` line naming what the run
did not prove. It is never a silent pass. Run `cargo xtask verify` and read those
lines before believing a green result.

Docker Desktop must be started before `cargo xtask verify`. It is not started for
you, and its Linux engine can take a minute to accept connections after the
window appears. `docker info` answering is the test.

Budget memory for it. `docker info` on this machine reports `MemTotal`
8,128,204,800 bytes to the daemon, and during the container lane the Linux
virtual machine's working set was measured at 7322.1 MB, forty-four percent of a
16 GB machine. Run the container lane with nothing else running. If a run is
killed, note that `docker run` losing its client does not stop the container: the
suite keeps going inside it and produces no verdict, so check `docker ps` before
starting again.

## The gate

`cargo xtask verify` is the whole matrix: format, dependencies, both lint arms,
the host build, the aarch64 Windows compile, the Windows suite, the build at the
stated rust-version, the recorded network subjects, the benchmark comparison, and
the Linux lanes in a container. It takes about half an hour here. Add `--arm` for
the emulated aarch64 Linux pair, which is slower again.

`cargo xtask verify --fast` is the push gate: format, dependencies, both lint
arms, the build, and the aarch64 Windows compile. Sixteen seconds when the tree
is fully warm, and about eighty when the lint arms have work to do. It names
every lane it declined in its own output, with a `NOT VERIFIED` line each.

`cargo xtask verify --install-hook` writes `.git/hooks/pre-push` running
`cargo xtask verify --fast`. Running it again when the hook is already there
changes nothing. If a different `pre-push` hook exists it refuses and writes
nothing, so read that file and decide yourself.

The hook does not run the suite. `cargo xtask verify` does, and that is what a
change is finished against. The hook exists to make the cheap and certain
failures impossible to push, in a time short enough that nobody reaches for
`--no-verify`.

## Running the suites here

Run one crate at a time, in the foreground:

```
cargo test -p fetchloom-cache -- --test-threads=2
```

Whole-workspace runs in the background have been killed for low memory on this
machine. The cause is not any one test. `crates/cache/tests/concurrency.rs`
spawns eight children over a thousand kills and its whole process tree peaks at
65.7 MB, measured; the pressure is ambient, and a long test is simply the process
that is alive when the machine runs short. Running in the foreground, one crate
at a time, keeps the number of test binaries alive at once small enough that it
does not happen.

## What the machine does to the numbers

Every timing number in this repository is a property of the machine that produced
it. On the Windows development machine these two effects are large enough to
swamp any change to the code.

Real-time antivirus scans every file the suites write. During one suite run
`MsMpEng.exe` grew from 3.0 GB to 5.5 GB on a machine with 15.6 GB of memory. In
that state the many-small-files regime reported small writes costing 47, 49, 57
and 173 times one large write across four runs of identical code. Add exclusions
for the workspace directory and for `%TEMP%` before any timing number measured
here means anything. Without them the numbers measure the scanner.

Memory pressure decides how the suites are run, which is the section above.

## How sessions are run

Fetchloom is built by separate sessions that share no memory. The rules below
exist to remove drift, assumptions, and unverified claims.

### Unit of work

A phase is too large for one session. A phase is split into three to five slices.
One session does one slice.

A slice is independently testable, independently mergeable, and small enough that
its result can be judged in minutes. If a slice cannot be described in one
paragraph, it is two slices.

### Session lifetime

Sessions reset at phase boundaries, not between slices. Within a phase, a session
that already holds the context keeps working. Across a phase boundary every
session is closed, because a new phase reads different contracts and inherits
nothing but the documents.

Decision records are how context crosses a boundary. Anything a session learns
that must survive is written to `docs/decisions.md` before it closes.

### Roles

Three roles, never combined in one session.

Researcher settles open questions and writes a decision record. Produces no code.

Builder implements one slice against settled decisions. Never decides anything
the docs are silent about; it stops and asks instead.

Judge reviews a finished slice against the contracts. Never wrote the code it
reviews, because a session cannot see its own assumptions.

### Loop

Research the open questions of the phase. Write decisions. Human approves.

For each slice: build, then judge, then fix or accept.

At the end of the phase, judge against the roadmap exit criteria. A phase is not
partially carried forward.

### Parallelism

Builders run in parallel only when they own disjoint crates. Two sessions inside
one crate produce merge conflicts and, worse, two different answers to the same
unwritten question.

Parallel builders require a foundation session first. Every seam trait, domain
type, and signature they compile against must already exist, or they will each
invent their own. That foundation is one session and it is never parallelized.

A judge runs alone, after the work it judges.

### Decision records

Every choice not already written in the docs is recorded in `docs/decisions.md`
before code depends on it. A record states what had to be decided, what was
considered, what was chosen, the evidence measured or cited, and what the choice
makes harder.

If a decision changes a contract, contracts.md is updated in the same change. The
decision record is the reason; the contract is the rule.

### Token discipline

The standing rules live in `CLAUDE.md` and are not repeated in prompts. A prompt
carries only what is specific to its slice.

Prompts name the sections to read, not whole documents.

Reports use the format in `CLAUDE.md`: short, `file:line` references, no pasted
code, no restating the plan.

A session that has drifted is restarted with a fresh prompt rather than argued
with.

### Sizing

A session takes the largest coherent batch it can finish well. Small batches
waste sessions and force the same context to be rebuilt repeatedly. A batch is
too large only when its work spans seams that do not depend on each other, or
when its result cannot be judged against a single set of contracts.

Research and build never share a session. That split is what removes assumptions,
and merging it costs more than it saves.

Research runs immediately before the build that depends on it, so decisions are
made once, in context, and never made twice.

### Prompts

A researcher prompt names the sections to read and the questions to settle, each
with what evidence would settle it, and asks for one decision record per
question. It forbids editing contracts.md and requires stopping when a question
cannot be settled with evidence.

A builder prompt names the slice, the sections and decision records to read, what
exists when it is done, the tests and adversarial cases required, the objective
criteria for done, and the files it may not touch. It requires every library API
to be verified against current documentation, and every test run to be pasted.

A judge prompt names the slice and the sections, and asks nine questions, each
answered with evidence from the code. Does the behavior match the contract
exactly, including error kinds, event names, exit codes and limits. Was each test
written to fail first, and does it assert a contract rather than an
implementation detail. Can any test pass while the feature is broken. Are the
adversarial cases real: failure, corruption, interruption, concurrency, hostile
input. Did it run green on both platforms, shown rather than claimed. Is there
any comment, version field, compatibility path, second way of doing an existing
thing, dead code, or placeholder. Any silent fallback without a degrade event.
Any new dependency without a stated reason. Any benchmark regime regressed or
missing. It outputs defects with `file:line` and fixes nothing.

## Before you push

`cargo xtask verify` green, with every `NOT VERIFIED` line read. Contracts
updated in the same change if behavior changed. A decision record for anything
the documents did not already answer. Benchmark numbers for anything
performance-relevant.

# Plan

The order Part B executes. Ranked by what unblocks what, not by severity.

Every entry states what changes, what proves it changed, what could break, and
whether it can run beside another entry. An entry marked **parallel with** may
run in a second session against a disjoint set of files; everything else runs
alone.

Three rules apply to every entry without being repeated in it. A test is written
first and fails for the right reason. Anything performance-relevant carries a
benchmark on a named regime. Both platforms, or it is not done.

Findings are numbered as in audit2.md, which continues audit.md's numbering.

---

## Stage 1. The instrument

Nothing downstream is provable until the thing that measures is honest. Every
entry here changes what a number means, so all of them land before any entry that
claims a number moved.

### 1.1 Make the work counters mean what contracts.md says they mean

**Changes.** Delete the double count at `crates/cache/src/ingest.rs:110`. Count
the read that `copy_bytes` performs (`crates/platform/src/lib.rs:117`) and the
read at `crates/cache/src/ingest.rs:96`. Closes F30.

**Proves it.** A test that runs a local `get` of an object above the pack
threshold and asserts `bytes_written` equals twice the object length and
`bytes_read` equals twice it, rather than asserting a recorded constant. The test
must fail before the change with the numbers in audit2.md's measurement table.
Independently, the Linux `du -sb` check: destination plus cache equals reported
bytes written, within the outboard tree's size.

**Could break.** Every deterministic baseline number for `one-large-file`,
`cold-cache`, `many-small-files` and `many-hosts` moves. The gate will fail, and
must, and the baseline is re-recorded once with each moved number attributed to
this entry by name.

**Alone.** Everything in stage 1 depends on it.

### 1.2 Make the deterministic gate two-sided

**Changes.** `xtask/src/bench.rs:278` fails only when a metric grew. Make it fail
when a metric moves in either direction by more than the band, and fail when the
baseline carries a deterministic metric the run did not produce. Closes F51.

**Proves it.** Two tests against `compare`: one where a metric halves, one where
a metric is absent from the run. Both must fail the gate. Then run the real
harness and confirm it still passes at the baseline 1.1 recorded.

**Could break.** A legitimate improvement now fails the gate, which is the point:
it fails, a human looks, and the baseline is re-recorded with the reason. That is
the workflow standards.md already describes for a deterministic metric that
moves.

**Parallel with 1.3.**

### 1.3 Assert the counters against the platform, mechanically

**Changes.** A test that drives a real run and compares each of the four counters
against what the platform actually performed, rather than against a recorded
number. On Linux this is disk occupancy; on Windows it is
`GetProcessIoCounters`, which needs no elevation. Closes the class that F5, F6
and F30 are three instances of.

**Proves it.** Reintroducing 1.1's deleted line makes it fail.

**Could break.** Nothing. This is new coverage.

**Parallel with 1.2.**

### 1.4 Decide the timing gate, and make the documents and the code agree

**Changes.** standards.md describes a timing gate under `cargo xtask verify`
against a same-machine baseline at a wider band. `xtask/src/bench.rs:265` skips
timing on every path and the committed baseline records none. One of the two is
wrong. F50.

The band is the thing standards.md does not state, and this machine has measured
the same regime at 7,150 ms and 28,801 ms, which is a factor of four. **This
needs a human decision and Part B stops for it rather than choosing:** either
build the gate and write a band into standards.md, or delete the paragraph and
say plainly that timing is published and never gated, which is what the code and
`xtask/src/main.rs:241` already say.

**Proves it.** Whichever is chosen, standards.md and the code say the same thing
afterwards, and a test or the absence of one matches.

**Alone. Blocking.** Ask before starting stage 1.

### 1.5 Give the no-op regime the run it is defined as

**Changes.** `xtask/src/bench.rs:188` runs `explain`. standards.md defines the
regime as a locked run against an unchanged destination. Phase 4 shipped the lock
four phases ago and decisions.md:993 said the regime would join it then. Make it
that run. Closes F52.

Also: the harness rebuilds the binary immediately before measuring startup, so
the regime measures an on-access scanner's first touch of a file written seconds
earlier, which is what the 9 ms against 131.7 ms spread is. Measure a binary that
has already been touched.

**Proves it.** The regime issues zero requests, reports status `unchanged`, and
writes nothing, asserted on those rather than on a duration.

**Could break.** The no-op baseline is a different number afterwards, attributed
to this entry.

**Parallel with 1.6.**

### 1.6 Record peak resident memory

**Changes.** Add peak resident set as a metric on every regime.
decisions.md:668 promised it at phase 0 and it arrived nowhere. On Linux read it
from the child's rusage; on Windows from `GetProcessMemoryInfo`, which needs no
elevation. Gate it as deterministic or record it as timing -- it is neither
exactly, and the honest answer is to record it and gate it at a wide band, with
the reason written down. Closes F53.

**Proves it.** The regimes report 7 to 9 MB, matching the numbers in audit2.md,
and a deliberately unbounded read makes the number move.

**Could break.** Nothing. New metric, so 1.2's absent-metric rule needs the
baseline recorded first.

**Parallel with 1.5.**

### 1.7 Give the fault library a delay, and build the two regimes standards.md names

**Changes.** `crates/faults/src/schedule.rs` can schedule a failure and nothing
else, which is why standards.md:137 says a slow disk needs "a fault injector this
build does not have". It has one; what it lacks is a delay. Add
`Faults::delay(operation, duration)` mirroring the fault server's `Latency`, and
`Operation::FreeSpace` while there (F54).

Then build the two regimes standards.md names and the harness does not have: a
slow disk, through the delaying platform, and a constrained network, through the
wait the fault server already charges.

**Proves it.** Each regime fails if its injector is removed, the way `many-hosts`
already refuses to pass unless the controller decided something.

**Could break.** Nothing existing. `Faults` gains a method; `FaultyPlatform`
gains a wait.

**Alone.** It touches the injector every other regime may come to use.

### 1.8 Split the many-hosts regime so each half measures one thing

**Changes.** Half its servers answer with a rate limit, so a host asking to be
left alone runs at one transfer in flight whatever the ceiling says, which is why
a per-host ceiling of one differs from four by fourteen percent rather than by
the factor the mechanism gives. Split it: one regime with latency and no rate
limit, which measures concurrency, and one with a rate limit, which measures
backoff. Keep the `decided()` check on both.

**Proves it.** On the concurrency regime a ceiling of one and a ceiling of four
separate by more than the noise of the defaults column. If they do not, that is
the answer and it is recorded as the answer.

**Could break.** `docs/benchmarks.md` gains a row and loses one.

**Alone.**

### 1.9 Re-run phase 6's matrix on the corrected instrument, and rule

**Changes.** Nothing. This is the measurement phase 6's exit criterion has never
had an instrument for: defaults against hand-tuned fixed settings across the
matrix, now including a regime where concurrency is the only variable.

**Proves it.** Either defaults beat fixed settings and phase 6's performance half
closes, or they do not and the record says so for a fourth time -- but this time
against a matrix that could have shown it.

**Alone.** Quiet machine. Nothing else runs.

---

## Stage 2. Correctness

Ordered by severity within what stage 1 unblocks. Every entry here is a defect a
user meets.

### 2.1 Make `--offline` mean offline

**Changes.** `crates/cli/src/run.rs:1353` asks whether the reference contains
`://`. Ask the adapter registry instead, which already answers `serves`, and
enforce again at the point a request is issued in `HttpSource` so that a
reference form added later cannot bypass it. F29.

**Proves it.** A test per reference shape -- `https:`, `file:`, `hf:`, `zenodo:`,
`croissant:`, a bare name against a configured HTTPS base -- asserting
`policy.offline`, exit 40, and `requests == 0`. The bare-name and provider cases
must fail before the change. A second test asserts that an offline run of every
shape issues zero requests, which is the property rather than the message.

**Could break.** A reference that is remote but was previously resolved locally
would now be refused offline. That is the contract.

**Alone.** It touches the composition root and the HTTP source.

### 2.2 Stop the raw location reaching the message

**Changes.** `crates/sources/src/http.rs:567` interpolates `ureq::Error`'s
`Display`, which embeds the URI for several variants. Stop interpolating a third
party's `Display` into a message this project is responsible for redacting. F32.
While there, `crates/sources/src/http.rs:619` still matches on substrings of an
`io::Error` message, which the phase 2 record says was removed.

**Proves it.** A test that fixes a sentinel secret inside a syntactically invalid
query string, drives it to `BadUri`, and searches stderr, the JSON result and the
event stream for the sentinel -- the shape phase 7's test uses, extended to the
branch it does not reach.

**Could break.** Error messages lose detail. That is the trade the redaction rule
already makes everywhere else.

**Parallel with 2.3.**

### 2.3 Build the SigV4 canonical request to specification

**Changes.** Percent-encode the canonical URI and each query key and value, and
sort the query pairs, in `crates/sources/src/http.rs:174` and
`crates/sources/src/signing.rs:127`. F33.

**Proves it.** A test with at least two query parameters given in reverse order
and a path carrying a byte outside the unreserved set, asserting the signature
against a hand-computed value. It fails before the change.

**Could break.** Nothing signed today carries a query string, so no shipped
behavior changes. The test is what makes that true tomorrow.

**Parallel with 2.2.**

### 2.4 Refuse a link that points outside the prefix, and count it

**Changes.** `crates/sources/src/index.rs:128` keeps a root-relative or
protocol-relative link and fabricates its location. Refuse and count anything
that still begins `/` after the strip attempts. F34.

**Proves it.** A test with `href="/other/thing"` under a `/set/` prefix and one
with `//elsewhere/x`, asserting both are counted as skipped and neither becomes
an entry. Both fail before the change.

**Could break.** A listing that previously produced entries now produces fewer.
Those entries named locations nothing serves.

**Parallel with 2.5.**

### 2.5 Join a container location to an entry path with a separator

**Changes.** `zenodo:10.5281/zenodo.1234567` -- contracts.md's own example --
yields `zenodo:10.5281/zenodo.1234567article.pdf`. F70. The same missing
separator is what F34 fabricates with.

**Proves it.** A test that resolves each provider container form against the
fault server and asserts the entry locations, and one that asserts every example
in contracts.md's reference grammar table is recognized. The second is the test
that should have existed since phase 8.

**Could break.** Provider resolution, which has no other coverage at this level.

**Parallel with 2.4.**

### 2.6 Round `ByteCount` and copy the remainder

**Changes.** `crates/platform/src/windows/ffi.rs:241` passes the object's exact
length where the specification requires a multiple of the cluster size. Clone the
aligned prefix and copy what is left, which is under one cluster. F31. Also fix
the probe at `crates/platform/src/linux/probe.rs:177`, which writes exactly 4,096
bytes and so answers for the one length that would have worked anyway.

**Proves it.** This is the entry that cannot be proven on this machine. It needs a
ReFS volume or a Dev Drive, which needs an elevated shell and Hyper-V. **Part B
should attempt the change, assert what it can -- that the aligned prefix and the
remainder cover the object exactly, tested against a byte copy -- and record
plainly that the clone itself is still unproven.** The alternative is to leave a
capability broken because the machine that would prove it is missing, which is
what eight phases already did.

**Could break.** A volume that does clone would start cloning, which changes
`bytes_written` on that volume and nothing else.

**Alone.**

### 2.7 Give Windows the scanner answer the contract defines

**Changes.** `crates/platform/src/windows/probe.rs:198` returns `Unknown`
whenever enumeration fails, whatever the ratio. Mirror the Linux fallback. F39.

**Proves it.** A test that a volume with a low ratio and no enumeration answers
`absent` and emits no degradation. It fails before the change on any unelevated
Windows machine, which is every one of them.

**Parallel with 2.8.**

### 2.8 Stop cloning the packed index per lookup

**Changes.** `crates/cache/src/pack.rs:199` returns the index by value. Hold it
behind an `Arc` and clone the handle, or do the lookup under the lock. F38.

**Proves it.** A benchmark: `cache status` against caches of 2,000 and 4,000
packed objects, asserting the cost ratio is near 2 rather than near 4. The
measurement in audit2.md is the before. This is also the entry that closes F26,
so the record says so.

**Could break.** `remember_packed` and `forget_pack` mutate through the same
lock, so a clone-on-write is needed where a clone-on-read is removed.

**Parallel with 2.7.**

### 2.9 One decider, and a walk that can catch a second

**Changes.** Route `crates/platform/src/linux/mod.rs:27`,
`crates/cli/src/run.rs:180`, `crates/cli/src/materialize.rs:98` and the inline
constructions in `crates/platform/src/windows/mod.rs` and `crates/cache/` through
the one decider. Give the destination a write-side counterpart so a full disk
stops telling the user to make the path readable. Then widen
`crates/engine/tests/filesystem_failure.rs:78` to every crate and to the
conversion rather than the declaration line. F37, F85.

**Proves it.** The widened walk fails at `e5d6f09` and passes after. That
ordering is the point: the walk is the part that was believed to be holding.

**Could break.** Exit codes change where a kind was hardcoded wrongly. Each
change is a contract row and is asserted.

**Alone.**

### 2.10 Wire the color subsystem and the two dead config keys

**Changes.** Call `crates/cli/src/terminal.rs:122` from the composition root and
thread the answer into output. Resolve `color` and `hints` from the configuration
file into `Settings` and report both in `explain`. F35, F36. contracts.md states
both behaviors, so this is implementing the contract rather than deciding
anything.

**Proves it.** `--color never` and `NO_COLOR` strip and `--color always` does
not, asserted on the bytes. `hints = false` suppresses a hint the flag would also
suppress. `explain` reports sixteen settings. `every_key_the_contract_names_is_read`
asserts the value takes effect rather than that the file parses.

**Could break.** Output gains escape sequences where a test compares exact
bytes. The display-mode equivalence tests are the ones to watch.

**Alone.**

### 2.11 The remainder, each small and independent

Each of these is a few lines with a test, and any two can run beside each other.

- Delete `crates/cli/src/planning.rs:183` and call `Host::of_location`. Test an
  IPv6 literal through `plan`. F41.
- Set `max_idle_age` to the sixty seconds decisions.md:2172 chose. F42.
- Read the zip general-purpose bit flag and skip the size comparison when bit 3
  is set; detect the ZIP64 sentinels and refuse by name. Add a corpus entry for
  each. F40.
- Rewrite the Amazon S3 help record to describe the signing credential the build
  actually resolves. F44.
- Pass `--threads` through at `crates/cli/src/main.rs:514` and
  `crates/cli/src/run.rs:959`. F88.
- Size `crates/cache/src/storage.rs:318`'s buffer like every other streaming path,
  and give the tree one declaration of that constant rather than three. F89.
- Replace two unsafe blocks with `std::os::windows::fs::symlink_file`/`symlink_dir`
  and `File::sync_all`. F90.
- Delete the orphaned docstring and `#[expect]` at `crates/cli/src/run.rs:1497`
  and the two dead lint allowances. Make the lint step fail on a warning. F45.
- `git rm` the two stray files at the repository root. F46.
- Give the archive crate one bomb guard instead of three, and check the expansion
  ratio on the paths that move bytes. F49.
- Move the duplicated hex helpers into `metadata/mod.rs`, reconcile the two
  disagreeing copies of `algorithm_name_for_length`, delete one `rung_of`, and
  hoist the listing bound. F79.
- Turn `crates/cli/src/policy.rs:134`'s `eprintln!` into an event or delete it,
  and stop `crates/cli/src/inference.rs:159` reusing `resolve.alias` for a count.
  F83.
- Query the path length per volume, or amend the contract to say it is a
  platform constant. F84. **This one is a contract question; ask.**

---

## Stage 3. Tests to FIRST

After stage 2, because the suite is the safety net for stage 4 and stage 2 closes
the holes in it.

### 3.1 Make the suite pass under parallel load

**Changes.** `crates/cli/tests/concurrent.rs:141,154` are wall-clock ratio
assertions that fail under whole-suite load and pass alone, measured. F62. The
options are a serialized lane, or a gate that fires only under
`cargo xtask verify` the way timing metrics are supposed to, or an assertion on
something other than a clock -- the number of transfers observed in flight by the
fault server, which the server already records and which is the property the test
is actually about.

The third is the right answer and should be taken: a test that asserts what it
means does not need a quiet machine.

**Proves it.** The whole suite passes, twice, on a loaded machine.

**Alone.**

### 3.2 Fix the tests that cannot fail for what they claim

**Changes.** `every_error_kind_is_reachable_and_carries_the_required_fields`
(F56) asserts a builder round-trip under a name about reachability. Either give
it a producing call site per kind or rename it to what it tests and add a
separate reachability test. `every_key_the_contract_names_is_read` (F57) is
handled by 2.10. The decider walk (F58) is handled by 2.9.

Add the two missing producing tests, for `network.timeout` and `cache.locked`
(F60), and a test that the union of events a real run emits across the suite
covers all thirty-four contracted names (F59).

**Proves it.** Each new test fails when the production path it names is removed.

**Parallel with 3.3.**

### 3.3 One test harness, and an environment every test controls

**Changes.** Nine names for one idea across twenty-five files (F67), sixteen of
which clear no environment variable and twenty-three of which never pass
`--no-config` (F61). Put one harness in `crates/cli/tests/support/`, have it
clear every `FETCHLOOM_*` variable and disable configuration discovery by
default, and move every file onto it.

**Proves it.** The suite passes with `FETCHLOOM_LOG=debug`,
`FETCHLOOM_OFFLINE=1`, `FETCHLOOM_CONCURRENCY=1` and a user configuration file
present. It does not today, and that is the failing-first evidence.

**Could break.** A test that was relying on inherited environment. Finding those
is the point.

**Parallel with 3.2.**

### 3.4 The smaller test findings

- Replace the resolver-reaching test with the fault server (F64).
- Replace the poll-with-sleep loops in `cancel.rs` with a condition (F66).
- Point `packed_digests_are_their_bytes` at the cache's own reader instead of a
  hardcoded offset (F68).
- Forbid the bare path segments in the view isolation test, or parse (F48).
- Decide what to do about `xtask/src/network.rs` reaching the internet inside
  `cargo xtask verify` (F65). It fails safe today. **This is a judgment call about
  whether a verify run should be reproducible from source; ask.**

**Parallel with anything.**

---

## Stage 4. Restructure

After stage 3, because the safety net has to hold first. In its own commits,
separate from behavior, so a bisect can tell them apart.

### 4.1 Split `run.rs`

**Changes.** Modules under `crates/cli/src/run/` by what is materialized: local,
remote, container, cached, archive, dataset, with verification and selection
beside them. No new crate, no new dependency edge. F77.

**Proves it.** The whole suite, unchanged, passes. The deterministic counters do
not move. That is the entire proof and it is sufficient: a pure move changes no
number.

**Alone.**

### 4.2 Split `main.rs`

**Changes.** Move reference resolution into `resolve.rs`, and each command body
next to the module owning that command's logic. What remains is dispatch and the
cache and destination bootstrap, which is what its own docstring claims. F77.

**Proves it.** As 4.1.

**Alone. After 4.1.**

### 4.3 The remaining splits

`crates/faults/src/archives.rs` into a tar writer, a zip writer, a DEFLATE
encoder and the corpus catalog (F78). `crates/engine/src/transfer.rs`'s retry
group and copy group into their own modules (F80).
`crates/platform/src/windows/ffi.rs` by Win32 family. Each is a move.

**Parallel with each other, after 4.2.**

### 4.4 The seam gaps, or a record saying why not

**Changes.** F81 lists what each of the six seams cannot say. Two are worth
acting on now because something already needs them: `Source::Listing` cannot say
it was clipped by a bound, and `Platform` cannot report its own degradation
although `preallocate`'s docstring promises one. The rest are observations.

**Widening a seam requires the same justification as changing a contract**, so
each is a decision record before it is code. **Ask before widening.**

---

## Stage 5. Documents

Two sessions may run in parallel here. contracts.md and decisions.md are not
split; one session owns both.

### 5.1 contracts.md and decisions.md

The five contract statements the code disproves: the log-line sentence (F71), the
`--color` sentence if 2.10 does not close it, the Limits "All configurable" line,
the path-length row (F84), and whatever 1.4 rules about timing. Plus the twelve
stale line references (F73) and decisions.md's two stale code references.

### 5.2 features.md, roadmap.md, standards.md, the reference, audit.md

The five false sentences in features.md (F69). roadmap.md's missing phase 7.5 and
its "seven regimes" (F72). standards.md's claim that the build has no fault
injector (F54). The six stale reference transcripts and the `hints` sentence
(F74). build-protocol.md's three platforms and continuous integration (F75).
docs/README.md's missing entries. audit.md's stale `--no-cache` note (F76), and
its F5, F6, F21 and F26 dispositions, which this pass reopens or settles.

### 5.3 The thing that would stop this recurring

Three audits have now found false sentences in features.md by hand, and each
recorded that nothing enforces the convention mechanically. A check that every
unmarked sentence naming a flag, a key, a command or an event corresponds to
something the binary has is not a full solution and would have caught F35, F36
and F69. Worth a decision record either way, because "the next audit should look
again" has now been written three times.

---

## Stage 6. Close

`cargo xtask verify` in full, thirteen steps, real output pasted.

Re-record the baseline, and separate the three claims the project has conflated
before: numbers that moved because code got faster, numbers that moved because
the measurement got honest, and numbers that are new. Stage 1.1 alone moves four
regimes for the second reason.

Amend audit2.md's disposition table. Amend audit.md where this pass closes or
reopens something it recorded. Write the record in decisions.md in the style of
the phase gates.

Then answer, directly: is every sentence in features.md still true; does any doc
disagree with the code; what was deleted and what proves nothing needed it; which
findings are deferred and what would clear each.

---

## What Part B should ask before starting

Four decisions are not Part B's to make, and each blocks or reshapes an entry.

1. **The timing gate** (1.4). Build it with a stated band, or delete the paragraph
   from standards.md. The code already says the second.
2. **Path length** (2.11). Query it per volume as contracts.md says, or amend the
   row to say it is a platform constant.
3. **`xtask/src/network.rs`** (3.4). Whether a `cargo xtask verify` run should be
   reproducible from source alone, given the lane fails safe today.
4. **Seam widening** (4.4). Two seams cannot say something a caller needs. Each
   widening is a contract-grade decision.

Two more are worth flagging though Part B can proceed without an answer:
`resident_memory` promises more than either platform cheaply enforces, and F31's
clone fix cannot be proven on any machine in this matrix.

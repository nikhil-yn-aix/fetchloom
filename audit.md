# Audit

Written at `b7a3f5b`, against the tree phase 5 closed at `730372d`. Everything
here was run on this machine unless it says otherwise.

No finding in this document was fixed when it was written. Three changes were
made alongside it and none of them touches behavior: macOS was removed end to
end at `9dc18dd`, docstrings that had grown into commentary were cut back at
`83a9714`, and `docs/reference/` was written at `b7a3f5b`. Line numbers below
are as of `b7a3f5b` and every one was checked against the file after those
changes.

This document has since been amended in place rather than replaced, because it
is the measurement the work was judged against. Every finding now carries a
**Disposition** line saying whether it was fixed and in which commit, or
deferred and what would clear it. The table of all twenty-eight is at the end,
under Dispositions. Nothing above a disposition line has been edited: where a
finding says something that is no longer true of the code, that is the point of
keeping it.

## Verdict

The project is not in the shape its docs claim, and the gap is narrower and
sharper than that sentence sounds. The hard parts are real: content addressing,
the store's crash safety, the resume ladder, localized repair, the canonical
form, and the offline plan-and-apply flow all work, are tested against
adversarial input, and produce identical deterministic counters on Windows and
Linux. What has quietly not kept up is the layer between those parts and a
person: four shipped behaviors are unusable through the command that exposes
them, one contracted seam is called by nothing, ten of the thirty-four
contracted events are emitted by no code, and the deterministic metric the whole
benchmark gate rests on reads 338 on one path and 2 on another for the same tree
digest.

Three things most need attention.

**Nothing tests the command surface against the contracts tables.** Every one of
the four highest-severity findings is a shipped, contracted behavior that fails
the first time a person tries it, and every one is invisible to the suite
because the suite exercises the layer below. A bare `.tar` cannot be fetched
(F1), `--layout flatten:n` fails on every archive that declares its directories
(F2), a plain local file writes no lock so `plan`, `apply` and `--locked` cannot
touch it (F3), and `cache clear` cannot clear a cache whose format does not
match, which is the one command its own error message names as the fix (F4).
548 tests pass. None of them fetches a bare tar, flattens a real archive, locks
a single file, or clears a mismatched cache.

**The work counters are not trustworthy, and phase 6 is about to gate on them.**
`file_operations` reads 5,138 on one path and 1 on another for the same tree
(F5); `bytes_written` reads 0 where a byte copy happened (F6). The
many-small-files regime, deferred three times as the project's worst number,
uses a corpus that is three-quarters duplicates and under-reports its own case
by 3.4x in time and 4.0x in operations (F7). Phase 6 turns measurement into
defaults. It should not start until the measurements are of the thing they name.

**The event stream is the weakest delivered surface, and phase 9 is contracted
to build the live view on it alone.** Ten payloads are constructed only in a test
(F8). Every `*.end` event but one carries `duration_ms: 0` (F9). A cold fetch of
an archive emits six events end to end and names neither the transfer, the
extraction, nor the verification.

The good news is that almost all of it is cheap. Most of these are a handful of
lines each, and the expensive one -- the many-small-files cost -- is now measured
rather than argued about, and the measurement says which of the two candidate
answers is the right one.

---

## Findings

Severity is what happens to a user, not how hard it is to fix.

### F1. A bare `.tar` cannot be fetched at all. HIGH

**Disposition.** Fixed at `a5a8e1e`. Recognition reads the 262 bytes a tar needs, which the archive crate now states as `SNIFF_LENGTH` rather than the caller guessing.

`crates/cli/src/run.rs:1638` reads a 16-byte header and hands it to
`fetchloom_archive::recognize`. `crates/archive/src/recognize.rs:86` finds the
tar magic at `header[257..262]`. `sniff` can therefore never answer `Tar`, and
every bare tar is refused as unrecognized bytes.

Measured, both formats GNU tar writes:

```
$ fetchloom get file:///.../posix.tar --output d --json     # tar --format=ustar
{"kind":"archive.unsupported","layer":"extract", ... ,"next_action":"rename it or state its format in a manifest, because the name said \"tar\" and the archive's bytes said unrecognized bytes"}
$ echo $?
70
$ fetchloom get file:///.../bad.tar --output d --json       # GNU default format
  ... the same, exit 70
$ fetchloom get file:///.../sample.tar.gz --output d --json # exit 0
$ fetchloom get file:///.../one.gz, pack.zip, one.bz2       # exit 0 each
```

Every other magic this build sniffs lives in the first 16 bytes, which is why
only `tar` is affected.

contracts.md Formats lists `tar` as one of the ten containers extraction
recognizes. The phase 3 gate claims "Ten formats ship and each is read back from
bytes it really holds: tar bare and wrapped in gzip, zstd, xz and bzip2".

Why nothing caught it: the archive crate's own tests call `recognize()` with a
full header and pass. `crates/cli/tests` names `.tar.gz` thirty times and `.tar`
eight, and every one of the eight is either a bundle, which goes through the
bundle reader rather than the recognizer, or `contract.rs:84`, an offline refusal
that never reads a byte. `xtask/src/network.rs` fetches two `.tar.gz` and one
`.tar.xz`. The defect sits exactly at the seam between the tested unit and the
shipped command.

Cost to fix: one constant, plus a CLI-level test per shipped format.
If never fixed: one of the ten advertised formats does not work, and the failure
message blames the archive.

### F2. `--layout flatten:n` fails on every archive that declares its directories. HIGH

**Disposition.** Fixed at `a5a8e1e`, and it was the contract that was wrong. A directory left with no path names the destination and is dropped; a file left with no path is still fatal; an empty result fails. contracts.md:121 was amended in the same commit.

Measured on a `tar.gz` holding `pkg-1.0/README` and `pkg-1.0/src/m.txt`:

```
$ fetchloom get file:///.../deep.tar.gz --output d2 --layout flatten:1 --json
{"kind":"destination.unrepresentable", ... ,"next_action":"flatten fewer than 1 components, because pkg-1.0 has 1 and would be left with no path"}
```

Measured on a zip of the same two files written with `zip -D`, which stores no
directory entries:

```
$ fetchloom get file:///.../flat.zip --output z2 --layout flatten:1 --json
{"status":"materialized", ... ,"entries":3, ... }
$ find z2
z2  z2/README  z2/src  z2/src/m.txt
```

Stripping a top-level `pkg-1.0/` is the only thing flatten is for. A top-level
directory entry has exactly one component, so at `n=1` the run always fails when
the writer wrote directory headers and always succeeds when it did not.

contracts.md Selection makes this inevitable as written: "Every ancestor
directory of a selected member is included whether or not a pattern matched it"
and "a member left with no path after dropping `n` components fails". Both
sentences are implemented correctly and together they make the feature
unreachable for `tar -c`, which is how nearly every published dataset tarball is
made.

Fetchloom synthesizes the ancestor directories itself for the zip case -- both
runs report the same entry count under `keep` -- so the same logical tree
flattens or fails on an encoding detail the user cannot see. That is a
determinism problem as well as a usability one.

Cost to fix: a contract decision first, then a few lines. The obvious answer is
that a directory member emptied by flattening is dropped rather than fatal, and
a *file* member emptied by it is still fatal.
If never fixed: the flag exists and cannot act, which contracts.md's Command
surface forbids.

### F3. A plain local file writes no lock, and says something untrue about why. HIGH

**Disposition.** Fixed at `a5a8e1e`. Whether a local file is an archive is decided from its own bytes, later, so the one-object path no longer gates on the extension. The degrade that called a file a directory is gone with it.

`crates/cli/src/run.rs:254` takes the one-object path only when
`archive_to_unpack` returns `Some`, which happens only for a recognized archive.
A plain file falls through to `materialize::walk`, produces no artifact, so
`run.rs:2507 resolved_object` returns empty, and `crates/cli/src/locked.rs:105`
emits:

```
{"event":"degrade","requested":"a lock pinning what blob.txt resolves to","used":"no lock entry at all","reason":"this reference names a directory, which resolves to no object, and a lock pins objects rather than trees"}
```

`blob.txt` is not a directory and it did resolve, to
`blake3:...`, which the cache holds with its interop digest in
`meta/object/`. Measured: no `fetchloom.lock` is written, and none is written
with an explicit `--lock my.lock` either. `plan` on the same reference then
exits 40 `policy.trust_refused`.

contracts.md Reference grammar: "A reference naming one object materializes a
destination directory holding that one entry". Locked runs: "An unlocked run
records what it resolved."

`crates/cli/tests/lock.rs` has `a_directory_source_writes_no_lock_and_says_so`
and nothing for a single file, so the branch is covered for the case where its
message is true and not for the case where it is false.

Cost to fix: route a non-archive single file through `materialize_object`, or
carry the interop digest through the walk. A test per reference shape.
If never fixed: `plan`, `apply` and `--locked` are unusable for `file:`
references naming one file, and the event stream states a falsehood.

### F4. `cache clear` cannot clear a cache whose format does not match. HIGH

**Disposition.** Fixed at `a5a8e1e`. `Cache::clear` was a method on an open cache, which is why it could not run; it is a free function taking a root, and contracts.md:516 now names it as the one command the format check does not apply to.

Measured: write anything but the fingerprint into `<cache>/format`, then

```
$ fetchloom cache status
cache.format_mismatch: run cache clear, because ...\cache was written in a format this build does not read and nothing is migrated
$ fetchloom cache clear --yes
cache.format_mismatch: run cache clear, because ...   # the same, exit 80
```

The command the error names as the fix is refused by the check the error is
about. The only way out is deleting the directory by hand, which the message
never suggests.

contracts.md Modes contradicts itself in one paragraph: "a format mismatch
always has one command that fixes it", and then "Every command that would touch
the cache fails with `cache.format_mismatch` and exit 80, naming `cache clear`
as the fix." The code implements the second sentence.

Cost to fix: exempt `clear` from the check, and fix the contract to say so.
If never fixed: the one failure mode the contract singles out as user-fixable is
the one that traps the user.

### F5. `file_operations` differs by two orders of magnitude across paths that produce one tree digest. HIGH

**Disposition.** Fixed at `ad5cd0b`. The CLI created directories with `std::fs::create_dir_all`, which the Platform seam never sees. It is `Platform::create_directories` now and both paths count. They still differ, and that difference is real work.

Measured, 64 files of 256 KiB, tree digest `blake3:ca5502...` in every row:

| run | file_operations | bytes_written |
|---|---|---|
| cold, cache open | 338 | 2,097,152 |
| warm, cache open, new destination | 2 | 1,048,576 |
| unchanged | 1 | 0 |

Measured, 1024 files of 1 KiB, same tree either way:

| run | file_operations |
|---|---|
| cold, cache open | 5,138 |
| cold, `--no-cache` | 1 |

`crates/engine/src/work.rs` documented the counter as being taken "at the
boundary each kind of work goes through rather than at each call site, so a new
caller counts by construction". That is true of `file_operations`, which is
incremented only inside `crates/platform/src/lib.rs` and `crates/cache/src`, and
false of the byte counters, which are incremented at eighteen scattered sites.
The CLI's own placement paths -- `crates/cli/src/materialize.rs` and
`fill_staging` in `run.rs` -- use `std::fs` directly and count no file operation
at all. (That docstring is gone as of `83a9714`; the behavior is not.)

contracts.md Output streams: file operations "counts every file or directory the
run created, every rename it performed, and every flush it issued ... counted
where the platform performs the operation, so a new caller counts by
construction."

This is the third and fourth instance of the same shape. The phase 5 record found
the counter blind to the lock file and to a cache record, and called it "the
second time a counter has been introduced and immediately been found blind to the
thing it was introduced for."

Cost to fix: route the CLI's placement through the Platform seam, which already
carries `create_file_exclusive`, `create_directory_exclusive`, `clone_or_copy`
and `publish_file`, all of which count.
If never fixed: phase 6 gates adaptation on a metric that reads 1 for a run that
created 1024 files.

### F6. `bytes_written` reads zero where a byte copy happened. HIGH

**Disposition.** Fixed at `ad5cd0b`. Archive extraction and `NativePlatform::copy_bytes` both count their bytes now.

`crates/platform/src/lib.rs:122` `copy_bytes` calls `std::fs::copy` and
`work.touched_file()`, and never `work.wrote_bytes()`.

Measured, `apply` of a 233-byte `tar.gz` from the cache into an empty
destination, extracting three files:

```
{"work":{"bytes_read":0,"bytes_written":0,"requests":0,"file_operations":6}}
```

`bytes_written` is a deterministic gate metric, so a regression on the
clone-or-copy path is invisible to `cargo xtask bench --compare`.

Cost to fix: one line.
If never fixed: as F5.

### F7. The many-small-files corpus is three quarters duplicates. HIGH

**Disposition.** Fixed at `ad5cd0b`. The generator writes each file's index into its leading bytes, so every file in a corpus is distinct. The baseline was re-recorded and each moved number attributed to the measurement or to the code.

`xtask/src/bench.rs:363` builds the corpus from
`non_repeating_bytes(index % 251)`, so 1024 files hold 251 distinct objects and
773 of them are cache hits. `file_operations` of 1273 = 5 x 251 + 18 confirms it.

Measured on this machine, same shape, quiet:

| corpus | wall | file_operations |
|---|---|---|
| 1024 files, 251 distinct (the harness) | 5,512 ms | 1,273 |
| 1024 files, 1024 distinct | 18,882 ms | 5,138 |

The regime the project calls its worst number, and has deferred three times,
under-reports the case it is named for by 3.4x in wall clock and 4.0x in
operations. It is measuring the deduplicating cache at least as much as it is
measuring many small files.

Cost to fix: change one modulo.
If never fixed: every conclusion drawn from the regime is drawn from the wrong
workload, and the deferred packing decision keeps being taken against a number
four times too small.

### F8. Ten of the thirty-four contracted events are emitted by no production code. HIGH

**Disposition.** Fixed at `ad5cd0b` for the four that were reachable: `extract.start`, `extract.end`, `cache.hit`, `cache.miss`, plus `cache.wait` through a new `Store::waited`. Six remain, each belonging to an unshipped phase: `resolve.alias` needs phase 7, and `listing.start`, `listing.skipped`, `listing.end`, `source.probe` and `source.selected` need phase 8.

Constructed only in `crates/engine/tests/contracts.rs`: `resolve.alias`,
`cache.wait`, `listing.start`, `listing.skipped`, `listing.end`, `source.probe`,
`source.selected`, `extract.start`, `extract.reject`, `error`.

Seven belong to phases that have not shipped: `resolve.alias` needs a moving
alias, the three `listing.*` need directory listing, the two `source.*` need
multi-source selection, and `extract.start` is paired with `extract.reject`.
Three do not:

- `extract.reject`. contracts.md Materialization: "Every rejection stops the run,
  **emits `extract.reject`** naming what was rejected, and publishes nothing."
  The forty-five-archive hostile corpus is rejected by kind and the event fires
  nowhere.
- `cache.wait`. contracts.md Cache: "A second process wanting an object being
  written waits and reuses the result." Shipped in phase 1 and proven by the
  concurrency suite. The event is not emitted.
- `error`. A failure reaches stdout or stderr as JSON or a line and never enters
  the event stream, so a `watch` consumer sees a run stop without being told why.

Three more are produced in code and unreachable for a different reason:
`credential.required`, `credential.offer` and `credential.declined` are all
constructed inside `crates/cli/src/policy.rs`, which nothing calls. See F10.

`every_event_name_the_contract_lists_is_produced_by_a_payload`
(`crates/engine/tests/contracts.rs:125`) asserts that the enum covers the
contract's names, which it does. It says nothing about whether a run produces
them, and passes with all ten unreachable.

Measured event streams, whole runs, nothing elided:

```
get file:///dir       run.start resolve.start degrade resolve.end plan.ready
                      transfer.start cache.miss transfer.progress transfer.end
                      publish.commit degrade run.end
get file:///x.tar.gz  run.start resolve.start resolve.end plan.ready
                      publish.commit run.end
apply plan            run.start resolve.start resolve.end cache.hit plan.ready
                      publish.commit run.end
```

An archive fetch emits six events. contracts.md: the live view "cannot display a
fact that the event stream does not carry", and phase 9 builds it on that alone.

Cost to fix: two emit sites for the shipped pair, and a decision about `error`.
If never fixed: phase 9's live view cannot show extraction, cannot show a
rejection, and cannot show a failure.

### F9. Every `*.end` event but one carries `duration_ms: 0`. HIGH

**Disposition.** Fixed at `ad5cd0b`. Duration is a `Span` taken at the start of the operation, and every end event carries a real one.

Only `crates/engine/src/transfer.rs:313` measures. Zeroed at
`cli/src/main.rs:115` (`run.end`), `run.rs:255,276,1223,1930,2066`
(`resolve.end`), `run.rs:686` (`transfer.end`) and `repair.rs:83,131`.

Measured on a real run: `resolve.end 0`, `transfer.end 0`, `run.end 0`.

standards.md Observability: "Every operation emits start and end events with
byte counts and durations."

Cost to fix: an `Instant` per operation.
If never fixed: the event stream carries no timing at all, so the live view and
`watch` can never show any, and nothing downstream can measure a phase of a run.

### F10. The Policy seam is never used by a run. HIGH

**Disposition.** Deferred with a statement, at `ad5cd0b`. Policy is asked for the cache directory, the verification and durability settings, and whether it accepts a trust class, and all four are read. What it does not do is arbitrate between two candidates, which is what phase 7 needs it for. It is a settings carrier today, and phase 7 either widens it or admits it is one.

**Widened at `aa19f31`.** `crates/cli/src/main.rs` now builds a `CommandLinePolicy` in every command that touches a run (`main.rs:436,524,777,853`), where before nothing built one outside `crates/cli/tests/policy.rs`. Production code reads eleven of the trait's fifteen methods off it: `cache_directory`, `verification`, `durability`, `accepts`, `limits`, `concurrency`, `per_host`, `bandwidth`, `aggressive`, `adapts` and `io` all have a call site in `main.rs`. `offline`, `credential`, `offer_credential` and `terms` still have none outside `policy.rs`'s own tests, so `policy.credential_missing`, `credential.required`, `credential.offer` and `credential.declined` remain unreached in a run, as F8 already says. The commit's own description names what changed: `--concurrency`, `--per-host` and `--bandwidth` are resolved through the seam and given a per-host controller that adapts from what the cache recorded, which is the seam's first arbitrating caller and the thing this finding said was missing. What is not closed is candidate arbitration in the sense phase 7 needs -- choosing between two sources -- because there is still one source until phase 7 ships it. The settings half of this finding is closed; the arbitration half is confirmed still open and is phase 7's, not phase 6's.

`crates/cli/src/policy.rs:92` is the only implementation of
`fetchloom_engine::seam::policy::Policy`. `CommandLinePolicy` is constructed
nowhere but `crates/cli/tests/policy.rs`, nine times. Every one of the trait's
twelve methods is called only from that file. `crates/cli/src/main.rs` never
builds one: the run reads its settings from `Settings` directly.

Visible consequences:

- `FETCHLOOM_TOKEN_<HOST>` is read only by `Policy::credential`, and therefore by
  no run. contracts.md Environment lists it and contracts.md Credentials
  specifies a whole lookup order behind it.
- `policy.credential_missing`, `credential.required`, `credential.offer` and
  `credential.declined` are produced only inside `policy.rs`, so none reaches a
  run.

roadmap.md names Policy as one of six seams "defined in phase 0 and never change
shape afterward". It has the shape and no traffic.

Cost to fix: wire it, or delete it and say Policy is not a seam. The second is
cheaper and the first is what the roadmap promises.
If never fixed: phase 7 discovers on its first provider that the seam it was
going to implement against has never run.

### F11. `cache.corrupt` is the catch-all for every filesystem failure in two crates. MEDIUM

**Disposition.** Fixed at `b7a8224`. One decider, `error::filesystem_failure`, plus `error::lock_failure` for the one operation whose kind comes from what was attempted. A test walks both crates and fails on any function that turns an `io::Error` into an `Error`.

One hundred and five sites across `crates/cache/src` and `crates/platform/src`,
including `platform/src/linux/mod.rs:80` (a `statx` of any path, destination
paths included), `:261` (flush), `:274` (directory flush), `:372` (`owns`), and
the Windows equivalents at `windows/mod.rs:47,60,73,348`.

`cache.corrupt` exits 80. An `ENOSPC` during a flush and a permission failure on
a destination stat both surface as a corrupt cache rather than as
`resource.disk` or a destination kind.

This is the same shape the project has already found twice: the phase 1 record
describes a macOS runner reporting `No space left on device` inside a
`cache.locked`, and the phase 2 record describes a network read failure reported
as `cache.corrupt`, which made an interrupted `get` exit 80 instead of retrying.

Related, and measured directly:

```
$ fetchloom get file:///.../src --output Q:/nowhere/out --json
{"kind":"destination.foreign","layer":"materialize","dataset":null, ... ,"next_action":"Q:\\nowhere\\.out.fetchloom-staging: The system cannot find the path specified. (os error 3)"}
$ echo $?
60
```

`crates/cli/src/run.rs:305` maps that `create_dir_all` failure to
`DestinationForeign`, as do seven other sites in the same file. contracts.md
defines `destination.foreign` as "Entry present, not in the resolved tree". The
`next_action` is a raw OS string rather than an action, and `dataset` is null
although the run resolved `src`.

Cost to fix: a mapping from `io::ErrorKind` to a kind, applied at the `failure`
helpers. Not large, but it changes what a hundred call sites report.
If never fixed: exit codes mislead, and `cache.corrupt` -- which should mean
"your cached bytes are wrong" -- means "something went wrong near a file".

### F12. A nonexistent host is retried five times and reported as refused. MEDIUM

**Disposition.** Fixed at `b7a8224`. The classifier matches on `ureq::Error` rather than on substrings of its message, and keeps a name no resolver knows apart from a resolver that could not be reached.

```
$ fetchloom get https://no-such-host.invalid/x.tar.gz --json
{"kind":"network.refused", ... ,"attempts":5,"retryable":true,"next_action":"try the source again, because the request did not complete: io: No such host is known. (os error 11001)"}
$ echo $?
20
```

NXDOMAIN is terminal. standards.md Network: "Never retry a non-idempotent or
terminal failure." roadmap.md phase 2 Prove names DNS failure as a case that must
produce the correct exit code; the exit code is right, the classification and the
attempt count are not. There is no `network.dns` kind for it to be.

Cost to fix: classify the resolver's own error separately.
If never fixed: every typo in a hostname costs five backoffs.

### F13. Cancellation is contracted and implemented nowhere. MEDIUM

**Disposition.** Fixed at `d383f09`. An interrupt records that it arrived; a run stops where it counts a file operation, which is the boundary between one unit of work and the next. A second interrupt ends the process. Both are tested, including that the cache invariants hold after the second.

contracts.md Cancellation: "First interrupt: stop new work, finish flushing
in-flight buffers, record resumable state, exit `130` within two seconds. Second
interrupt: abort immediately."

No signal handler exists anywhere in `crates/cli/src`. The only occurrence of 130
is `crates/engine/src/outcome.rs:47`, a variant nothing constructs. No phase in
roadmap.md names cancellation in its Build paragraph, so it is not deferred; it
is unscheduled.

The half that matters most does hold by construction: nothing enters `objects/`
without a completed verification and an atomic rename, so an interrupt leaves a
valid object or a resumable partial either way. What is missing is the exit code,
the two-second bound, and the deliberate flush.

Cost to fix: a signal handler and a cancellation token through the transfer loop.
If never fixed: a contracted behavior with an exit code in the table never
happens, and scripts branching on 130 never see it.

### F14. Disk accounting is contracted before the transfer and happens during it. MEDIUM

**Disposition.** Deferred. `plan` states the requirement per volume from the size the lock pins, which is what contracts.md asks, and no sentence requires `get` to check free space first. What is wrong is smaller: the plan reports `staging` and `destination` as zero for an archive whose expanded size is unknown, while listing `expanded` under `unknown`, and contracts.md:265 says a field is never estimated into a number. Clearing it changes the shape of a portable artifact, which is additive-only, so it is a decision rather than an edit.

**Fixed at `c83a6a8`.** The decision was made and the shape changed. `crates/cli/src/planning.rs` now leaves `staging` and `destination` without a `bytes` field, and `PlanArtifact::unknown` names both alongside `expanded`, exactly as it names `expanded` alone today. `partial` and `cache` still state a number, because they come from the size the lock pins rather than from the expanded length. contracts.md:270 was amended in the same commit to say so. `crates/cli/tests/tune.rs:466 a_plan_lists_every_field_no_source_stated_and_reports_none_of_them_as_zero` asserts `disk.staging.bytes` and `disk.destination.bytes` are `null` and both names appear in `unknown`, and that `partial` and `cache` are still numbers. The pre-flight check this finding also named is still nowhere; that half stays open, and nothing in phase 6 claimed it.

contracts.md Disk accounting: "Four requirements are computed separately ...
Requirements on a shared volume are summed and checked against that volume.
Insufficient space fails before transfer begins."

Every `ErrorKind::ResourceDisk` in the tree is raised where a reservation or a
write is refused. There is no pre-flight check anywhere, and nothing sums a
volume's requirements.

`crates/cli/src/planning.rs:98,102` hard-code `bytes: 0` for the staging and the
destination requirement. The partial and the cache requirements are computed --
the sum of every artifact the cache does not already hold -- and are correctly
zero in the run below because the object was cached. The other two are zero
always. The plan lists only `expanded` as unknown, so a reader is told the
staging and destination requirements are known and are nothing:

```yaml
disk:
  cache:       {bytes: 0, volume: "C:"}
  destination: {bytes: 0, volume: "C:"}
  partial:     {bytes: 0, volume: "C:"}
  staging:     {bytes: 0, volume: "C:"}
unknown:
  - "expanded"
```

contracts.md Plan: "A field is never estimated into a number." Zero is a number.

Cost to fix: compute what is known and put the rest in `unknown`. The pre-flight
check is a larger job and needs the expanded size, which most sources do not
state.
If never fixed: a run fills a disk partway through instead of refusing at the
start, and a plan's disk block is decorative.

### F15. No result carries a trust class. MEDIUM

**Disposition.** Fixed at `ad5cd0b`. `RunResult` carries the weakest trust class of any artifact in the receipt, and the policy is asked whether it accepts that class before the run reports success.

`get --json` returns `status`, `dataset`, `tree`, `destination`, `entries`,
`bytes` and `work`. `crates/cli/src/run.rs:58 RunResult` has no trust field.

The class exists only inside a receipt (`run.rs:1829`), and for a `file:`
reference the receipt's `artifacts` map is empty, so for that shape it is nowhere
at all:

```yaml
artifacts: {}
```

vision.md Promises: "Every result carries a trust class with a mechanical
definition." features.md: "Every result is labeled with one of four classes."

Cost to fix: one field, and F3 for the empty-artifacts half.
If never fixed: the product's stated headline promise is not in its output.

### F16. Four seams have one implementation, and three have no defense. MEDIUM

**Disposition.** Fixed at `a9848c5`, in standards.md, which is where the contradiction was. The six named seams are exempt from the delete-a-lonely-trait rule, with the reason written down.

| Seam | Implementations |
|---|---|
| Platform | `platform/src/lib.rs:187`, `faults/src/platform.rs:48`, which nothing calls -- see F17 |
| Observer | four, in `cli/src/observer.rs` and `faults/src/observer.rs` |
| Store | `cache/src/store.rs:263` only |
| Policy | `cli/src/policy.rs:92` only, and see F10 |
| Archive | `archive/src/reader.rs:188` only |
| Source | `sources/src/http.rs:190` only |

standards.md Design: "A trait exists to allow a second real implementation or a
fault-injecting one. A trait with one implementation and no test double is
deleted." roadmap.md Seams: the six "are defined in phase 0 and never change
shape afterward."

The two rules contradict each other and three seams sit in the gap. Source is
defensible: phase 7 is exactly the second implementation. Store, Policy and
Archive are not covered by either rule as written.

Cost to fix: a sentence in standards.md exempting the six named seams, which is
the honest resolution, since the seams are an architectural commitment rather
than dependency injection.
If never fixed: a stated rule that three of six seams break.

### F17. The platform half of the fault library has no caller and no test. MEDIUM

**Disposition.** Fixed at `b7a8224`. `FaultyPlatform` has its first caller, and the ENOLCK skip is closed by testing what it was asserting rather than by finding a filesystem.

`crates/faults/src/platform.rs` (163 lines, `FaultyPlatform`) and
`crates/faults/src/schedule.rs` (111 lines, `Faults`, `Operation`) are referenced
by nothing outside `crates/faults/src`. `crates/faults/tests` holds `archives.rs`
and `http.rs` only. 274 lines that compile, lint, and never run.

standards.md Tests: "Fault injection is a library in the repository, not a mock
in a test file. It is part of the product." standards.md also forbids dead code.

It is also what would clear the `ENOLCK` skip that the phase 1, 2 and 5 records
each carry forward: a fault-injecting lock failure needs no exotic filesystem,
and `schedule.rs` already has the shape for it.

Cost to fix: use it, in the skip it was built for.
If never fixed: the second Platform implementation that justifies that seam does
not actually exist at runtime, and the one named skip stays permanent for want of
a filesystem when a test double would do.

### F18. The Source seam cannot report the origin that served the bytes. MEDIUM

**Disposition.** Fixed at `69b18a8`. `Source::fetch` returns `Served`, so the location that answered travels with the bytes, and the witness records it rather than the first location the manifest listed.

`crates/sources/src/http.rs:211,248` set
`SourceMetadata.location = SafeUrl::new(location)`, the location that was
*requested*. `send` follows redirects internally at `http.rs:100` and discards
`current`.

So the witness-origin gap the phase 5 record names is structural rather than a
missing assignment: there is no field to put the answer in. contracts.md
Witnesses requires the origin to be "the origin that served the bytes"; this
build records what was asked for, so two aliases redirecting to one host count as
two origins and can raise a class they should not.

The seam also carries no egress cost, which contracts.md Source selection scores
on and which phase 7's plan must report, and no requester-pays notion.

Cost to fix: one field on `SourceMetadata` and on `Transferred`. Widening a seam
needs the same justification as changing a contract, which is the actual cost.
If never fixed: `corroborated` can be reached by a mirror pointing at itself,
which is the one thing the independence rule exists to prevent.

### F19. Four declared limits are read nowhere, one of them a memory ceiling. MEDIUM

**Disposition.** Two fixed at `69b18a8`: the credential offer threshold and the listing entry limit are read. Two deferred with reasons. `Probed candidates` bounds a choice between sources and there is one source until phase 8. `Resident memory` needs a resident set query on both platforms behind the Platform seam, which is a seam widening and a piece of work rather than a line that reads a field.

`crates/engine/src/limits.rs` declares `resident_memory`, `probed_candidates`,
`credential_offer_threshold` and `listing_entries`. No file under `crates/*/src`
reads any of them.

Three belong to phases that have not shipped. `resident_memory` does not.
contracts.md Limits gives it 1 GiB and says none of these "may be raised past a
hard ceiling that would allow unbounded memory or disk use". Nothing checks it,
and it pairs with the whole-list reads in F26.

contracts.md Limits also says "Defaults. All configurable." Nothing in that table
is configurable in this build: no flag, environment variable or config key
reaches any field of `Limits`. The five that contracts.md does give flags to --
concurrency, per-host, bandwidth, retries and timeout -- are not in this struct,
and none of those five flags is in the binary either.

Cost to fix: either enforce it or state in contracts that it is a design bound
rather than a runtime one.
If never fixed: a stated memory ceiling nothing enforces.

### F20. `repair` cannot reach an object a local reference cached. MEDIUM

**Disposition.** Fixed at `69b18a8`. The digest comes from the file, which repair reads anyway, and `Repair` is generic over the Source seam with a `FileSource` behind it.

`crates/cli/src/run.rs:1244,2279` call `remember()` only on the remote transfer
path, so `meta/resolution/` is written for an `https:` reference and never for a
`file:` one or a manifest.

Measured: `get file:///big.bin` (70,000,000 bytes, so a 4,232-byte chunk tree is
stored in `outboard/`), damage one megabyte of the cached object, then

```
$ fetchloom repair file:///.../big.bin --json
{"kind":"reference.unresolved", ... ,"next_action":"run get on file:///.../big.bin first, because repair puts right an object this cache already holds and this one has never been fetched here"}
$ echo $?
10
```

`cache verify` still quarantines it correctly with a diagnosis. It is only the
reference-to-digest step that cannot answer. contracts.md Repair says the
reference is resolved "exactly as `get` does" and nothing that limits it to a
network reference; the phase 5 gate proves repair over the fault server alone.

Cost to fix: record a resolution on the local path too, or resolve a `file:`
reference to a digest by hashing it.
If never fixed: the project's headline capability is unavailable for half the
reference forms it supports.

### F21. Every cold local fetch writes the bytes twice. MEDIUM

**Disposition.** Confirmed and deferred, with the audit's own reason: it needs a volume that clones, which is the phase 0 debt still open, and no machine in this matrix can measure what removing it would buy. The small end improved anyway at `a9848c5`, where a packed object is written once rather than twice.

Measured: `one-large-file` writes 536,887,240 bytes for a 268,435,456-byte
source, which is the destination, the cache copy, and the 16,328-byte chunk tree.
`cold-cache` writes 33,554,432 for 16,777,216 read.

`crates/cli/src/run.rs:758 through_cache` writes the destination and then hands
the cache that file, which is the right shape: on a volume that shares blocks the
second write is metadata. On this volume it is a full copy, and 256 MiB copies in
251 ms measured, so the amplification is about a quarter of the one-large-file
regime's whole wall clock.

A remote fetch does not pay it: `cold-transfer` writes 4,194,304 for 4,194,304.

Cost to fix: nothing in the code. It needs a volume that clones, which is the
phase 0 debt still open. Worth stating because it is the largest single
inefficiency in the one-large-file regime and because no machine in this matrix
can measure what removing it would buy.
If never fixed: local fetches cost double the bytes on every filesystem without
reflinks, and no benchmark regime names it.

### F22. Six error kinds are produced by no test, and six contract rows have none. MEDIUM

**Disposition.** Fixed at `b7a8224`. Six of the seven kinds with no test have one, including `policy.credential_invalid`, which is now produced where contracts.md says it should be. `source.identity_changed` is still produced by nothing and needs a human: the seam is never given the identity it would compare against, and clearing it needs either a contract sentence or a widened seam.

**The seventh closed at `f5dc98d`.** `source.identity_changed` is now produced by `crates/engine/src/transfer.rs rung_for`, and the ruling is the rung the transfer stood on. Rungs three, four and five treat a validator that no longer matches as ordinary: the partial is discarded, the transfer restarts from zero, and a `degrade` names the rung it stood on and the rung it fell to (`transfer.rs:268-276`). Rung two is different by construction: it is reached only when the source stated an immutable content address or an immutable version identity, and if the current response states a *different* one under that same immutable promise, the promise itself was false, so the transfer fails with `source.identity_changed`, is not retryable, and exits 20. A source that has simply stopped stating an immutable identity has broken no promise and falls to `NoValidator` like any other rung, which restarts rather than fails. `crates/engine/tests/transfer.rs an_immutable_identity_that_moved_is_terminal_rather_than_a_restart` and `a_content_address_that_moved_is_terminal_rather_than_a_restart` hold the terminal case and assert `ErrorKind::SourceIdentityChanged`, `!retryable()`, and exit code 20 (`ExitCode::Network`, `outcome.rs:40`); `a_source_that_stopped_stating_an_immutable_identity_restarts_rather_than_failing` holds the non-promise case. `crates/cli/tests/transfer.rs` exercises the same shape at the command surface.

`crates/engine/tests/contracts.rs:102`,
`every_error_kind_is_reachable_and_carries_the_required_fields`, constructs each
`Error::new(kind, ...)` by hand and asserts the builder round-trips. It cannot
fail for the thing its name claims and exercises no code path.

Six kinds appear in the test suites only as strings in that label table:
`alias.unstable`, `network.tls`, `policy.credential_missing`,
`policy.credential_invalid`, `policy.terms_required`, `resource.limit`.

`alias.unstable` is the one that matters. `crates/engine/src/lock.rs:69,82,203`
produce it for four rows of contracts.md's Locked runs table -- a changed
`manifest`, `release`, `select` or `layout`, and an artifact present in one and
not the other -- and `crates/cli/tests/lock.rs` asserts none of them. That is
shipped phase 4 behavior with no coverage.

standards.md: "Every entry in the contracts tables has a test. Every error kind
has a test that produces it."

Two more contracts tables are partly covered. Revalidating a completed remote
object has three rows -- `304`, `200`, and anything else --
and `crates/cli/tests/prove.rs:648` covers `304` only. The `200` branch emits a
`degrade` at `crates/engine/src/transfer.rs:353` that no test observes.

Cost to fix: six tests.
If never fixed: the locked-run comparison, which is the whole point of a lock,
is proven for two of its six rows, and the conditional request is proven for one
of its three answers.

### F23. `explain` reports six settings and ignores `--json`. LOW

**Disposition.** Fixed at `1a59ff8`. `explain` honors `--json`, and reports the thread budget as measured rather than as a value nothing supplied.

```
$ fetchloom explain
project config: none found
user config: none found
offline = false (default)
threads = ? (default)
display = plain (default)
cache.dir = C:/Users/you/flman/cache (environment)

$ fetchloom explain --json      # the same plain text
$ fetchloom explain threads
threads = ? (default)
```

contracts.md and features.md: "`explain` shows every effective setting and where
it came from, including values chosen by measurement." `threads` reads `?`
although the budget is detected locally, and contracts.md reserves `?` for a
value the *source* did not supply.

Cost to fix: report the resolved settings rather than a hand-written list, and
honor `--json`.
If never fixed: the command that exists to make configuration unmysterious covers
a fifth of it.

### F24. Flags exist on commands that cannot act on them. LOW

**Disposition.** Fixed at `1a59ff8`. `plan --force` and `plan --adopt` are refused naming the flag, and `get` and `plan` take the one reference they perform.

`plan --force --adopt` is accepted and does nothing; measured, no error, exit 40
for an unrelated reason. `completions --offline --yes` likewise. `get <ref> <ref>`
declares a variadic argument and refuses a second reference with exit 2.

contracts.md Command surface: "There is no state in which something is present
and unable to act, because that is a placeholder, and because a caller cannot
distinguish it from a usage error."

Cost to fix: scope the flag groups per command in `surface.rs`.
If never fixed: a caller cannot tell an accepted-and-ignored flag from a
supported one.

### F25. Two functions compute the platform default cache directory. LOW

**Disposition.** Fixed at `a5a8e1e`. `policy.rs`'s copy is deleted and the test points at the one in `settings.rs`.

`crates/cli/src/policy.rs:206 default_cache_directory` and
`crates/cli/src/settings.rs:43 default_cache_dir`. Only `settings.rs` has a
production caller (`settings.rs:190`); `policy.rs`'s is reached only by
`crates/cli/tests/precedence.rs:265`.

standards.md: "Two ways to do the same thing is a defect. Delete one."

Cost to fix: delete one and point the test at the other.

### F26. The Archive, Store and Source seams hold whole lists in memory. LOW

**Disposition.** Deferred, on the audit's own instruction to measure first, and the measurement has not been taken. `Store::list` just grew: it concatenates the packed index, so a cache of a million small objects holds a million digests and placements. Clearing it needs the million-member experiment and a resident set query to enforce the ceiling against.

`seam/archive.rs members() -> Vec<ArchiveMember>` against a 1,000,000 entry
limit; `seam/store.rs list() -> Vec<ContentDigest>`; `seam/source.rs
list() -> Vec<ListingEntry>` against a 500,000 entry limit.

standards.md Memory: "Nothing is read whole into memory. Every path is streaming,
with a bounded buffer." The phase 4 record justifies the member list deliberately
-- selection is decided against the whole list before any member is read, and
removing the second pass costs either the decompressed size in memory or a spill
file. The other two are justified nowhere. Nothing measures any of the three, and
`resident_memory` (F19) is enforced nowhere.

Cost to fix: measure first. A million-member tar is a cheap experiment nobody has
run.

### F27. Assorted user-facing text defects. LOW

**Disposition.** Fixed at `ad5cd0b` and `1a59ff8`. The 26 literal spaces, the doubled sentence, the plan message advising a flag that was never given, the three reference forms that all said "names a path that exists", and the `<dir>/./local-cache` path.

- `crates/cli/src/main.rs:346` carries 26 literal spaces inside a message, which
  reach the user: `"...the run that wrote it                          reported
  blake3:..."`. A rustfmt artifact that shipped.
- The human error form prints the same sentence twice, once after the kind and
  once after `next:`. standards.md Debloat: say a thing once.
- `plan` of an unpinned reference says "run once without `--locked`" when
  `--locked` was not given.
- `hf:`, `blake3:<hex>` and a bare name are documented reference forms and all
  three fail with "check that ... names a path that exists". `s3://` gets an
  honest message naming what this build resolves; the others do not.
- A project `[cache] dir = "./local-cache"` is reported as
  `C:\Users\you\proj\./local-cache`.

### F28. Documentation that disagrees with the code. LOW

**Disposition.** Fixed at `1a59ff8`, `80c5253` and `a9848c5`. contracts.md's cache tree, the duplicated Path length row, the regime names in standards.md and roadmap.md, the symlink claim in features.md, and the run status, which is a type with four values now. `docs/reference/` was corrected in the same pass it was found wrong in.

- contracts.md Cache omits `receipts/` and `meta/recovered` and `meta/prune`,
  all of which a real cache holds. The code is right.
- contracts.md Limits lists **Path length** twice, with two different
  definitions. The second is right.
- standards.md Measure names eight regimes; roadmap.md phase 6 Build says "all
  seven"; `xtask/src/bench.rs` runs a different seven. `slow disk` and
  `constrained network` exist nowhere, and `cold-transfer` exists in the harness
  and in neither document.
- features.md Safe materialization: "Hard links and symlinks are opt-in." There
  is no flag for either, contracts.md treats a symlink as an ordinary entry type,
  and `run.rs` creates every symlink the tree names unconditionally.
- `crates/cli/src/run.rs:60` types the run status as `&'static str` where
  contracts.md says it "is one of exactly four values, and no other value is ever
  written". All four are produced; the type does not say so.
- The result's `bytes` field means the compressed artifact on a `materialized`
  run and what the entries hold on an `unchanged` run: 1,017,723 then 4,539,480
  for one archive and one tree digest. contracts.md does not define the field at
  all.
- `--events -` writes the event stream to the same stdout as `--json`, which
  contracts.md reserves for the final result only.
- After the macOS removal, `Normalizing` is promised by no volume in the matrix,
  so `crates/platform/tests/capability.rs:419` iterates zero volumes and passes.
  Same shape as `Network`, which is a named skip; this one is named nowhere but
  here and in the removal record.
- `crates/engine/tests/witness.rs:4` carries an unfulfilled
  `#[expect(clippy::unwrap_used)]`, which warns on every build of the workspace.
- `cache clear` with no terminal to ask on fails with `policy.terms_required`,
  which contracts.md defines for licence acceptance. contracts.md says only that
  "a required prompt is a policy failure" and names no kind, so the taxonomy has
  no kind for a confirmation that cannot be asked.

---

## Profiling

Everything below was measured on this machine: Windows 11 26200,
`x86_64-pc-windows-msvc`, 16 processors, NTFS with Microsoft Defender enabled and
not excluded. The container lane is Docker Desktop on the same host, kernel
`6.18.33.2-microsoft-standard-WSL2`, root on overlayfs inside a virtual machine.

Timing on this machine is not reproducible to better than about 40 percent: the
same 1024-object cold run measured 28,300, 19,885 and 18,882 ms across the
session. Every timing below is evidence only where the gap is far larger than
that. Every deterministic number is exact.

### The regimes, both platforms

`cargo run -p xtask -- bench`, natively and in the container.

Deterministic, and **identical on both**, regime by regime:

The no-op regime carries no work counters, only a startup time and the binary
size, so it is absent from this table and present in the next one.

| regime | bytes read | bytes written | requests | file operations |
|---|---|---|---|---|
| cold-cache | 16,777,216 | 33,554,432 | 0 | 338 |
| warm-cache | 16,777,216 | 16,777,216 | 0 | 2 |
| cold-transfer | 0 | 4,194,304 | 2 | 33 |
| interrupted-transfer | 4,194,304 | 4,194,304 | 6 | 53 |
| many-small-files | 1,048,576 | 1,305,600 | 0 | 1,273 |
| one-large-file | 268,435,456 | 536,887,240 | 0 | 26 |

That table agreeing to the byte across two operating systems is the strongest
cross-platform statement this project has produced, and it is stronger than the
tree-digest agreement, because it covers behavior and not only encoding.

Timing, median of the harness's iterations:

| regime | Windows | Linux container |
|---|---|---|
| no-op startup | 7.10 ms | 1.22 ms |
| binary size (deterministic) | 5,515,776 B | 7,388,632 B |
| cold-cache | 1,656.75 ms | 438.51 ms |
| warm-cache | 179.62 ms | 30.99 ms |
| cold-transfer | 75.21 ms | 31.35 ms |
| interrupted-transfer | 364.94 ms | 364.89 ms |
| many-small-files | 2,449.80 ms | 8,737.97 ms |
| one-large-file | 999.35 ms | 2,146.95 ms |
| scanner cost ratio | 72.17x | 5.92x |

Two things to take from it.

`interrupted-transfer` agrees to 0.05 ms across both platforms. That regime is
dominated by retry backoff rather than by I/O, and the agreement is a check that
the harness measures what it thinks it does.

**Linux is 3.6x slower on many-small-files and 2.1x slower on one-large-file**,
while being faster on everything else. That inverts the story the phase 3 record
tells, which attributes the small-files cost to a Windows on-access scanner: the
platform measuring a scanner ratio of 72 beats the one measuring 6 on that regime
by 3.6x. The caveat belongs with the number: the container's root is overlayfs in
a VM, so the timing half of the comparison is between two storage stacks as much
as between two operating systems. The deterministic half needs no caveat.

### Where the many-small-files time actually goes

This is the regime deferred three times, so it gets the space.

Floors on this volume, 1024 files of 1 KiB, one process each:

| | |
|---|---|
| create only (`File.WriteAllBytes` x 1024) | 674 ms, 0.66 ms per file |
| create then rename | 1,304 ms, 1.27 ms per file |
| `cp -r` of the tree | 834 ms |
| write 256 MiB | 317 ms (807 MiB/s) |
| copy 256 MiB | 251 ms (1,020 MiB/s) |

A namespace operation on this volume costs about 0.65 ms.

Fetchloom on the same 1024-file corpus, all 1024 distinct:

| | wall | file operations |
|---|---|---|
| `--no-cache` | 1,159 ms | 1 (uncounted, F5) |
| cache open | 18,882 ms | 5,138 |

Without the cache Fetchloom is at the floor: 1,159 ms against 834 ms for `cp -r`,
1.39x, for a run that also hashes every byte with two algorithms and publishes
through staging. There is nothing to find there.

**Retracted in phase 6.** That `--no-cache` run was the one the finding below
describes: it skipped extraction, and its file operations were uncounted, so
1,159 ms is the cost of a run that did not do the work. Re-measured at `f5dc98d`
on the corrected binary, same volume, same 1024-file corpus all distinct, median
of three runs each:

| | wall | file operations |
|---|---|---|
| `cp -r` of the tree | 1,190 ms | |
| `tar -cf` of the tree | 2,801 ms | |
| `--no-cache` | 8,278 ms | 3,093 |
| cache cold | 9,011 ms | |
| cache warm | 1,815 ms | 1,027 |

So the floor comparison is 7.0x, not 1.39x, and the conclusion that there is
nothing to find there does not survive it. What the corrected number exposes is
not new: a `--no-cache` run now takes the same path a cached run takes, which
means three file operations per file against `cp -r`'s one, and 12,720 bytes
written for 6,144 read. That is the double write already recorded under "Every
cold local fetch writes the bytes twice", deferred with its reason at
decisions.md:5976. This measurement is its cost on this regime.

It is not a tuning problem and phase 6 does not close it. No concurrency,
protocol, or write-path choice removes a write that the store performs by
design; closing it means the store publishing to the destination directly for a
local source, which is a Store change and a contract question about what
`--no-cache` guarantees. Phase 6 records the number and leaves the finding open.

Note also that the cached warm path at 1,027 operations is now cheaper per file
than `--no-cache` at 3,093, which inverts the framing this section was written
under. The packing work at `b12d66d` is what moved it: cache cold is 9,011 ms
here against the 18,882 ms recorded below.

With the cache it costs **18.4 ms per object** for **five file operations each**.
The operation count is exact and linear: 5n + 18 for every n measured (1 object
23, 2 objects 28, 64 objects 338, 1024 objects 5,138).

Where the 18.4 ms is not:

- **The durability flush is 9 percent.** `fast` 18,089 ms against `normal` 19,885
  ms. `strict` costs 22,389 ms and 6,163 operations, the extra 1,025 being the
  directory flushes.
- **Directory fill is a 37 percent term, not the base.** On a fresh cache each
  time: 13.50 ms per object at 64 objects, 14.85 at 256, 18.44 at 1024. Real, and
  not the dominant cost.

What remains is a flat ~13 ms per object for five namespace operations, against
0.65 ms measured for a bare create or rename on the same volume. **That factor of
four is unattributed.** Naming it needs a kernel-level trace this machine has no
profiler configured for, and it is stated as unattributed rather than guessed at.
Candidates not distinguished: the preallocation call, the separate handle per
record, the read of the destination file that `adopt` performs, and the scanner
charging a create and a rename separately.

**What this settles about the deferred packing question.** Packing removes four of
the five operations per object. Fanout removes only the 37 percent growth term.
Whatever the unattributed factor of four turns out to be, it is charged per
operation, so packing attacks it and fanout does not. Packing is the answer and
fanout is not, and that is measured rather than argued. git chose fanout because
its objects are looked up by name at random across millions of them; restic and
borg chose packing because theirs are written in bulk and read in bulk, which is
this workload.

### The no-op regime

7.10 ms on Windows, 1.22 ms in the container. The binary is 5,515,776 bytes.
Startup does no configuration discovery and opens no cache until a command needs
one, and the number reflects it. Nothing here needs attention.

### The regimes that do not exist

standards.md names a **slow disk** and a **constrained network** regime. The
harness has neither, and nothing in the repository can produce either. That is
the honest answer to "profile every regime standards.md names": two of the eight
cannot be profiled because they have not been built.

---

## The deferred list, measured

From the phase 5 gate at `decisions.md:5393`.

**Block cloning has never succeeded on any machine in this matrix.** Still real.
Unchanged. The Windows volume script needs an elevated shell and Hyper-V; the
container's images include no cloning filesystem it can mount as one. Now with a
number attached: F21 measures what it costs, about a quarter of the
one-large-file regime.

**The portable corpus is built by the same code on every target rather than
carried.** Still real for a materialized tree. Phase 4's bundle lane closed it for
objects. What is new is the table under Profiling: every deterministic counter agrees
to the byte across Windows and Linux, which is a stronger statement about behavior than
the tree digest is about encoding, and it was not available before this audit.

**Locking that fails with `ENOLCK` or `EOPNOTSUPP` is a named skip.** Still real
and **now cheaper than when it was deferred**. Every gate record says it needs a
filesystem that refuses. It does not: `crates/faults/src/platform.rs` and
`schedule.rs` already carry a `FaultyPlatform` that can fail any named operation,
and they have no caller at all (F17). The skip needs a test, not a filesystem.

**A network-backed volume is built nowhere.** Still real, unchanged. The NFS
export did not survive the move into the container.

**macOS is compiled and never executed, and two crates are not compiled for it.**
**Closed**, by removal, at `9dc18dd`. The record is at the end of decisions.md.

**Windows on ARM is neither compiled nor run.** Still real. Now the only
unreachable target, and its cost has dropped: `rustls-graviola` 0.4.1 is a
`rustls` `CryptoProvider` that is pure Rust with no C compiler and no assembler,
supporting x86_64 and aarch64, which are all the targets this project has. That
is a concrete replacement for `ring`, the crate that made cross-compilation
impossible from here. Whether it also removes the need for a Windows on ARM
machine is a separate question -- it removes the compile barrier and not the
execute one -- but the compile barrier is what the phase 2 record blamed.

**Rung two is proven only at the unit level.** Still real, needs phase 7.
Unchanged.

**Many small files cost about twenty milliseconds each.** Still real, now
measured properly: 18.4 ms per *object* at 1024 distinct objects, five file
operations each, on this volume. Three things about it are new. The regime
measuring it uses a corpus that is three-quarters duplicates (F7). The cost is
not primarily the Windows scanner, because Linux is 3.6x slower on the same work.
And the packing-versus-fanout question is settled by measurement: packing.

**`plan` and `apply` take one reference and one artifact.** Still real. `get`
declares a variadic reference argument and refuses a second (F24).

**`cache export` writes the whole cache.** Still real, unchanged. Measured: an
export of a two-object cache writes a 2,048-byte tar.

**The remote manifest, object store, provider and metadata reference forms are
not resolved.** Still real, phases 7 and 8. What is new is that three of the four
fail with a message about a path not existing rather than about the form not
being supported (F27).

**The repair copy.** Still real. A repair copies the object before patching it,
which on a volume without cloning writes the whole object locally to save the
network. Unmeasured then and unmeasured now: it needs a cloning volume to
compare against, which is the first deferred item.

**The witness origin field.** Still real and **more expensive than it looked**.
The phase 5 record calls it "one field on `Transferred`". It is not: the origin
never leaves `crates/sources/src/http.rs`, where `send` follows redirects and
discards the final location, and `SourceMetadata` has no field for it (F18). It
is one field on `SourceMetadata`, one on `Transferred`, and a seam widening.

**`corroborated` is unreachable on a single machine with a single mirror.** Still
real by construction, and correct. Not a defect.

**The speculative ingest write. CLOSED.** `crates/cache/src/ingest.rs:44` hashes
first, checks `contains`, and writes only on a miss. Measured: a warm run against
a filled cache and a new destination writes 1,048,576 bytes for a 1,048,576-byte
corpus, exactly the destination, with cache growth zero. The cold path pays one
extra read instead, which `through_cache` avoids entirely by adopting the
destination file it just wrote.

**The destination fingerprint store. CLOSED.** It became the receipt's
`fingerprints` field. Measured on an unchanged 1 MiB tree: `--verify fingerprint`
reads 1,048,576 bytes (the source walk only), `--verify always` reads 2,097,152
(source and destination), `--verify never` reads 1,048,576. The default no longer
rehashes the destination, which is exactly what the phase 4 record said was
missing.

**Revalidation of an unpinned remote. CLOSED.** `crates/cli/tests/prove.rs:648`
asserts one request and no body bytes. Measured independently: a second run of
the hello tarball issues 1 request, reads 0 bytes, writes 0 bytes. The `200`
branch of the three-row table is untested and the retry branch is untested; the
`304` branch works.

### The cache format questions, which phase 6 needs settled

Two are open and one is now answered.

**Answered: packing, not fanout.** See the profiling section. Five operations per
object, charged per operation, on both platforms. Packing removes four of them.

**Open: what a pack format costs the rest of the design.** Packing conflicts with
three things the current design gets for free: an object file that a rename makes
atomic, an object file another process can open under a shared lock, and a cache
that is "plain directories" so "deleting Fetchloom leaves the data usable"
(vision.md, Freedom from lock-in). restic and borg both accept a repository that
is not browsable; this project has promised that it is. That is a vision
decision, not an engineering one, and it has to be made before a format is
chosen.

**Open: whether the object record can be folded into the object.** Two of the
five operations per object are the record's create and rename. The record holds
the interop digest and a fingerprint. Folding it in is a smaller change than
packing and buys 40 percent of the same win, and nothing has evaluated it.

---

## What is missing

Read against features.md and vision.md rather than against the roadmap.

**The first ten minutes.** A new user fetching a `.tar` hits F1 and is told the
archive's bytes are unrecognized. One fetching a package tarball and trying to
strip its top directory hits F2. One fetching a single file and then trying to
`plan` it hits F3. None of the three is exotic; all three are what a person does
first.

**Sources.** The tool speaks HTTPS and `file:` only, and every real dataset lives
on HuggingFace, Zenodo, Kaggle or S3. Two things follow.

First, the practical cost is smaller than it looks for the long tail this project
names as its audience. Zenodo, most institutional repositories, and most lab web
servers serve plain HTTPS with ranges, and the HTTPS source already handles them.
What is genuinely out of reach is anything needing an API to enumerate (an S3
prefix, an HF repo), anything needing a credential (F10 means the credential path
is not wired at all), and anything whose identity is a version rather than an
ETag, which is what rung two waits for.

Second, and more important for phase 7: **the Source seam looks ready and is not
tested for readiness.** It has one implementation, no double (F16), no field for
the origin that served the bytes (F18), no egress cost, no requester-pays, and a
`list` returning a `Vec` against a 500,000-entry limit (F26). The `location: &str`
shape is fine and provider-agnostic. What is missing is the two fields contracts
already require of a second implementation, and they are exactly the fields a
second implementation would discover. Adding a fault-injecting `Source` before
phase 7 would find the rest.

**Things features.md promises that no phase schedules.** Directory listing is
phase 7/8. Metadata absorption (Croissant, Frictionless, pooch, DVC, BagIt,
torrent piece hashes) is phase 8. Both are scheduled. What is *not* scheduled
anywhere: cancellation (F13), which has a contracts section and an exit code;
hard links and symlinks being opt-in (F28); and the two missing benchmark regimes
(F28).

**What the field does that this project does not.** Content-defined chunking, the
thing HuggingFace's Xet work and restic both build on, is deliberately absent
here and should stay absent: this project fetches published immutable artifacts
rather than versioning mutable ones, and CDC buys deduplication across *versions*
of a file, which is not the workload. Bao-style verified streaming, which is what
the outboard tree already is, is the right borrowing and is already borrowed. The
gap against the field is not an algorithm; it is that git, restic and borg all
solved the small-object problem years ago and this project has measured it three
times and not yet chosen.

---

## What I read outside this repository

Read for this audit, and where each one changed a conclusion.

**Small objects on a filesystem.** git shards loose objects into 256 directories
by the first two hex characters of the object id, because many filesystems slow
down badly with a large number of entries in one directory, and practical advice
is to keep a directory between five and twenty thousand entries.
[gitformat-loose](https://git-scm.com/docs/gitformat-loose),
[git-annex on large repositories](https://git-annex.branchable.com/tips/Repositories_with_large_number_of_files/).
On ext4 the htree limit is around ten million entries per directory, and
performance degrades severely long before that; past roughly 320,000 entries
`readdir` returns files in hash order rather than disk order and copy times
collapse.
[Oracle on ext4 large directories](https://blogs.oracle.com/linux/space-management-with-large-directories-in-ext4),
[ext4 vs btrfs getdents](https://lkml.iu.edu/hypermail/linux/kernel/1203.2/00836.html).
This is the case *for* fanout, and it is the one my measurement says does not
apply here: this cache's growth term is 37 percent between 64 and 1024 objects,
and the base cost is five namespace operations charged individually.

restic writes many blobs into pack files rather than one file per blob, with the
header at the end so blobs can be streamed in as they are read, and caps index
files below 8 MiB. [restic design](https://github.com/restic/restic/blob/master/doc/design.rst).
borg does the same with segment files holding many objects each, up to 500 MB.
[borg data structures](https://borgbackup.readthedocs.io/en/stable/internals/data-structures.html).
Both chose packing for a workload where objects are written in bulk and read in
bulk, which is this one. That is the case *for* packing, and it is what the
measurement supports.

**Verified streaming.** The outboard tree here is Bao: chunks and parent nodes in
pre-order with an eight-byte little-endian length prepended, and an outboard form
that keeps the tree separate from the content so a large input is not duplicated.
[bao spec](https://github.com/oconnor663/bao/blob/master/docs/spec.md). IPFS
arrived at the same shape from the other direction: its trustless gateway serves
CAR responses that are incrementally verifiable, and `entity-bytes` is explicitly
"a trustless equivalent of an HTTP Range Request".
[trustless gateway spec](https://specs.ipfs.tech/http-gateways/trustless-gateway/),
[IPIP-0402](https://specs.ipfs.tech/ipips/ipip-0402/). Fetchloom's localized
repair is the same idea applied to an origin that has never heard of it, which is
the genuinely novel part and is worth protecting.

**Content-defined chunking.** HuggingFace's Xet cuts chunk boundaries by a rolling
hash so an edit re-uploads kilobytes rather than gigabytes, and reports 2-3x
faster transfers on the Hub.
[Xet chunking](https://huggingface.co/docs/xet/en/chunking),
[Xet deduplication](https://huggingface.co/docs/xet/en/deduplication).
This is the right answer for versioning mutable files and the wrong one here:
Fetchloom fetches published immutable artifacts, so there is no second version to
deduplicate against. It should stay out.

**Content addressing and trust.** Nix's content-addressed derivations buy early
cutoff and remove the need for substituters to trust each other, at the cost that
the derivation must be a complete description of the build.
[Tweag on content-addressed Nix](https://www.tweag.io/blog/2021-12-02-nix-cas-4/),
[RFC 62](https://github.com/NixOS/rfcs/blob/master/rfcs/0062-content-addressed-paths.md).
Guix's 2026 substitute vulnerabilities are the cautionary half: HTTPS alone did
not prevent exploitation because the narinfo metadata fetch did not securely bind
the download URL to the signed metadata, and a malicious source could return
authorized metadata for one item when another was requested.
[Guix substitute vulnerabilities](https://guix.gnu.org/en/blog/2026/guix-substitute-pull-vulnerabilities/).
That is exactly the shape of F18: a metadata field that describes something other
than what was actually served.

**The tools in this space.** DVC and DataLad both hash content and keep only the
hash in git, and both are reported slow at scale -- DVC taking tens of minutes to
add 600k small files, and checkouts over an hour with few changes.
[DVC #7607](https://github.com/iterative/dvc/issues/7607),
[DVC #10491](https://github.com/iterative/dvc/issues/10491). Both mitigate it with
reflinks, hardlinks or symlinks rather than by packing, which is a fourth answer
this project has not considered and which its "the destination is the real copy"
promise arguably rules out. git-annex is the lower-level tool the other two build
on, and its HTTP path still does not support range requests.
[git-annex p2p over http](https://git-annex.branchable.com/design/p2p_protocol_over_http/).
Fetchloom's resume ladder is genuinely ahead of all three.

**Transfer concurrency.** aria2 caps `--max-connection-per-server` at 16 and its
own guidance is that 16 is more than enough; rclone defaults to 4 transfers and 8
checkers and warns that raising them overwhelms the client as often as it helps,
while recommending 16-32 transfers specifically for many small files.
[aria2c manual](https://aria2.github.io/manual/en/html/aria2c.html),
[rclone docs](https://rclone.org/docs/). Contracts already cap connections to one
host at 4 and require concurrency to be discovered by measurement, which is a
stronger position than either. Phase 6 should treat 16 as the ceiling those two
converged on rather than a number to discover from scratch.

**Connection reuse.** A full TLS handshake costs 7.84-9.22 ms of CPU per peer
against 0.76-1.33 ms for a resumed TLS 1.2 handshake and 2.33-2.62 ms for TLS 1.3,
and around 40 percent of HTTPS connections in the wild are resumptions.
[TLS resumption across hostnames](https://arxiv.org/pdf/1902.02531),
[Cloudflare on 0-RTT](https://blog.cloudflare.com/introducing-0-rtt/).
Contracts already require one pool per host with keep-alive and session
resumption. Nothing here measures whether the client actually resumes, and the
`cold-transfer` regime at 75 ms against a local server is too small to show it.

**io_uring.** Worth it for many independent operations across threads, not for a
single-threaded workload, and the buffer-lifetime problem is not something the
Rust borrow checker helps with.
[Exploring better async Rust disk I/O](https://tonbo.io/blog/exploring-better-async-rust-disk-io),
[io_uring, kTLS and Rust](https://blog.habets.se/2025/04/io-uring-ktls-and-rust-for-zero-syscall-https-server.html).
This workload is five small namespace operations per object. io_uring batches
those well in principle, and the measured base cost here is not syscall overhead
at 0.65 ms per operation. It is not the answer to the small-files number, and
Windows has no equivalent, so it would be one more platform-specific path. Not
worth it.

**Scanners on Windows.** Defender's synchronous on-access scanning is what makes
small-file work expensive on NTFS, and Microsoft's own answer is Dev Drive:
ReFS plus a performance mode that defers the scan until after the open completes,
reported at up to 30 percent better build times.
[Defender performance mode](https://learn.microsoft.com/en-us/defender-endpoint/microsoft-defender-endpoint-antivirus-performance-mode),
[Dev Drive](https://learn.microsoft.com/en-us/windows/dev-drive/).
This is what `doctor` should state in phase 9, and it is also the closest thing to
a cloning volume this project can reach on Windows, so it would close the phase 0
block-cloning debt at the same time.

**A pure-Rust TLS provider.** `rustls-graviola` wraps `graviola` 0.4.1, which
needs "no C compiler, assembler or other tooling ... just the Rust compiler" and
supports x86_64 and aarch64.
[graviola](https://docs.rs/graviola/0.4.1/graviola/),
[rustls crypto providers](https://docs.rs/rustls). `aws-lc-rs`, which rustls now
defaults to, still needs a C compiler and CMake and narrows the target set, so it
is not the answer. This is the concrete replacement for `ring`, which is the crate
the phase 2 record blames for two crates being uncompilable and for the host lane
linting only its own target.

## What I could not determine

**Why a cache file operation costs four times a bare one.** Measured at ~2.6 ms
per operation against 0.65 ms for a create or rename on the same volume, after
excluding the flush (9 percent) and directory fill (37 percent growth term).
Distinguishing preallocation, handle count, the adopt read, and scanner behavior
needs an ETW or `perf` trace, and no profiler is configured on this machine.

**Whether the Linux small-files number is a property of Linux or of overlayfs in
a VM.** The container is the only Linux this matrix has. Bare-metal Linux on an
ext4 or btrfs volume would settle it, and would also settle the cloning debt.

**Whether the `--layout flatten` behavior is a contract defect or a code defect.**
Both are implemented as written; together they make the feature unusable. Which
one changes is a decision, not a finding.

**Whether `resident_memory` is exceeded by a real million-member archive.** The
limit is enforced nowhere and a million-member tar was not built. Cheap to
answer; not answered here.

**Whether any of the ten unemitted events would break the live view.** The live
view does not exist yet, so the claim that it "cannot display a fact the event
stream does not carry" is a contract about a thing nobody has built. What is
certain is that the stream carries no durations and no extraction.

**Whether the six error kinds with no producing test are unreachable or merely
untested.** Three are settled: `alias.unstable` is reachable and untested;
`policy.terms_required` is reachable and untested, and `cache clear` with no
terminal produces it; the two credential kinds are unreachable because of F10.
`network.tls` and `resource.limit` have producing code
(`crates/sources/src/http.rs` and `crates/cli/src/run.rs`) that I did not find a
way to trigger from outside.

**Whether the tree digest survives a physical move between two machines.** Still
the phase 0 debt. Two machines would settle it, and the deterministic-counter
agreement under Profiling is the closest this session could get.

---

## Dispositions

Written at `a9848c5`, against the work done between `b7a3f5b` and it. Every
finding above carries its own disposition line; this is the same information in
one table, so that whether this was done or mostly done can be checked without
reading twenty-eight sections.

| Finding | Disposition | Where |
|---|---|---|
| F1 bare `.tar` | fixed | `a5a8e1e` |
| F2 `--layout flatten:n` | fixed, contract amended | `a5a8e1e` |
| F3 local file writes no lock | fixed | `a5a8e1e` |
| F4 `cache clear` on a mismatch | fixed, contract amended | `a5a8e1e` |
| F5 `file_operations` across paths | fixed | `ad5cd0b` |
| F6 `bytes_written` reads zero | fixed | `ad5cd0b` |
| F7 many-small-files corpus | fixed, baseline re-recorded | `ad5cd0b` |
| F8 ten unemitted events | four fixed, six deferred to phases 7 and 8 | `ad5cd0b` |
| F9 `duration_ms: 0` | fixed | `ad5cd0b` |
| F10 Policy seam unused | deferred with a statement for phase 7 | `ad5cd0b` |
| F11 `cache.corrupt` catch-all | fixed, one decider, enforced by test | `b7a8224` |
| F12 nonexistent host retried | fixed | `b7a8224` |
| F13 cancellation | fixed, both interrupts tested | `d383f09` |
| F14 disk accounting | deferred, needs a contract decision | |
| F15 no trust class on a result | fixed | `ad5cd0b` |
| F16 seams with one implementation | fixed in standards.md | `a9848c5` |
| F17 fault library has no caller | fixed, ENOLCK skip closed | `b7a8224` |
| F18 witness origin | fixed, seam widened | `69b18a8` |
| F19 four unread limits | two fixed, two deferred with reasons | `69b18a8` |
| F20 `repair` on a local reference | fixed | `69b18a8` |
| F21 cold local fetch writes twice | confirmed, deferred, needs a cloning volume | |
| F22 error kinds with no test | six fixed, `source.identity_changed` needs a human | `b7a8224` |
| F23 `explain` | fixed | `1a59ff8` |
| F24 inert flags | fixed | `1a59ff8` |
| F25 two cache directory functions | fixed | `a5a8e1e` |
| F26 seams hold whole lists | deferred, measure first | |
| F27 user-facing text | fixed | `ad5cd0b`, `1a59ff8` |
| F28 documentation disagreements | fixed | `1a59ff8`, `80c5253`, `a9848c5` |

Twenty-two fixed, six deferred. Of the six, two need a human decision (F14's
plan shape, F22's `source.identity_changed`), two need a later phase (F10's
arbitration, part of F8 and F19), and two need a measurement or a machine this
matrix does not have (F21, F26).

**Phase 6, against this table.** Both human decisions this table lists as owed
got made. F14's plan shape: `c83a6a8` leaves `staging` and `destination` without
a `bytes` field and lists both in `unknown`, and contracts.md:270 was amended in
the same commit. F22's `source.identity_changed`: `f5dc98d` rules that rung two
alone is terminal, ruled by `rung_for` in `crates/engine/src/transfer.rs`, and
the seven-of-seven producing-test set F22 was one short of is now complete. Of
F10, the settings half is closed at `aa19f31` -- `CommandLinePolicy` is now built
by every run in `crates/cli/src/main.rs` rather than by tests alone, and eleven of
its fifteen methods have a production caller -- and the arbitration half this
table already routed to phase 7 is confirmed still open there: there is one
Source implementation until phase 7 ships a second, so nothing exists yet for
Policy to arbitrate between. Twenty-four fixed or closed, four deferred, as of
`f5dc98d`: F8's remaining six events and F19's two limits still wait on phases 7
and 8, F10's arbitration waits on phase 7, and F21 and F26 still wait on a
measurement or a machine this matrix does not have.

Two things were done that no finding asked for. Packing, which the audit's
closing section said the measurement had settled, is at `a9848c5` along with the
one lookup that keeps it one way. Windows on ARM compiles at `80c5253`, which
the audit named as the only unreachable target.

## Found while fixing these

The surface suite that F1 through F4 justified found more than the four it was
written for, and the work that followed found the rest. None of these is in the
findings above.

**A run after `--adopt` reported a tree the destination did not hold.** `--adopt`
records the adopted tree; the next run resolved a different tree, found every
recorded fingerprint still matching, and wrote that different tree into the
receipt as truth. `verify` disagreed with `get` on the same directory. Fixed at
`a5a8e1e`: a recorded fingerprint answers only for the tree the receipt
describes.

**A bare `.gz` materialized its one file under a digest in hexadecimal.** The
archive reader was given the cache object's path as the archive's name. Fixed at
`a5a8e1e`.

**`--no-cache` silently stops extracting archives.** `get ./p.tar.gz --no-cache`
produces a different tree digest with no `degrade`. contracts.md:511 says
`--no-cache` means the partial lives beside the destination and verification is
unchanged, so the fix is a scratch store beside the destination. Not fixed. This
is the most serious of the ones found here: it is a silent determinism failure
under a documented flag, which is the thing this project exists to make
impossible.

**`--threads 0` is silently ignored and `--threads 99999` clamps without
saying.** `NonZeroUsize::new(0)` returns `None`, which reads as "not requested"
rather than as an error, and contracts.md says a ceiling above the detected
budget is clamped and the clamp reported. Fixed at `aa19f31`. The flag's own
`clap::value_parser!(u32).range(1..)` at `crates/cli/src/surface.rs:69` now
rejects `0` before it ever reaches a `NonZeroUsize`, which is a usage error and
exits 2 naming `--threads`. A value above the detected budget still clamps, and
now says so twice: a `degrade` names the requested ceiling and the budget it was
held to, and `explain threads` reports the result as clamped rather than as the
plain number. `crates/cli/tests/tune.rs a_thread_ceiling_of_zero_is_a_usage_error`
and `a_thread_ceiling_above_the_detected_budget_is_clamped_and_says_so` hold both
halves.

**`resource.limit` was produced only where `Processor::new` fails,** which is
effectively unreachable. Now produced by the redirect limit and the listing
limit as well, at `b7a8224` and `69b18a8`.

**`hash_object` opened an object's container and read it whole.** Harmless while
every object had a file of its own, and wrong the moment one did not. Found by
the one-lookup test rather than by a user, and fixed in the same commit that
could have shipped it, `a9848c5`.

**`cache-growth` measured `objects/` and called the answer the cache.** The same
defect as F5, one placement reported as the whole. Fixed at `a9848c5`.

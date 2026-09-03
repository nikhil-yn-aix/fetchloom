# Audit 2

Written at `e5d6f09`, against the tree phase 8 closed at `111a1fe` and the four
commits after it. Everything here was run on this machine unless it says
otherwise: Windows 11 26200, `x86_64-pc-windows-msvc`, 16 processors, NTFS with
Microsoft Defender enabled and not excluded.

No finding in this document was fixed when it was written. Part A of this pass
changed no code at all, which is why the findings can be trusted: the tree they
describe is the tree at `e5d6f09`.

Numbering continues from `audit.md`, which ended at F28, so a reference to F31
is unambiguous across both documents. `audit.md` stays where it is; it is the
measurement the first eight phases were judged against and this one does not
replace it.

## What is different about this pass

The first audit found that the layer between the working parts and a person had
not kept up. That layer is now built: all twelve commands act, every reference
form the grammar names is recognized, all thirty-four contracted events have a
production emitter, and the deterministic counters agree to the byte across
Windows and Linux.

This pass found a different shape of defect, and it is the shape that survives a
green suite and a careful review. Three examples, each measured rather than
argued:

**A run given `--offline` reaches the public internet.** Not for every reference
-- for three of them, and for a reason nobody would find by reading the offline
code, because the offline code is correct. The gate asks whether the reference
the user typed contains `://`. `hf:`, `zenodo:` and a bare name resolving
through configured sources do not. Proven by pointing `HTTPS_PROXY` at a dead
port and watching an `--offline` run fail with `network.refused` naming
`https://huggingface.co/...`. vision.md calls freedom from the network one of
five freedoms that override feature requests. The test that guards it,
`the_seam_is_what_refuses_a_network_reference_offline`, passes, because it uses
an `https://` reference.

**Two deterministic gate metrics are wrong, and the kernel says by how much.**
`bytes_written` reads exactly 1.5 times what the kernel wrote and `bytes_read`
exactly 0.5 times what it read, at every object size measured. The committed
baseline and the published benchmark page both carry the inflated number. This
is the fifth time a work counter has been found blind or doubled in this
project, and the first time one has been checked against the operating system
rather than against another number the same program produced.

**A capability the product advertises has never once worked, for a reason in
four lines of FFI.** Copy-on-write cloning on Windows passes the object's exact
byte length as `ByteCount` to `FSCTL_DUPLICATE_EXTENTS_TO_FILE`, which the
filesystem algorithm specification says MUST fail unless it is a multiple of the
volume cluster size. For an arbitrary object length that is one chance in 4,096.
"Block cloning has never succeeded on any machine in this matrix" has been an
open debt since phase 0 and has been blamed on the absence of a machine every
time it was carried forward. The machine was never the only problem.

The three have one thing in common. Each is guarded by a test that passes, a
document that agrees with itself, and a mechanism that was checked against
another part of the same program rather than against the thing outside it --
the operating system, the wire, the specification. That is what this pass looked
for and it is what the next one should look for again.

---

## Findings

Severity is what happens to a user, not how hard it is to fix.

### F29. `--offline` reaches the network for three reference forms. HIGH

`crates/cli/src/run.rs:1353`:

```rust
let network = reference.contains("://") && !reference.starts_with("file://");
```

`hf:datasets/org/name@rev`, `zenodo:10.5281/zenodo.1234567` and a bare name that
resolves through the configured `sources` list all carry no `://`, so the gate
never fires and the run proceeds to open a socket.

Measured, with `HTTPS_PROXY` and `HTTP_PROXY` pointed at `http://127.0.0.1:1`
so that any attempt to connect is visible as a failure rather than as success:

```
$ fetchloom get hf:datasets/org/name@rev --offline --json
{"kind":"network.refused","layer":"transfer", ...,
 "source":"https://huggingface.co/api/datasets/org/name/tree/rev"}      exit 20

$ fetchloom get zenodo:10.5281/zenodo.1234567 --offline --json
{"kind":"network.refused","layer":"transfer", ...,
 "source":"https://zenodo.org/api/records/1234567"}                      exit 20

$ fetchloom get https://example.invalid/x.tar.gz --offline --json
{"kind":"policy.offline","layer":"policy", ...}                          exit 40
```

Without the proxy the `hf:` run reports `policy.credential_missing` "because
https://huggingface.co/api/datasets/org/name/tree/rev answered 401 without one".
A `401` is a response. The request went out.

A bare name behaves the same way. With one HTTPS base configured and `--offline`
given, the run reports `reference.unresolved` "because silesia matched none of
the 1 configured sources", which is what a failed network attempt looks like
from the inside, and is a false statement about why the run could not answer.

contracts.md Offline: "`--offline` forbids DNS resolution, connection attempts,
source probes, remote credential lookups, and update checks. Any operation
requiring one of these fails with `policy.offline` before doing avoidable work."
vision.md Freedoms: "Freedom from the network. Every operation declares its
network need. Offline means no DNS, no probe, no credential call, no update
check." vision.md says a feature that breaks a freedom does not ship.

The exit code is wrong as well: 20 where the table says 40.

Why nothing caught it: `crates/cli/tests/policy.rs
the_seam_is_what_refuses_a_network_reference_offline` asserts the refusal against
an `https://` reference, which is the one shape the gate handles. Nothing tests
the gate against a provider reference or a bare name, and no test asserts that
an offline run issues zero requests.

Cost to fix: the gate asks the wrong question. It should ask the adapter
registry, which already answers `serves(reference)`, or refuse at the point a
request is issued inside `HttpSource`, which is the only place that cannot be
bypassed by a new reference form. A test per reference shape asserting
`policy.offline` and `requests == 0`.
If never fixed: an air-gapped user, who is one of the four audiences vision.md
names, has no way to know which references are safe to give the tool.

### F30. `bytes_written` over-reports by exactly one copy and `bytes_read` under-reports by exactly one read. HIGH

Measured against the kernel's own per-process I/O counters
(`GetProcessIoCounters`), release binary, one local object per run, three sizes:

| object | kernel wrote | `bytes_written` | kernel read | `bytes_read` | file ops |
|---|---|---|---|---|---|
| 8 MiB | 17,303,527 | 25,165,824 | 16,777,216 | 8,388,608 | 27 |
| 64 MiB | 134,744,046 | 201,326,592 | 134,217,728 | 67,108,864 | 27 |
| 256 MiB | 537,413,565 | 805,322,696 | 536,870,912 | 268,435,456 | 30 |

The ratio is 1.498, 1.494, 1.499 on the write side and 0.500 on the read side at
every size. The counter reports three copies where the operating system performs
two, and one read where it performs two.

Cause. `crates/cache/src/ingest.rs:109-110`:

```rust
self.platform().clone_or_copy(source, &scratch)?;
self.work().wrote_bytes(length);
```

`crates/platform/src/lib.rs:119` `copy_bytes` already calls
`self.work.wrote_bytes(copied)`. On a volume that cannot clone, the same bytes
are counted twice. On a volume that can, a clone that wrote nothing is counted
as a full write, so the number is wrong in both directions rather than merely
inflated. The read that `std::fs::copy` performs is counted nowhere.

The 256 MiB row reproduces `benchmarks/x86_64-pc-windows-msvc.json`'s
`one-large-file` `bytes-written` of 805,322,696 exactly, so the committed
baseline and `docs/benchmarks.md` both carry the inflated figure, and the
five-percent gate is enforced against it.

This also settles F21, which has been carried since the first audit. A cold
local fetch writes the bytes exactly twice, not three times. The third copy is
the counter, not the disk.

contracts.md Output streams: the four counters "are identical on identical
inputs, so they are what a benchmark gates on ... counted where the platform
performs the operation, so a new caller counts by construction." That sentence
is true of `file_operations`, which is incremented only inside the platform and
the cache. It is false of the byte counters, which are incremented at twenty-six
sites across five crates -- eight more than the eighteen F5 counted.

Cost to fix: delete one line, count the copy's read, and add a test that
compares the reported counters against what the platform actually performed. The
fourth instance of this class deserves a mechanical check rather than a fifth
finding.
If never fixed: every performance claim this project publishes rests on a number
that is half again too large.

### F31. Copy-on-write cloning on Windows cannot succeed for almost any object. HIGH

`crates/platform/src/windows/ffi.rs:228-254` `duplicate_extents` builds
`DUPLICATE_EXTENTS_DATA { SourceFileOffset: 0, TargetFileOffset: 0, ByteCount: length }`
with `length` the object's exact, unrounded byte length, and issues
`FSCTL_DUPLICATE_EXTENTS_TO_FILE`. It is called from
`crates/platform/src/windows/mod.rs:264-266` `clone_file`.

The filesystem algorithm specification states without exception that if
`ByteCount` is not a multiple of the volume's cluster size the operation MUST
fail with `STATUS_INVALID_PARAMETER`, and the same rule holds for both offsets.
The conceptual Block Cloning page states it independently: the source and
destination regions must begin and end at a cluster boundary.
`FSCTL_DUPLICATE_EXTENTS_TO_FILE_EX` adds an atomicity flag and relaxes nothing.
ReFS's default cluster size is 4 KiB, unchanged for Dev Drive.

A content-addressed object's length is a multiple of 4,096 about one time in
4,096. Every other object fails the ioctl, and `clone_or_copy`
(`crates/platform/src/lib.rs:316-341`) treats the failure as "this volume does
not clone", remembers the refusal for the whole run, emits one `degrade`, and
byte-copies from then on. The degrade is emitted, so nothing is silent -- but it
names the volume as the cause when the cause is the request.

Two consequences follow. The first is that features.md's "Copy-on-write clones
are used when the filesystem supports them, so materializing from a warm cache
costs metadata rather than a second copy of the data" has never been true on
Windows for an ordinary object. The second is that "block cloning has never
succeeded on any machine in this matrix", carried since the phase 0 gate and
blamed on the lack of a ReFS volume every time, has a second cause that a
machine would not have fixed.

`crates/platform/src/linux/probe.rs:177-188` compounds it: the clone probe
writes exactly 4,096 bytes, which is cluster-aligned, so a volume can answer
`clone: true` while every real object on it fails to clone.

The Linux path is structurally immune. `crates/platform/src/linux/mod.rs:301-319`
uses `ioctl_ficlone`, which reflinks a whole file and takes no offset or length,
so there is no alignment parameter to get wrong.

Cost to fix: clone the cluster-aligned prefix and copy the remainder, which is
under one cluster. The volume's cluster size is already queryable.
Confidence: high on the specification, which is a MUST-fail statement
cross-confirmed by three current Microsoft pages. Not reproduced on a live ReFS
volume, because this machine has none. That is the one step left.
If never fixed: a headline feature is unreachable on one of two platforms, and
the debt that has been carried for eight phases stays carried for the wrong
reason.

### F32. A source error interpolates the raw location beside the redacted one. HIGH

`crates/sources/src/http.rs:565-570`:

```rust
pub(crate) fn transport_failure(location: &str, reason: &ureq::Error) -> Error {
    let (kind, retryable, action) = classify(reason);
    Error::new(kind, format!("{action}: {reason}"))
        .with_source(location)
        .with_retryable(retryable)
}
```

`with_source` redacts. The message does not. `ureq::Error`'s own `Display`
embeds the URI for several variants, and the crate constructs them that way:
`ureq-3.4.0/src/util.rs:330` builds `BadUri(format!("{} is missing scheme", self))`
where `self` is the whole `Uri`, query string included, and
`ureq-3.4.0/src/error.rs:234,263` render `BadUri` and `RequireHttpsOnly` with
that string inline.

`next_action()` reaches stderr, the JSON result and the event stream. A signed
URL whose query carries `X-Amz-Signature`, or a userinfo component, reaches all
three in full while the field one line above reads `[redacted]`.

This is the sixth instance of the exact shape phase 7 found five of and phase 7.5
found a sixth of. The phase 7 record says the fix was to search the whole error
rather than the field that was already right;
`crates/sources/tests/secrets.rs` does that, but only for failures that classify
through branches that never reach `{reason}`'s embedded text, so no test
constructs a syntactically invalid, secret-bearing URL.

Related, in the same file: `crates/sources/src/http.rs:619-623` `unresolvable`
matches on substrings of an `io::Error`'s message. The phase 2 disposition of F12
says the classifier "matches on `ureq::Error` rather than on substrings of its
message". It does, at the top level; one arm still does not.

Cost to fix: stop interpolating a third-party `Display` into a message this
project is responsible for redacting. One test with a sentinel secret in an
invalid query string.
If never fixed: the one thing this project promises never to write down is
written down, on a path a test cannot reach today.

### F33. SigV4 canonical URI and query string are not built to specification. HIGH

`crates/sources/src/http.rs:174-185` `path_and_query` slices the path and the
query verbatim out of the request URL and hands them to
`crates/sources/src/signing.rs:127-141` `canonical_request`, which uses them
unmodified.

The specification requires every URI byte outside the unreserved set to be
percent-encoded, and query parameters to be sorted by name in byte order before
being joined. Neither happens. Everything else in the implementation is correct
and was checked against the published worked example and the HMAC test vectors:
the signing key derivation chain, the string to sign, the canonical headers, the
signed headers list, the empty-payload sentinel, and the authorization header
assembly.

Latent today, because the only signed requests this build issues are plain
object `GET` and `HEAD` with no query string. It stops being latent the first
time a signed request carries two query parameters, or a path with a byte
outside the unreserved set. The failure is a `SignatureDoesNotMatch` the user
cannot act on.

Why nothing caught it: `crates/sources/src/signing.rs:412` is the only test that
comes near, and it uses one query parameter, which is trivially sorted against
nothing.

Cost to fix: percent-encode the path and each query key and value, sort the
pairs, and add a test with at least two parameters given in reverse order.

### F34. A directory index keeps links that point outside the prefix, and fabricates their locations. HIGH

`crates/sources/src/index.rs:128-142` `relative` rejects a target beginning
`../` and a target beginning `http`, and nothing else. A root-relative link
survives:

For a listing under prefix `/set/` and `href="/other/thing"`, neither
`strip_prefix("/set/")` nor `strip_prefix("set/")` matches, so `unwrap_or`
returns the target unchanged, `trim_start_matches('/')` makes it `other/thing`,
it does not begin `http`, and it is kept. `index.rs:106-110` then builds its
location as `format!("{location}{path}")`, which is
`http://host/set/` + `other/thing` -- a URL that does not correspond to the link
at all. A protocol-relative `//elsewhere/x` fails identically.

contracts.md Directory listing: "Only entries at or below the given prefix are
considered. Links pointing outside the prefix are ignored and counted in the
result." Both halves are broken: the link is not ignored, it is not counted, and
the entry it becomes names a location nothing serves.

The two tests present (`index.rs:152-187`) use a `../` link and an in-prefix
relative link. Neither is site-absolute.

Cost to fix: after the strip attempts, refuse and count anything still beginning
`/`. One test per link shape.

### F35. `--color` and `NO_COLOR` are honored nowhere. HIGH

`crates/cli/src/terminal.rs:122` `resolve_color` is fully implemented and unit
tested and has no caller outside its own test module. `GlobalFlags.color`
(`crates/cli/src/surface.rs:79`) is read by no file in `crates/cli/src`; it is
the only global flag with zero readers. No SGR escape is emitted anywhere in the
crate.

Measured: `get nope --color always` and `cache status --color always` emit no
escape byte at all.

contracts.md Presentation: "One accent color ... `NO_COLOR` and `--color` are
honored, and all styling is stripped when the stream is not a terminal."
contracts.md Command surface: "There is no state in which something is present
and unable to act, because that is a placeholder, and because a caller cannot
distinguish it from a usage error."

F24 in the first audit was this exact rule, and its disposition says it was fixed
at `1a59ff8` by scoping flag groups per command. A flag scoped to the right
command and then never read is the same defect one level down.

`anstream` and `anstyle` are declared in `[workspace.dependencies]` and used by
no member crate.

Cost to fix: call `resolve_color` from the composition root and thread the answer
into the renderer, which is what the contract states. A test that `--color never`
and `NO_COLOR` strip and `--color always` does not.

### F36. Two configuration keys are parsed and thrown away. HIGH

`crates/cli/src/config.rs:104-107` declares `color` and `hints`.
`crates/cli/src/settings.rs` resolves neither and `Settings` has no field for
either. `terminal::hints_permitted` is fed from the `--no-hints` flag alone
(`crates/cli/src/main.rs:184`). Measured: `explain` reports fourteen settings and
neither of these is among them.

Three documents state otherwise. contracts.md:992: "`--no-hints` and the
equivalent config value disable them permanently." docs/reference/commands.md:538
repeats it. features.md:155: "Every one of those keys is read, and a key this
build does not act on is refused rather than accepted and ignored." All three are
false, and the third is false in the specific way it was written to prevent.

Why nothing caught it: `crates/cli/tests/precedence.rs:218
every_key_the_contract_names_is_read` asserts only that `config::read` does not
error on each key. For the other four keys in the same test the value is wired;
for these two it is not, and the test's name is what hid it.

Cost to fix: wire both, which is what the contract states, and change the test to
assert the value takes effect rather than that the file parses.

### F37. The one place that decides a filesystem failure's kind is one of five. HIGH

F11's disposition says the catch-all was fixed with "one decider,
`error::filesystem_failure`, plus `error::lock_failure` ... A test walks both
crates and fails on any function that turns an `io::Error` into an `Error`."
Three of those clauses do not hold.

The test walks two crates of eight
(`crates/engine/tests/filesystem_failure.rs:85`, `for crate_name in ["cache", "platform"]`).

The test matches text, not types. `filesystem_failure.rs:96-100` flags a line
only when it begins `fn`/`pub fn`/`pub(crate) fn` and contains both the literal
`io::Error` and the literal `-> Error`. A failure built inside a `.map_err`
closure evades it because the closure is not a declaration. A helper whose
parameter is `rustix::io::Errno` rather than `std::io::Error` evades it because
the match is on the substring.

There are at least four other deciders. `crates/platform/src/linux/mod.rs:27`
`from_errno` takes the kind as a parameter and is used at four sites.
`crates/cli/src/run.rs:180` `failure` takes the kind as a parameter.
`crates/cli/src/materialize.rs:98` `read_failure` always renders "make {path}
readable", including at `materialize.rs:181,198` where the path was opened for
writing, so a full disk on the destination tells the user to make it readable.
Windows does the same thing inline rather than through a helper at
`crates/platform/src/windows/mod.rs:163,181,311,331`.

Cost to fix: route the remaining sites, widen the walk to every crate, and match
on the conversion rather than on the declaration line. The walk is the part that
matters, because it is the part that was believed to be holding.

### F38. The packed index is cloned in full on every lookup. HIGH

`crates/cache/src/pack.rs:193-209` `packed_index` memoizes the index and then
returns it **by value**:

```rust
if let Some(built) = held.as_ref() {
    return built.clone();
}
```

`Index` is `BTreeMap<ContentDigest, (PathBuf, Entry)>`.
`crates/cache/src/storage.rs:127` `placement` calls it on every invocation, and
`holds`, `size_of`, `read` and `fingerprint_of` all route through `placement`,
which contracts.md requires ("Exactly one lookup answers where an object is, and
every reader goes through it"). So every cache hit clones the whole index,
including a heap allocation for each entry's `PathBuf`.

`crates/cache/src/store.rs:513` `status` calls `list()` and then `size_of` for
every digest, so one `cache status` on a cache of M packed objects performs M
full clones of an M-entry map. That is O(M squared) allocations for a command
whose job is to count.

The same shape reaches `prune` (`crates/cache/src/prune.rs:26`) and
`rebuild` (`crates/cache/src/rebuild.rs:43`), both of which call a per-digest
operation in a loop.

standards.md Memory: "Buffers are allocated once and reused. No allocation inside
a per-chunk or per-entry loop."

F26 deferred the whole-list question on the instruction to measure first, and the
measurement was never taken because it was framed as needing a million objects.
The arithmetic does not need them: at a thousand objects this is a million
entry-clones per `status`, which is survivable and easy to miss; at a million it
is on the order of a trillion, which is not a slow command, it is a command that
does not return.

Cost to fix: hold the index behind an `Arc` and clone the handle, or do the
lookup under the lock without materializing the map. Contained to two files.
This is what should close F26 rather than a million-object corpus.

### F39. Windows never gives the scanner answer the contract defines for it. HIGH

contracts.md Platform capabilities gives `absent` two conditions: "The platform
enumerated and none is a scanner, **or** the platform cannot enumerate and small
writes cost no more than the ratio."

`crates/platform/src/linux/probe.rs:271-274` implements both.
`crates/platform/src/windows/probe.rs:198-200` implements only the first: when
`loaded_minifilters()` returns nothing, which its own note at `probe.rs:67` says
is what happens without elevation, it returns `Unknown` whatever the measured
ratio is. `SMALL_WRITE_COST_RATIO` is not imported into the Windows file at all.

So an ordinary unelevated Windows process on a volume with no scanner reports
`unknown` and emits a `degrade`, where the same situation on Linux correctly
reports `absent`. The divergence is not a platform difference; it is a missing
branch.

Today this changes no behavior, because `--io auto` also requires
`CAN_RELEASE_PAGES`, which is Linux-only, so `auto` would choose `buffered` on
Windows regardless. It changes what `doctor` and every plan and result say about
the volume, and it emits a degradation that is not true.

Cost to fix: mirror the Linux fallback. Four lines.

### F40. A zip written by a streaming writer is refused as tampered with. MEDIUM

`crates/archive/src/zip_reader.rs:158-192` `read_local_header` never reads the
general-purpose bit flag, so it cannot see bit 3, which says the local header's
CRC and sizes are zero and the real values follow the data in a descriptor.
`zip_reader.rs:271-301` `agrees_with_central_directory` then compares those zeros
against the central directory and reports a disagreement.

The message tells the user the local header disagrees with the central directory,
which reads as evidence of tampering. It is this build not accounting for a
flag that any writer producing a zip to a non-seekable stream sets.

Related, same file: neither `central_directory_names` nor `read_local_header`
handles the ZIP64 sentinels, so a zip needing ZIP64 is misread rather than
refused by name. `crates/faults/src/archives.rs:1486` exercises a ZIP64 record
whose legacy fields remain independently valid, which is the one ZIP64 case that
does not need the sentinels, so the corpus cannot catch it either.

Cost to fix: read the flag word and skip the size comparison when bit 3 is set,
keeping the method comparison; detect the ZIP64 sentinels and refuse by name.

### F41. `plan` reports `[` as the host of an IPv6 reference. MEDIUM

`crates/cli/src/planning.rs:183-193` `host_of` hand-rolls host extraction and has
no branch for a bracketed literal, so `https://[::1]:8080/x` yields the single
character `[`. `crates/engine/src/reference.rs:87-102` `Host::of_location` has
that branch and is what `crates/cli/src/run.rs:363` calls.

The phase 7.5 record names this exact defect as one concurrency exposed: "Every
measurement, credential and in-flight count for a bracketed IPv6 host had been
filed under `[` since phase 6." It was fixed in `Host::of_location` and the copy
in `planning.rs` was missed.

`host_of` feeds `Plan.network.hosts` (`planning.rs:90`), which is a field of a
portable artifact and is what tells a reader which hosts an apply will contact.
The many-hosts benchmark regime serves half its objects from `[::1]`.

Cost to fix: delete the copy and call the one that is right. standards.md: "Two
ways to do the same thing is a defect. Delete one."

### F42. The idle-connection lifetime the decision record chose was never set. MEDIUM

decisions.md:2162 chooses "one `ureq` agent per host, holding that host's pool,
with keep-alive on and no idle connection kept past sixty seconds", and gives the
reason at :2172: "Sixty seconds of idle is the same number as the retry ceiling,
and it is chosen so that a resumed transfer after the longest permitted wait
still finds its connection rather than paying a handshake."

`crates/sources/src/http.rs:229-244` `build_agent` sets five things and does not
set `max_idle_age`. The client's default is fifteen seconds
(`ureq-3.4.0/src/config.rs:887`). The retry policy produces waits of sixteen,
thirty-two and sixty seconds, so every retry at attempt five or six finds the
connection evicted, which is the case the decision was written to prevent.

The pool shape itself is correct: one `Agent` per host, cached for the run
(`http.rs:52-56,71-78`), one pool inside each. TLS resumption works, and works
because rustls resumes by default and ureq caches the config per agent, not
because anything here asked for it.

Cost to fix: one line. Worth recording because a decision record that names a
number is the strongest form of intent this project has, and the number reached
no code.

### F43. The connect path is sequential, so a dead IPv6 route costs seconds. MEDIUM

`ureq-3.4.0/src/unversioned/transport/tcp.rs:52-124` tries resolved addresses one
at a time in resolver order, splitting the connect budget across them
geometrically. A refused connection moves on at once; a blackholed one burns its
whole share. This is not Happy Eyeballs: there is no racing and no preference
delay.

The project met this once. A regime reached half its objects by the name
`localhost`, which resolves to `::1` first on Windows, against a server bound
only to `127.0.0.1`, and sixteen objects took 17,958 ms instead of 831 ms. The
fix at `04785b9` gave both host keys a literal loopback address, which removes
the resolver from that test and leaves the mechanism in place. A real dual-stack
host with a broken IPv6 route -- a dead tunnel, a misconfigured ISP -- still pays
up to the connect timeout before falling back.

`crates/sources/src/http.rs:229-244` never sets `ip_family`, so the order is
whatever the platform resolver returns.

Cost to fix: not small, and not obviously this project's to fix, since the
behavior is the client's. Worth stating because the fix that was applied is a
test fix and was recorded as if it were the fix.

### F44. The shipped help record for Amazon S3 contradicts the shipped adapter. MEDIUM

`crates/sources/src/help.rs:104-142` tells the user that S3 "authenticates a
request by signing it with an access key and a secret key, which is not a token
that can be sent, and this build sends a token", that "A private Amazon S3 bucket
cannot be fetched by name here", and that placement is "None. There is nowhere to
put an access key and secret key that this build would use."

`crates/sources/src/signing.rs` is a working SigV4 implementation,
`help.rs:206-209` flags `amazonaws.com` as a signing host, and
`crates/cli/src/policy.rs:159-217` resolves `FETCHLOOM_ACCESS_KEY_<HOST>`,
`FETCHLOOM_SECRET_KEY_<HOST>`, `FETCHLOOM_REGION_<HOST>` and the `AWS_` variables
into signing keys that `http.rs:190-227` signs with.

contracts.md Provider help records: "Every provider ships one fixed record, and
Fetchloom never improvises the wording." The record is fixed and wrong, which is
worse than improvised: it tells a user with a private bucket to go and do
something else.

Cost to fix: rewrite the record. No code.

### F45. Three lint expectations are unfulfilled, and one of them is an orphaned docstring. LOW

`cargo clippy --workspace --all-targets` warns three times at `e5d6f09`:
`crates/cli/src/run.rs:1507`, `crates/cli/tests/interface.rs:5`,
`crates/cli/tests/diagnose.rs:6`.

The first is the interesting one. `crates/cli/src/run.rs:1497-1520` carries a
docstring describing `materialize_remote_container` -- it names a destination,
`force` and `adopt` -- then an `#[expect(clippy::too_many_arguments)]`, then a
second docstring describing `infer_remote`, and then `infer_remote`, which takes
four arguments and none of those flags. Rust attaches all of it to
`infer_remote`, so its published documentation describes a different function's
contract and it carries a suppression for a lint it never triggers.
`materialize_remote_container` has its own correct `#[expect]` thirty lines
below.

`cargo xtask verify`'s lint step does not pass `-D warnings`, so none of the
three fails it.

Cost to fix: delete the orphan and the two dead lint allowances.

### F46. Two stray files are committed at the repository root. LOW

`fetchloom.lock` is a real lock pinning a dataset named `x.txt`, left behind by a
run in the working directory. `ith.tuning.controller(host, Some(cache));` with two
trailing fullwidth vertical bars is 242 bytes of `grep` output from a botched
shell redirect. Both are tracked, which is why `git status` reports the tree
clean.

Cost to fix: `git rm`.

### F47. Eleven docstrings carry rationale, and nothing checks. LOW

standards.md Docstrings: "A docstring is one sentence saying what the item is,
plus an `# Errors` section when the item can fail. Nothing else. It never
restates the signature, and it never carries history, rationale, or an example.
Rationale belongs in decisions.md."

Eleven docstrings carry a second paragraph beginning with a justification:
`crates/cache/src/measurement.rs:32`, `crates/cache/src/pack.rs:12`,
`crates/cache/src/pack.rs:188`, `crates/cli/src/doctor.rs:103`,
`crates/cli/src/hint.rs:113`, `crates/engine/src/adapter.rs:54`,
`crates/engine/src/flights.rs:56`, `crates/engine/src/metadata/mod.rs:54`,
`crates/engine/src/seam/source.rs:47`, `crates/engine/src/transfer.rs:295`,
`crates/engine/src/transfer.rs:585`. Several more carry an extra paragraph after
an `# Errors` heading, including `crates/platform/src/lib.rs:525` and
`crates/engine/src/tuning.rs:311`.

`xtask/src/comments.rs` enforces the comment rules -- no comment that is not a
docstring, no block comment, no banner, no decoration, no `SAFETY:` line over
safe code -- and checks nothing about a docstring's shape. Phase 6 cut 1,657
lines of exactly this and rewrote standards.md so it would not grow back. It grew
back, because the rewrite added a rule and no check.

Two smaller gaps in the same checker: `xtask/src/comments.rs:140` permits two
stacked `SAFETY:` lines over one `unsafe` block where standards.md says a single
line, and `:213` decides a line opens an unsafe block by looking for the
substring `unsafe` anywhere in it.

Cost to fix: a rule in the checker. It is the checker's job.

### F48. The live view's isolation test can be defeated by a grouped import. LOW

`crates/view/tests/isolation.rs:17-26` forbids the substrings `std::fs`,
`std::net`, `std::process`, `std::env`, `File::open`, `File::create`,
`TcpStream` and `include_str!` in the crate's own source text.
`use std::{fs, net};` contains none of them, and `fs::read` afterwards contains
none of them either.

The phase 8 gate calls this the answer to the thing worth watching for: "It is
not 'inspected the code and it only uses events'." The dependency half of the
claim is sound -- `isolation.rs:52-75` reads the crate's own manifest and refuses
any dependency outside one permitted name -- and that half genuinely enforces
the property for anything outside the standard library. The substring half is the
only guard against the standard library, and it is a substring match.

Cost to fix: forbid the bare segments as well, or parse.

### F49. Bomb limits are enforced by three implementations that disagree. LOW

`crates/archive/src/bomb.rs:78-141` `BombGuard` counts entries, expanded bytes
and the expansion ratio, and runs during listing.
`crates/archive/src/resolve.rs:347-388` `Counted` counts entries and bytes and not
the ratio, and runs while hashing member bodies.
`crates/archive/src/extract.rs:26-68` `WriteGuard` counts entries and bytes and
not the ratio, and runs while writing to staging. All three have near-identical
bodies and near-identical messages.

The asymmetry is a consequence of the triplication rather than a decision:
the ratio is checked once, at listing, and not again on either path that actually
moves bytes.

Cost to fix: one guard with the byte-only entry point the other two need.

---

## The instrument

Phase 6's performance half has stayed open across three phases. This section is
about why, and it is separate from the findings above because the harness is not
the product.

### F50. The timing gate standards.md describes does not exist. HIGH

standards.md Measure: "Timing metrics are a property of the machine as much as
the code. They gate only under `cargo xtask verify`, against a baseline recorded
by that command on that same machine for that same target, and at a wider band
than a deterministic metric."

`xtask/src/bench.rs:265`, inside `compare`:

```rust
if metric.kind == MetricKind::Timing {
    continue;
}
```

That skip is unconditional on every path. `xtask/src/main.rs:241` says so in its
own success line: "a timing metric is recorded and reported and never gates,
because no machine here is quiet enough for a wall clock to mean anything."
`gate_timing`, threaded from `FETCHLOOM_VERIFY`, controls only whether timing is
*recorded*, never whether it gates, and the committed baseline carries no timing
metric at all, so nothing is recorded either.

The code's position is defensible and it is the opposite of what standards.md
says. One of the two is wrong and they have disagreed since phase 6.

### F51. The deterministic gate only fails upward. HIGH

`xtask/src/bench.rs:278`:

```rust
if metric.value > previous.value * (1.0 + REGRESSION_GATE) {
```

A metric that halves passes. A metric the run stops producing passes, because
`compare` iterates the current run's metrics and looks each up in the baseline,
so a metric present in the baseline and absent from the run is never examined.

Every counter defect this project has found presents as a drop. The phase 3
record: a counter blind to a write. The phase 5 record: "the second time a
counter has been introduced and immediately been found blind to the thing it was
introduced for." F5: 5,138 on one path and 1 on another. F6: zero where a copy
happened. F30 above: half the reads. The gate is built to catch the one direction
this project's defects do not take.

### F52. The no-op regime measures a different command than the one it is defined as. MEDIUM

standards.md Measure: "The no-op regime is a locked run against an unchanged
destination. It measures startup, configuration discovery, and reconciliation,
and it bounds how much a dependency may cost simply by existing."

`xtask/src/bench.rs:188` runs `fetchloom explain`.

decisions.md:993 chose that deliberately and said why: "Reconciliation is named in
the standards as part of this regime and joins it when a locked run against an
unchanged destination exists to measure, which needs the lock that phase four
writes." Phase 4 closed four phases ago. Nothing joined.

### F53. Peak resident memory was promised by the harness and is measured nowhere. MEDIUM

decisions.md:666: "Chosen: wall time and binary size now. Peak resident memory and
the byte counters are added when the `platform` crate exists and exposes them."
The byte counters arrived. Resident memory did not, and
`crates/engine/src/limits.rs:28` `resident_memory` is read by no file in the tree,
which is the open half of F19.

This pass measured it from outside the process instead, and the answer is good
news: peak working set is flat at 8.8 to 9.0 MB for a local `get` of an 8, 64 or
256 MiB object. Streaming holds on that path. It is the first resident-set number
this project has, and it took a nineteen-line script.

### F54. Six of eight regimes cannot exercise what phase 6 is gated on. HIGH

This is not new. standards.md:137 states it plainly and the phase 6, 7.5 and 8
gate records each carry it forward. It is recorded here as a finding rather than
as inherited context because it is the reason phase 6's exit criterion has never
been provable, and because two of the three things needed to fix it turn out to
be cheaper than the records assumed.

`no-op`, `cold-cache`, `warm-cache`, `many-small-files` and `one-large-file`
issue zero requests. `cold-transfer` issues two. `interrupted-transfer` issues six
and is dominated by retry backoff -- it agrees to 0.05 ms across two operating
systems, which is the signature of a regime measuring a sleep. `many-hosts`
issues forty and is the only one that exercises the controller, and half its
servers answer with a rate limit, so a host asking to be left alone runs at one
transfer in flight whatever the ceiling says. That is why a per-host ceiling of
one differs from four by fourteen percent rather than by the factor the mechanism
should give.

standards.md names a slow disk and a constrained network as regimes and says the
first "cannot be produced on a machine in the verification lane without a fault
injector this build does not have". The build does have one:
`crates/faults/src/platform.rs` `FaultyPlatform` gates every `Platform` method
through `crates/faults/src/schedule.rs` `Faults::check`. What it lacks is a
*delay*: `Faults` can schedule a failure and nothing else. The fault server
already has exactly the missing shape, `Latency`, which is what makes the
constrained-network regime buildable and is already used by `many-hosts`. A slow
disk is one method on `Faults` away, not a machine away.

`Operation` also has no `FreeSpace` variant, so `free_space` is gated on
`Operation::VolumeId` (`crates/faults/src/platform.rs:56`) and cannot be faulted
apart from it.

### F55. A deterministic metric is measured three times and only the last is kept. LOW

`xtask/src/bench.rs:376,386` and the equivalents in `run_cache` and
`run_transfer` overwrite the work counters each round. Nothing asserts the rounds
agreed. A metric that quietly stopped being deterministic would report its last
value rather than failing, and "identical on identical inputs" is the entire
justification for gating on it.

---

## Tests, against FIRST

814 test functions in 77 files, plus test modules in 18 source files.

### Self-validating

F56. HIGH. `every_error_kind_is_reachable_and_carries_the_required_fields`
(`crates/engine/tests/contracts.rs:102`) constructs each error by hand with
`Error::new` and asserts the builder returns what was put in. It cannot fail for
the reachability its name claims, and it exercises no code path. F22 said this
and the test is unchanged.

F57. MEDIUM. `every_key_the_contract_names_is_read`
(`crates/cli/tests/precedence.rs:218`) asserts parseability and calls it reading.
Two of its six keys are not read. See F36.

F58. HIGH. `no_other_place_turns_a_filesystem_failure_into_an_error`
(`crates/engine/tests/filesystem_failure.rs:78`) cannot catch the class it is
named for. See F37.

F59. MEDIUM. Nothing asserts that a real run emits each contracted event.
`every_event_name_the_contract_lists_is_produced_by_a_payload`
(`crates/engine/tests/contracts.rs:127`) asserts that the enum covers the
contract's names, which is precisely what F8 found passing while ten of them were
emitted by no production code. `crates/cli/tests/stream.rs` reads a real run's
stream and asserts properties of it, but never that the union across the suite
covers the thirty-four.

F60. MEDIUM. Two error kinds have no test that produces them: `network.timeout`
and `cache.locked`. Both appear in the test suites only inside the label table at
`crates/engine/tests/contracts.rs:63,81`. `network.timeout` is produced by
`a_stalled_connection_exits_twenty_rather_than_hanging`
(`crates/cli/tests/adversarial.rs:228`), which asserts the layer and the exit code
and not the kind. standards.md Tests: "Every error kind has a test that produces
it."

### Independent

F61. HIGH. Sixteen of the twenty-five test files that spawn the binary clear no
environment variable at all, and only two of the twenty-five ever pass
`--no-config`. `FETCHLOOM_LOG`, `FETCHLOOM_OFFLINE`, `FETCHLOOM_CONCURRENCY`,
`FETCHLOOM_PER_HOST`, `FETCHLOOM_THREADS`, `FETCHLOOM_CACHE_DIR` and a user
configuration file at the platform's own configuration location all change what
those tests measure. `concurrent.rs`, `tune.rs`, `surface.rs`, `probe.rs`,
`inference.rs`, `display.rs`, `diagnose.rs` and `interface.rs` clear some; the
rest clear none.

F62. HIGH. `eight_artifacts_behind_a_charged_wait_finish_in_far_less_than_eight_one_at_a_time`
and `a_per_host_ceiling_of_one_takes_measurably_longer_than_a_ceiling_of_four`
(`crates/cli/tests/concurrent.rs:141,154`) are wall-clock ratio assertions that
compete with every other test binary. The phase 8 gate names the first. Both are
the same defect.

F63. MEDIUM. `crates/cli/tests/cancel.rs:214,323` assert a run finished within two
seconds. That is a contracted bound, so the assertion is legitimate; under
whole-suite load it is a measurement of the machine.

### Repeatable

F64. MEDIUM. `a_name_no_resolver_knows_exits_twenty_and_is_not_retried`
(`crates/cli/tests/adversarial.rs:259`) resolves
`fetchloom-no-such-host.invalid` through the machine's own resolver.
standards.md Tests: "No test reaches the public network. Sources are served by a
local test harness that can be told to misbehave."

F65. MEDIUM. `xtask/src/network.rs:22,28,34` fetches three pinned tarballs from
`ftp.gnu.org` and `files.pythonhosted.org`, and the lane runs inside
`cargo xtask verify`. It fails safe -- an unreachable host degrades to a skip and
the matrix records a `degrade` naming what was wanted -- so it never produces a
false failure. It is still the one input to the verification matrix that is not
reproducible from the repository.

F66. LOW. standards.md Tests: "No test sleeps. Wait on a condition or a channel."
`crates/cli/tests/cancel.rs:134,258` poll with `thread::sleep`;
`crates/engine/tests/flights.rs:66,99,121,144` and
`crates/cli/tests/transfer.rs:723` sleep to represent work. The second kind is
arguably the product's shape rather than the test's, and the first kind is the
banned form exactly.

### Fast

F67. MEDIUM. Nine names for one idea. `Workspace` in six files, `Run` in four,
`Scene` in three, plus `Subject`, `Ground`, `Snapshot`, `Hostile`, `Harness` and
`Reported`. Each builds a temporary directory, spawns the release binary, and
captures stdout, stderr and an exit code. `crates/cli/tests/support/` exists and
holds a fake environment and a policy. This is both why the suite is slow and why
environment isolation varies file by file (F61): there is no one place to fix it.

### Timely

F68. LOW. `crates/cache/tests/support/mod.rs:151-178`
`packed_digests_are_their_bytes` re-implements the pack layout with a hardcoded
header offset of 72 rather than calling the cache's own reader, so it asserts an
implementation detail and will stop catching format regressions silently if the
header changes.

---

## Documents against the code

### F69. Five sentences in features.md are false. HIGH

features.md's own convention, at line 5: "Every sentence here describes the build
as it stands, except where it is followed by **Not built.** ... A reader can take
an unmarked sentence as something the binary does today." The phase 7.5 and phase
8 records both say nothing enforces that mechanically and that the next audit
should look again. It did.

- Line 9, "Every form is recognized and named back to the user. Every form
  resolves." False for `zenodo:`. See F70.
- Line 147, hints are "disabled by `--no-hints` or the `hints` setting". The
  `hints` setting does nothing. See F36.
- Line 155, "Every one of those keys is read, and a key this build does not act
  on is refused rather than accepted and ignored." False for `color` and
  `hints`. See F36.
- Line 187, every degradation "emits an event and appears in the result".
  `RunResult` (`crates/cli/src/run.rs:65-89`) has no degradation field, and a
  measured `--json` result carries none.
- Line 191, performance claims come "only from repeatable tests covering cold
  cache, warm cache, interrupted transfer, many files, large files, slow disks,
  and constrained networks". There is no slow-disk regime and no
  constrained-network regime. standards.md:137 admits this; features.md does not.

Line 117, on copy-on-write clones, is false on Windows for the reason in F31, but
that is a defect in the code rather than in the sentence.

### F70. contracts.md's own `zenodo:` example does not resolve. HIGH

contracts.md:83 gives `zenodo:10.5281/zenodo.1234567` as the provider form.
Measured:

```
$ fetchloom get zenodo:10.5281/zenodo.1234567 --json
{"kind":"reference.unresolved", ...,
 "source":"zenodo:10.5281/zenodo.1234567article.pdf",
 "next_action":"write it as zenodo:10.5281/zenodo.1234567, because zenodo: names
 zenodo:10.5281/zenodo.1234567article.pdf and this build cannot tell what that is"}
```

A listed entry's path is concatenated onto the container reference with no
separator, and the message then instructs the user to write what they already
wrote. `ZenodoSource::record_of` parses the reference correctly and has a passing
unit test at `crates/sources/src/provider.rs:548`; the join is what is wrong, and
it is the same missing-separator shape as F34.

### F71. standards.md and contracts.md disagree about what a log line is. MEDIUM

contracts.md:783: a log level "decides which of the events the run already emits
are rendered to standard error as human lines."
standards.md Observability: "Structured logging only. Fields, never formatted
sentences."
`crates/cli/src/logging.rs:130-142` serializes each admitted event to JSON and
writes it to stderr. Measured on a real run, stderr carries
`{"seq":0,"timestamp":...,"event":"run.start"}`.

The code follows standards.md. contracts.md is the defect.

### F72. roadmap.md does not contain a phase that ran and closed. MEDIUM

decisions.md:7578 holds a phase 7.5 gate record. roadmap.md names no phase 7.5
anywhere. contracts.md:725 says "The roadmap says which phase delivers each one",
and for the concurrency work, the probe phase, ranged splitting and the `serves`
dispatch, it does not.

roadmap.md:119 still says the published suite covers "all seven regimes the
harness runs". There are eight. F28's disposition says this was fixed;
standards.md was fixed and roadmap.md was not.

### F73. Twelve line references in the documents point at the wrong lines. LOW

Checked by reading the line each names.

Wrong: contracts.md:265 and :270, cited for "a field is never estimated into a
number", which is at :288. contracts.md:511, cited for `--no-cache`, which is at
:557. contracts.md:516, cited for the `cache clear` exemption, which is at :571.
contracts.md:836, cited for the fixed scoring order, which is at :910.
contracts.md:923, cited for the offline refusal, which is at :1150. Every phase
7.5 citation of features.md -- :49, :51, :63, :163, :165 -- is two lines low.
decisions.md:6650 cites `crates/engine/src/credential.rs:22`, which is a derive.
decisions.md:8003 cites `crates/cli/src/run.rs:2599`, which is a scheduler call.

Correct: roadmap.md:123, :129, :131, :135, :141, :150; standards.md:19, :69,
:137; contracts.md:13, :113, :121, :286, :368, :424, :518, :701, :761, :766,
:768, :803, :1002.

### F74. docs/reference/ carries six transcripts the binary no longer produces. LOW

`cache.md:253`, `commands.md:45`, `commands.md:236`, `files.md:51`,
`getting-started.md:40` and `getting-started.md:83` show `--json` results with no
`trust` field. `RunResult.trust` is not skipped on serialization and every
measured result carries it. The phase 8 gate record notes that the reference had
said the JSON result carries no trust class and that this was corrected; the prose
was corrected and the transcripts were not.

`commands.md:538` states that `hints = false` turns hints off. See F36.

### F75. build-protocol.md describes a three-platform project with continuous integration. LOW

macOS was removed end to end at `9dc18dd`. Continuous integration stopped after
`9efb4a4` and the phase 1 gate says it is not returning; `cargo xtask verify` is
what replaced it. build-protocol.md still says "Continuous integration for three
platforms" in the R1 description, "continuous integration green on all six
targets" in B0, and asks the judge "Did it run green on all three platforms?"

docs/README.md indexes neither `docs/reference/`, `docs/benchmarks.md` nor
`docs/decisions.md`.

CLAUDE.md instructs using Context7 for library documentation. No Context7 server
is configured in this environment; the only configured server is `ide`, and it
failed to connect. Every library claim in this pass was verified against the
vendored crate source and the publisher's current pages instead.

### F76. audit.md is stale in one place. LOW

audit.md:1425-1431 records that `--no-cache` silently stops extracting archives
and says "Not fixed. This is the most serious of the ones found here."

Measured today on the same shape, a `tar.gz` holding three entries:

```
cached    tree blake3:0620f4...  entries 3  work{read 546, written 451, ops 28}
--no-cache tree blake3:0620f4... entries 3  work{read 546, written 451, ops 28}
```

Identical. It was closed by the scratch store contracts.md:557-567 describes.
audit.md should say so, in the place it says the opposite.

---

## Structure

### F77. Two files hold 8 percent of the codebase and at least twelve reasons to change. HIGH

`crates/cli/src/run.rs` is 3,217 lines and `crates/cli/src/main.rs` is 1,966.
Together 5,183 of 65,221.

`run.rs` holds, with the line ranges each occupies: reference, host and path
naming (93-190, 360-378, 1488-1518, 2079-2119, 2438-2458); local tree
materialization and destination reconciliation (379-946); verification and
rehashing (948-1172); selection (1173-1320); the terms and offline gates
(1321-1375); single remote object materialization (1376-1487); object store
container materialization (1518-1864); cached object materialization
(1865-2093); archive recognition and extraction (2120-2312); manifest synthesis
and receipts (2313-2437); multi-artifact dataset orchestration (2503-3158); and
trust bookkeeping (3159-3217).

standards.md:69: "One reason to change per module. Transfer does not know about
archives. Extraction does not know about HTTP. The cache does not know what a
dataset is." Between crates that still holds, and it was checked literally:
`crates/archive` names no HTTP type and depends on no source crate;
`crates/sources` names no archive type; `crates/cache` names no dataset,
manifest or reference type, and its one piece of source-shaped knowledge,
`resolution.rs`, is assigned to the cache by contracts.md's own layout. Inside
`run.rs` it does not hold at all: `publish_one_object` (archives) sits beside
`materialize_remote_container` (HTTP) beside `materialize_cached` (the cache)
beside `publish_dataset` (datasets), sharing private helpers that no seam
exposes. The rule is enforced by the reader's discipline and by nothing else.

`main.rs`'s own module docstring calls it "The composition root: the only place
the seams are wired together". `resolve_reference` (1262-1320) is reference
resolution logic, duplicating the territory `crates/cli/src/resolve.rs` exists to
hold; `run_get` (583-698), `run_init` (824-929), `run_plan` (1007-1106) and
`run_apply` (1107-1206) are hundred-line command implementations with their own
control flow. A change to `get`'s reconciliation does not need to touch
`apply`'s plan reading, and today both are edited in the same file.

decisions.md:97 makes `cli` the only composition root deliberately, and
decisions.md:101 rightly forbids inventing a crate to make a diagram look right.
Neither bounds the size of a file inside that crate, and the fix needs no new
crate: modules under `crates/cli/src/run/` split by what is materialized --
local, remote, container, cached, archive, dataset -- with verification and
selection beside them, and each command body moved next to the module that owns
that command's other logic.

For contrast, the seams themselves are in good order: the six files under
`crates/engine/src/seam/` total 849 lines and none exceeds 236.

### F78. The fault corpus file is three jobs concatenated. MEDIUM

`crates/faults/src/archives.rs` is 1,574 lines, the largest non-CLI file in the
project, and carries an `#[expect(clippy::too_many_lines)]` acknowledging it.
Lines 1-110 are the corpus data model. 112-433 are a raw ustar writer. 434-836
are a zip writer bundled with a hand-written fixed-Huffman DEFLATE encoder that
has nothing to do with either. 838-1574 are the catalog of named hostile and
benign archives.

A hostile corpus is inherently long, so the catalog earns its 671 lines. The tar
writer, the zip writer and the DEFLATE encoder are three separable things, and
the encoder in particular implements enough of RFC 1951 to deserve tests of its
own.

Checked and correct while reading it: the CRC-32 polynomial and initialization,
the fixed literal and length code assignments, the maximum match length coded via
symbol 285, and every ustar header field offset and the checksum convention.

### F79. Two functions and five helpers exist twice or three times. MEDIUM

`rung_of` is byte-for-byte identical at `crates/engine/src/transfer.rs:132` and
`crates/engine/src/candidate.rs:143`.

`is_hex`, `hex_nibble` and `decode_hex_32` are identical in
`crates/engine/src/metadata/bagit.rs:11,15,24`,
`crates/engine/src/metadata/pooch.rs:9,13,22` and
`crates/engine/src/metadata/sidecar.rs:9,13,22`. `split_digest_and_path` and
`algorithm_name_for_length` are duplicated between `bagit.rs:38,48` and
`sidecar.rs:36,46`, and the two copies of the second disagree: bagit recognizes
SHA-512 at length 128 and sidecar stops at SHA-1.
`crates/engine/src/metadata/mod.rs` already exists and already holds the helpers
these readers share.

The listing entry bound is written three times, in
`crates/sources/src/http.rs:470`, `crates/sources/src/object_store.rs:182` and
`crates/sources/src/provider.rs:494`, the third of which factored it into a
private helper the other two cannot reach.

`crates/cache/src/bundle.rs:196-421` hand-rolls a ustar reader and writer for
bundles while `crates/archive` reads ustar for materialization. The two have
different jobs -- a bundle member is named by a digest and is never a path -- so
this is worth a decision rather than an automatic deletion.

standards.md: "Two ways to do the same thing is a defect. Delete one."

### F80. `transfer.rs` holds five policies that change on five schedules. MEDIUM

`crates/engine/src/transfer.rs` is 1,076 lines: source selection and scoring
(257-362), the credential offer calculation (295-320, 1073-1076), resume rung
decisions (100-141, 538-583), split and parallel span mechanics (591-738), retry
and backoff (61-82, 926-1012), and the byte copy loop with its backpressure
(873-924). Each can change without the others. `Retry`, `Pause`, `SleepingPause`,
`backoff` and `honors` are already a self-contained group and would move whole.

### F81. What each seam cannot say. MEDIUM

The question that found the last two structural defects in this project -- the
Source seam could not say which references it serves, and could not say how many
links a listing ignored -- asked of all six.

**Platform.** It cannot report its own degradation. `seam/platform.rs:134-139`
promises that `preallocate` "emits a degrade event" when a volume cannot reserve
blocks, and no method on the trait takes an observer or a degrade queue. Whoever
calls it has to infer the degradation from a side effect.

**Store.** `list()` returns a bare `Vec<ContentDigest>` with no size and no
placement, and there is no paging, so `status` and a prune dry-run have nothing
finer than the aggregate to work from -- which is what makes F38's cost
inevitable rather than accidental. `waited(&lease) -> bool` cannot say how long
or why, so `cache.wait` carries less than its contract slot allows.

**Source.** `Listing` carries the entries and the count that were skipped, which
is what phase 7.5 added. It cannot say that the listing was clipped by the
500,000-entry or 16 MiB bound: a container with more entries than the limit and
one with exactly that many produce the same shape.

**Archive.** `members()` is all or nothing against a 1,000,000-entry limit, so
the trait cannot report progress or reject a member while enumerating. `open()`
folds "no member by that path" and "a member by that path that differs" into one
error.

**Policy.** Every concurrency getter is global. There is no way for a policy to
state a per-host or per-provider ceiling, which is what the adaptive controller
and the many-hosts regime are both about. `accepts(class) -> bool` cannot say
why a class was refused, so the reason is reconstructed downstream.

**Observer.** The trait is one method and leaks nothing. The gap is who calls
it: inside `crates/engine`, only `transfer.rs` does. `reconcile.rs`, `repair.rs`
and the outboard verification walk each compute something the contract names an
event for, and none of them can emit it, so the module that knows the outcome is
not the module that reports it. The CLI emits those events instead, which is why
all thirty-four have a production emitter and why changing when
`reconcile.outcome` fires means editing a crate that has never imported
`Observer`.

### F82. Removing one packed object rewrites its whole pack. MEDIUM

`crates/cache/src/storage.rs:243-248` `remove_object` calls `rewrite_pack`, and
`crates/cache/src/pack.rs:229-283` reads every surviving object's full bytes out
of the old pack and writes them into a new one. The cost is the size of what
remains, not of what was removed, and `prune` calls it once per digest, so a
sweep removing many objects from one large pack pays that cost once per object
rather than once.

decisions.md:5895 refuses a tombstone because "a tombstone would be a second
authority on what a pack holds", which is a correctness argument and survives at
any scale. It does not require the rewrite to be synchronous or per-object. borg
marks segments sparse and compacts later; restic batches removals into one pass.
Batching a sweep's same-pack deletions into one `rewrite_pack` turns
O(deletions x pack size) into O(pack size) and touches no authority question.

### F83. Two smaller reporting defects in the CLI. LOW

`crates/cli/src/policy.rs:134` prints `using {label} for {host} from {from}` with
`eprintln!`, unconditionally, ignoring the log level, as a formatted sentence,
where every sibling fact in the same file goes through `self.emit`. It carries no
secret -- `label()` returns "a bearer token" or "a signing key pair" -- and it is
the shape the redaction discipline exists to prevent, sitting in the credential
resolver. No test covers the line.

`crates/cli/src/inference.rs:159` emits `EventPayload::ResolveAlias` with
`to: format!("{} artifacts", count)`. The event is contracted to say what a
reference resolved to, and `crates/cli/src/main.rs:1298` uses it correctly for
exactly that. A consumer reading the stream to learn what was resolved gets
"5 artifacts".

### F84. Path length is a constant on both platforms, and the contract says it is queried. LOW

contracts.md Limits: "Path length | the target platform's own maximum, queried
per volume." `crates/platform/src/linux/probe.rs:93` hardcodes 4,096 and
`crates/platform/src/windows/mod.rs:23` hardcodes 32,767. The adjacent
`max_component_length` genuinely is queried on both, through `statfs` and
`GetVolumeInformationByHandleW`, so the shape exists and this row does not use
it. A Windows volume without long-path support has an effective limit of 260.

### F85. Three decider bypasses inside the cache, in the code the first audit's fix covered. MEDIUM

`crates/cache/src/bundle.rs:292-298` and `:303-315` map a read failure straight to
`cache.corrupt`. `crates/cache/src/lib.rs:361-370` does the same in the one branch
of `check_format` that writes the format file, while every other branch in that
function uses the decider. `crates/cache/src/storage.rs:274-286` re-wraps an
already-typed error from `publish_file` into `cache.corrupt`, discarding a
cross-volume or disk-full verdict. `crates/cache/src/store.rs:491-498` maps every
failure from `remove_file` to a fixed "nothing pins it", where the same file uses
the correct not-found-is-fine idiom thirty lines earlier.

### F86. What was checked and found correct. Not a finding.

Recorded because an audit that reports only defects gives no sense of what it
looked at.

All forty unsafe blocks live in `crates/platform/src/windows/ffi.rs`, each
carries exactly one `SAFETY:` line, and each line states the invariant the call
actually relies on. Every Win32 signature was checked against the pinned
`windows-sys 0.61.2` bindings and current Microsoft documentation: the
BOOL-returning calls check `GetLastError` correctly, `RegGetValueW` and
`GetSecurityInfo` correctly treat their return value as the error code rather
than calling `GetLastError`, and the `FilterFind*` family is correctly handled as
`HRESULT` rather than BOOL. `crates/engine`, `crates/cache`, `crates/archive`,
`crates/sources`, `crates/faults`, `crates/view` and `xtask` contain no unsafe
code at all.

No plain comment exists anywhere in the tree. The comment checker works and its
own tests cover strings, raw strings, char literals against lifetimes, nested
block comments and doc comments containing block-comment markers.

Every `cfg` in `crates/cli`, `crates/cache`, `crates/archive`, `crates/faults`
and `xtask` is a real platform divergence with both arms implemented, not an
assumption. `CAN_RELEASE_PAGES` being Linux-only was re-verified against current
Microsoft documentation rather than inherited: there is no public per-range
post-write page-cache eviction call on Windows, `FILE_FLAG_NO_BUFFERING` would
require sector alignment on every write, and `FlushFileBuffers` flushes without
evicting. The code's claim is correct.

Redaction is clean everywhere except F32. `SafeUrl` redacts userinfo and every
query value at construction, as a class rather than by a list of names, and every
error construction site in `crates/engine`, `crates/cli`, `crates/faults` and the
rest of `crates/sources` was read for the leak shape and did not have it.

SigV4's signing key derivation, string to sign, canonical headers, signed headers
list and empty-payload sentinel all match the published worked example and the
HMAC test vectors. The tar reader's magic offset, entry type handling and setuid
rejection match POSIX ustar. The zip reader searches for the end-of-central-
directory record backwards and takes the last match, which is the spec-correct
way to keep a forged signature inside a comment from shadowing the real one, and
enforces store-and-deflate against the central directory rather than the local
header, which is the authoritative one.

The connection pool shape is what the contract requires: one agent per host,
built once and cached for the run, one pool inside each. TLS session resumption
works, because rustls resumes by default and the client caches the configuration
per agent.

`--no-cache` produces an identical tree digest, entry count and set of work
counters to a cached run over the same archive, which closes what audit.md still
records as open.

---

## Measurements

Windows numbers were taken on a quiet machine with nothing else in flight. Linux
numbers were taken in WSL2 Ubuntu 24.04, kernel 6.18-microsoft-standard-WSL2, on
an ext4 volume, with the source copied out of the 9p mount first so the
filesystem is real. Both used the release binary.

Two instruments were built for this pass, neither of them in the repository.
The first reads the kernel's own per-process counters for one run --
`GetProcessIoCounters` and `GetProcessMemoryInfo`, both of which answer for a
process that has already exited while a handle is held, and neither of which
needs elevation. The second is `du -sb` on Linux, which needs nothing at all.
Both exist because every number this project has published about its own work
came from the program describing itself.

### The counters against the operating system

Windows, `GetProcessIoCounters`, one local `get` per row:

| object | kernel wrote | `bytes_written` | kernel read | `bytes_read` | file ops | peak working set |
|---|---|---|---|---|---|---|
| 8 MiB | 17,303,527 | 25,165,824 | 16,777,216 | 8,388,608 | 27 | 8,830,976 |
| 64 MiB | 134,744,046 | 201,326,592 | 134,217,728 | 67,108,864 | 27 | 8,822,784 |
| 256 MiB | 537,413,565 | 805,322,696 | 536,870,912 | 268,435,456 | 30 | 9,048,064 |

Linux, same three sizes, same binary source, `du -sb` on what the run left and
`/usr/bin/time -v` for the resident set:

| object | `bytes_read` | `bytes_written` | file ops | peak RSS |
|---|---|---|---|---|
| 8 MiB | 8,388,608 | 25,165,824 | 27 | 7,108 KB |
| 64 MiB | 67,108,864 | 201,326,592 | 27 | 7,272 KB |
| 256 MiB | 268,435,456 | 805,322,696 | 30 | 7,168 KB |

Every deterministic counter is byte-identical across the two operating systems,
which is the statement this project values most and which holds.

The 256 MiB Linux run left, measured with `du -sb`:

```
268,435,456   the destination
268,452,609   the cache
------------
536,888,065   on disk
805,322,696   what bytes_written reported
```

1.4999. Two platforms, two independent methods -- a kernel counter on one and
disk occupancy on the other -- give the same answer, so F30 is a defect in the
code and not an artifact of either. 536,888,065 is also within a kilobyte of the
536,887,240 audit.md's F21 recorded before `ad5cd0b` touched the counters, which
means F21's number was right, the disk has not changed, and what changed is the
reporting.

### Peak resident memory

Flat at 8.8 to 9.0 MB on Windows and 7.1 to 7.3 MB on Linux for objects of 8, 64
and 256 MiB. Streaming holds on the local `get` path and the memory rule in
standards.md is being followed rather than merely stated. This is the first
resident-set number this project has, five phases after decisions.md:666 said one
would be added.

### The pack index, at four sizes, on both platforms

`cache status` against a cache holding only packed objects, which is the command
whose whole job is to count what is there. CPU time, median of three on Windows
and of two on Linux:

| packed objects | Windows CPU | Linux CPU |
|---|---|---|
| 250 | 15.6 ms | |
| 500 | 31.3 ms | |
| 1,000 | 62.5 ms | |
| 2,000 | 484.4 ms | 260-290 ms |
| 4,000 | 1,765.6 ms | 1,200-1,220 ms |

Doubling the object count from 2,000 to 4,000 multiplies the cost by 3.65 on
Windows and by 4.1 to 4.4 on Linux. Quadratic is 4. The Windows numbers below
1,000 are quantized by the scheduler's 15.6 ms tick and are not evidence; the
2,000-to-4,000 step is, on both platforms.

Fitting the quadratic term against the Linux numbers, which are the cleaner set,
gives about 7.6e-8 seconds per object squared. That extrapolates to:

| packed objects | one `cache status` |
|---|---|
| 10,000 | 7.6 s |
| 100,000 | 13 minutes |
| 1,000,000 | about 21 hours |

F26 has been deferred three times on the ground that settling it needed a
million-object corpus. It needed four thousand and an afternoon. The answer is
that `status` does not complete at a million objects, and the cause is F38 rather
than the whole-list reads F26 named.

### The regimes, on a quiet machine

`cargo xtask bench --compare` at `e5d6f09`, 366 seconds:

| regime | wall | bytes read | bytes written | requests | file ops |
|---|---|---|---|---|---|
| no-op | 131.7 ms | | | | |
| cold-cache | 1,682.0 ms | 16,777,216 | 33,559,040 | 0 | 213 |
| warm-cache | 562.2 ms | 16,777,216 | 16,777,216 | 0 | 67 |
| cold-transfer | 288.9 ms | 0 | 8,388,608 | 2 | 38 |
| interrupted-transfer | 795.6 ms | 4,194,304 | 8,388,608 | 6 | 58 |
| many-small-files | 28,801.6 ms | 1,048,576 | 2,170,880 | 0 | 3,093 |
| one-large-file | 1,043.5 ms | 268,435,456 | 805,322,696 | 0 | 30 |
| many-hosts | 11,407.9 ms | 0 | 6,292,608 | 40 | 325 |

Every deterministic metric matches the committed baseline exactly, so the gate
passes and the counters are reproducible. That is worth saying plainly: they are
consistent, they are cross-platform, and two of them are wrong, and consistency
was never going to catch that.

The timing numbers are a different matter, and comparing them against
docs/benchmarks.md, which was generated by this same harness on this same
machine, is instructive:

| regime | published | today | ratio |
|---|---|---|---|
| no-op | 9 ms | 131.7 ms | 14.6x |
| many-small-files | 7,150 ms | 28,801.6 ms | 4.0x |
| warm-cache | 478 ms | 562.2 ms | 1.2x |
| one-large-file | 759 ms | 1,043.5 ms | 1.4x |

The scanner cost ratio the same run measures moved too, from 47.33 to 13.07. So
the machine is in a different state, which is exactly what standards.md says a
wall clock is. Two things follow. The published page is not reproducible on the
machine that produced it, which is worth saying on the page. And the no-op
regime's fourteen-fold spread has a specific cause worth naming: the harness
rebuilds the binary immediately before measuring it, so every iteration of the
regime that is supposed to bound "how much a dependency may cost simply by
existing" is paying an on-access scanner's first touch of a file written seconds
earlier.

### What was not measured, and why

**A sampled call-stack profile, on either platform.** Every Windows option --
`xperf`, `wpr`, `samply`, and `cargo-flamegraph` through `blondie` -- records
through an ETW kernel session, and an ETW kernel session requires elevation. The
tools are installed; the shell is not elevated and cannot answer a consent
prompt. This is a sharper statement than the first audit's "no profiler is
configured on this machine": the profiler is configured, and the blocker is a
privilege. In WSL2 the kernel exposes almost no PMU events, `perf` is not
present, and building it from the WSL2 kernel tree is setup rather than
measurement. The one tool that would run here unelevated is the `dhat` crate,
which needs a development dependency and a feature gate rather than an install,
and which profiles allocations rather than time.

**The factor of four in the per-object cost.** audit.md left it unattributed and
said it needed a kernel trace nobody had configured. It still needs one, and the
reason has changed from absence to elevation. What this pass can add is that two
of the four candidates are now sharper. The adopt path's second read is real and
uncounted (`crates/cache/src/ingest.rs:96` for a packed object,
`std::fs::copy`'s read inside `copy_bytes` for a loose one), which is what the
0.5x in F30 is made of. And `crates/cache/src/storage.rs:318` materializes a
packed object through a 64 KiB buffer where every other streaming path in the
tree uses 1 MiB, so that path issues roughly sixteen times the syscalls the
others do for the same bytes. Neither of those is the whole factor of four, and
neither was distinguished by a measurement here.

**Whether cloning works on a volume that clones.** F31 is a specification
argument, and this machine has no ReFS volume or Dev Drive to try it on. That is
the one empirical step left on the highest-value I/O finding.

### Binary size, by crate

`cargo bloat --release --bin fetchloom --crates`, the first per-crate size
attribution this project has taken. The profile carries debug info, so the file
is larger than the gated 8,315,904 bytes; the proportions are what matter.

| share of .text | size | crate |
|---|---|---|
| 24.3% | 1.5 MiB | graviola |
| 21.1% | 1.3 MiB | fetchloom_cli |
| 7.9% | 505 KiB | fetchloom_engine |
| 7.5% | 482 KiB | rustls |
| 7.4% | 472 KiB | std |
| 5.5% | 353 KiB | clap_builder |
| 3.3% | 210 KiB | ureq |
| 2.4% | 154 KiB | fetchloom_sources |
| 1.8% | 113 KiB | clap_complete |

The pure-Rust cryptography provider is the largest single crate in the binary,
larger than the command-line crate and three times the engine. That is the price
of the swap at `80c5253` which bought Windows on ARM back, and it has never been
stated as a number. Argument parsing is 466 KiB across two crates.
`cargo machete` finds no unused dependency in any member manifest.

The `[profile.release]` section does not exist. Every setting is the Cargo
default: `opt-level = 3`, `lto = false`, `codegen-units = 16`, `panic = "unwind"`,
`strip = "none"`. Eight crates compiled in sixteen units each with no cross-crate
optimization is the largest untaken lever on a metric this project gates at five
percent. `panic = "abort"` is not available: `crates/cli/src/doctor.rs:285` uses
`std::panic::catch_unwind` to report whether the trust store loads, and aborting
would turn a diagnostic into a crash.

### The suite

`cargo test --workspace --exclude xtask`, whole suite, quiet machine: **442
seconds, two failures.**

Both failures are in `crates/cli/tests/concurrent.rs`:
`eight_artifacts_behind_a_charged_wait_finish_in_far_less_than_eight_one_at_a_time`
and `a_per_host_ceiling_of_one_takes_measurably_longer_than_a_ceiling_of_four`.
Run alone, that binary passes three of three in 6.28 seconds. The phase 8 gate
predicted the first; the second is the same defect and is not recorded anywhere.

Where the 442 seconds goes, measured per binary alone:

| binary | alone |
|---|---|
| `fetchloom-cache --test concurrency` | 293 s |
| `fetchloom-cli --test prove` | 109 s |
| `fetchloom-cli --test ranged` | 45 s |
| `fetchloom-cli --test transfer` | 4 s |
| everything else | under 7 s each |

One test is 293 of those seconds:
`a_thousand_kills_leave_no_invalid_object_and_no_orphan_after_recovery`. It is
the phase 1 exit criterion and it is the strongest correctness statement this
project makes. `prove.rs`'s 109 seconds is the phase 5 exit criterion, a 68 MB
object repaired by range.

So the honest answer to "what does the suite spend its time on" is: two tests,
both of which are gate criteria, neither of which should be shortened. The
twenty-five duplicated process-spawning harnesses of F67 are a maintenance and
isolation problem rather than a speed one, and the earlier framing that they are
what makes the suite slow does not survive the measurement.

---

## Research, against the code

Every technique below was checked against current documentation or against the
vendored source of the dependency itself. Where a domain yielded nothing that
applies here, that is written down rather than padded.

### F87. `cargo xtask profile` has only ever measured a debug build. HIGH

`.cargo/config.toml` defines `xtask = "run --package xtask --"`, which builds in
the dev profile. `xtask/src/profile.rs` hashes in process rather than driving the
binary, so the code it times is the code the dev profile produced. Run that way
it reports about 5 MB/s. Re-run as
`cargo run --release --package xtask -- profile`, it reports 450 to 1,370 MB/s --
a factor of roughly two hundred and fifty.

decisions.md:3612 records that this profile is what settled the shape of the
hashing pipeline. Every number it has printed until now was a measurement of an
unoptimized build, and standards.md says "Profile before changing anything.
Guessing at bottlenecks is not permitted as a justification."

The conclusion the profile supports turns out to survive the correction, which is
luck rather than method. Re-run in release, best of five, sixteen threads:

| size | install-per-chunk | install-once | inline | install-once-parallel-content |
|---|---|---|---|---|
| 1 KiB | 454 MB/s | 306 | **525** | 119 |
| 256 KiB | **1,318** | 1,291 | 1,066 | 1,006 |
| 1 MiB | **1,367** | 1,169 | 714 | 793 |
| 4 MiB | 950 | **984** | 656 | 942 |
| 16 MiB | **919** | 829 | 629 | 873 |

The code hashes inline below `POOL_THRESHOLD` (1 MiB,
`crates/engine/src/hashing.rs:15`) and enters the pool once per chunk above it.
Inline is fastest at 1 KiB and worst at every size at or above 1 MiB, and
install-per-chunk is fastest or within noise everywhere above the threshold. The
code took the answer. `update_rayon` for the content digest, which the fourth
column measures, would not have been a win at any size here, which is consistent
with blake3's own documented crossover being processor-dependent and above
128 KiB.

The same run also prices the buffer-reuse rule: a fresh 1 MiB `vec![0u8; _]`
costs about 20 microseconds, so a 256 MiB object streamed in 1 MiB chunks would
pay 5.1 ms in allocation and zeroing alone if the buffers were not reused. They
are (checked at every site in `transfer.rs`, `hashing.rs`, `outboard.rs`,
`ingest.rs`, `extract.rs` and `materialize.rs`), so the rule is being followed
rather than merely stated.

Cost to fix: build the profile in release, and say in the record which build a
number came from.

### F88. `--threads` is ignored on two paths. MEDIUM

`crates/cli/src/main.rs:1583` `thread_budget` resolves the detected parallelism
against the user's `--threads` setting.
`crates/cli/src/main.rs:514` and `crates/cli/src/run.rs:959` both call
`ThreadBudget::resolve(available_parallelism(), None)` directly, hard-coding the
override away. The first is inside `verify`, the second on the local-tree digest
path.

contracts.md lists `--threads` as a global flag and says a ceiling above the
detected budget "is clamped rather than honored, and the clamp is reported". A
ceiling that is discarded is neither honored nor reported.
standards.md: "Every fallback emits a `degrade` naming what was requested, what
was used, and why."

### F89. One streaming path uses a buffer sixteen times smaller than every other. LOW

`crates/cache/src/storage.rs:318` materializes a packed object through
`vec![0_u8; 1 << 16]`. `crates/cache/src/ingest.rs:17`,
`crates/cache/src/bundle.rs:19` and `crates/cache/src/rebuild.rs:16` each declare
`const BUFFER: usize = 1 << 20` and use it; `crates/archive/src/extract.rs:19`
declares `BUFFER_LEN = 65_536` for archive extraction.

So the tree carries three separate declarations of the same 1 MiB constant, one
separate 64 KiB constant, and one anonymous 64 KiB literal, and the anonymous one
is on the path that reads every packed object out of the cache. That path issues
about sixteen times the read and write calls the others do for the same bytes.
standards.md: "Buffer sizes are aligned and sized for the device, not for
convenience."

### F90. Two unsafe blocks now have safe equivalents. LOW

`crates/platform/src/windows/ffi.rs:265` calls `CreateSymbolicLinkW` with the
unprivileged-create flag so that a symlink can be made under Developer Mode
without administrator rights. `std::os::windows::fs::symlink_file` and
`symlink_dir` have done exactly that since 2017.

`crates/platform/src/windows/ffi.rs:183` calls `FlushFileBuffers` on a
`&std::fs::File`'s raw handle. `File::sync_all` is documented to do that on
Windows, and Windows has no data-only flush, so there is no behavioral
difference.

Neither is a performance question. standards.md: "No unsafe code except where a
platform call requires it." Two of the forty-one no longer require it.

None of the other thirty-nine has a safe equivalent: the standard library wraps
no ACL, credential-manager, minifilter, job-object or block-cloning API.

### F91. The release profile does not exist, and the largest crate in the binary is the crypto provider. MEDIUM

There is no `[profile.release]` anywhere in the workspace, so every setting is
the documented Cargo default: `opt-level = 3`, `lto = false`,
`codegen-units = 16`, `panic = "unwind"`, `strip = "none"`. Eight crates each
split into sixteen codegen units with no cross-crate optimization is the largest
untaken lever on a metric this project gates at five percent.

`panic = "abort"` is not available: `crates/cli/src/doctor.rs:285` uses
`std::panic::catch_unwind` so that a trust store that fails to load is a
diagnostic rather than a crash. The pervasive `PoisonError::into_inner` pattern
is not a blocker, because with `abort` a lock can never be observed poisoned.

`strip` mostly helps the Linux side, because MSVC always splits debug information
into a separate PDB and the PDB does not count toward the gated size.

The per-crate attribution is under Measurements. `graviola` is 24.3 percent of
the text section, larger than `fetchloom_cli` and three times `fetchloom_engine`.
That is the price of the `ring` swap at `80c5253` that made Windows on ARM
compile, and it has never been recorded as a number.

PGO is available on both targets and is not worth it here: the loop that runs
many times is inside blake3 and sha2, whose own tuned kernels a caller's profile
cannot influence, and the code a profile could influence runs once per process.
It would also add a training run whose drift would move the gated binary size
with no source change, which is the one thing standards.md says must be
investigated rather than explained away. BOLT is disqualified outright, being
ELF-only against a rule that says both platforms or it is not done.

### F92. What the research confirmed rather than overturned

Recorded because inheriting a claim and re-deriving it are different things, and
the project's own rule is not to inherit.

**ureq 3.4 offers no HTTP/2.** Confirmed three ways: no occurrence of `http2`,
`HTTP/2` or `h2` anywhere in the vendored `ureq-3.4.0/src` tree, no changelog
entry through 3.4.0, and the crate's own issue tracker still carrying it as a
request. features.md's "not built ... because the HTTP client it is built on
offers nothing else" is accurate. The alternative would be `reqwest` on `hyper`,
which brings an async runtime this synchronous engine is not built for.

**The connection pool is the shape the contract requires.** One `ureq::Agent` per
host, built on first use and cached for the run
(`crates/sources/src/http.rs:52-56,71-78`), one pool inside each. TLS session
resumption works, because rustls resumes by default and ureq caches the built
configuration per agent, so the resumption store is shared across every reconnect
to that host. Nothing in this project asked for it and nothing measures it. It
could be measured without elevation by pointing the run at an unprivileged local
TLS-terminating proxy and comparing the second `ClientHello` for a pre-shared-key
extension.

**`CAN_RELEASE_PAGES` being Linux-only is correct, not assumed.** There is no
public per-range post-write page-cache eviction call on Windows.
`FILE_FLAG_NO_BUFFERING` bypasses the cache but constrains every read and write
to sector alignment, which is the second write path
`crates/engine/src/tuning.rs:369` refuses to build; `FILE_FLAG_WRITE_THROUGH`
forces durability without evicting; `FlushFileBuffers` flushes without releasing.
The claim contracts.md's Write path section rests on holds.

**The `SetFileValidData` exclusion still holds.** It needs a privilege, it
exposes other users' deleted data on an unclean shutdown, and it excludes
network, compressed, sparse and transacted files. decisions.md:588's reasoning is
unchanged against current documentation, and its own observation that a killed
transfer is a normal event here rather than an exceptional one is what makes the
exclusion right.

**The Windows preallocation order is correct.** `FileAllocationInfo` then
`FileEndOfFileInfo`, which matches the documented constraint that end of file
must not exceed allocation size.

**io_uring and IOCP are not worth it at this shape.** Both amortize syscall
overhead across many small independent operations submitted as a batch. This
workload is about five namespace operations plus one streaming copy per object,
parallelized across objects at the task level rather than batched within one.
What would change the answer is a workload that needed thousands of concurrent
small preallocate-write-rename triples in one submission stream, which is not
what ships.

**Vectored I/O does not apply.** Nothing here issues several small buffers per
call; the buffers are already coalesced by construction.

**`ioctl_ficlone` on Linux is whole-file and takes no range**, so the alignment
class of defect F31 found on Windows cannot occur there, and there is no partial
`copy_file_range` return to handle because `copy_file_range` is not used.

**No allocation was found inside a per-chunk or per-entry loop**, no clone was
found on a per-chunk path, and `clippy::nursery` and `clippy::perf` run across
the workspace find `redundant_clone` seven times and `needless_collect` twice --
every one of them in a test file, none in production code. 348 warnings in total,
188 of which are `missing_const_for_fn` on repeated test helpers.

**`#[inline]` appears nowhere in the workspace and does not need to.** Every
crate boundary on the hot path is crossed at 1 MiB granularity or coarser, so a
missed cross-crate inline costs one call per megabyte of hashing.

**Arena allocation would not help.** The allocation pattern is either one
long-lived reused buffer per stream or one allocation per unit of work whose
lifetime crosses a function return, and the per-entry planning vectors already
use `with_capacity`.

**Enum sizes are inherent rather than padding defects.** `Error` is 120 bytes,
carrying three optional heap strings and two boxed strings, and is moved once per
attempt rather than per byte. `EventPayload` is 144 bytes and its one per-entry
emission fires once per file. `ErrorKind` is one byte.

**Work counter false sharing is real and immaterial.** The four `AtomicU64`
counters occupy 32 bytes in one cache line and are contended by concurrent spans,
but each increment corresponds to a megabyte-granularity chunk or a whole
request, so the line ping-pong costs tens of nanoseconds against the I/O it is
reporting. Padding the struct is measurable in principle and not worth it.

**The whole-list seam reads are bounded and much smaller than the pack index
problem.** At its stated limit, `Archive::members()` is 64 bytes of spine per
entry plus a heap allocation per path, so roughly 110 to 160 MB at 1,000,000
members; `Store::list()` is 33 bytes per digest with no indirection, so about
33 MB at the same count. Both are one allocation held for one call. F38's clone
is per lookup, which is why it is the finding and these are not.

**Enforcing `resident_memory` is not a one-line change on either platform, and
the field's name promises more than either platform cheaply delivers.** Windows
job objects bound committed virtual memory rather than working set, and the
working-set fields trim rather than refuse. Linux's `RLIMIT_RSS` has been a no-op
since kernel 2.4.30; `RLIMIT_AS` bounds address space, and true resident-set
enforcement needs a cgroup. The honest options are to enforce the closest
available bound and rename the limit for what it is, or to state in contracts.md
that it is a design bound rather than a runtime one. That is a decision, not an
edit.

**Comparable projects were not measured.** Line counts for other Rust workspaces
of this size could not be verified from primary sources in this pass, so no
comparison is asserted. What can be said from this repository's own records is
that decisions.md:97 makes `cli` the only composition root deliberately and
decisions.md:101 rightly forbids inventing a crate to make a diagram look right
-- and that neither bounds the size of a file inside that crate, which is what
F77 is about.

---

## What I could not determine

**Whether cloning succeeds on a volume that clones.** F31 is a specification
argument backed by three current Microsoft pages, and this machine has neither a
ReFS volume nor a Dev Drive. Building one needs an elevated shell and Hyper-V,
which is the same blocker the phase 0 debt has carried since the beginning. What
has changed is that there is now a second, cheaper hypothesis to test, and it can
be tested by rounding `ByteCount` down and seeing whether the call stops failing.

**Why a cache file operation costs four times a bare one.** Still unattributed,
and the reason has changed from "no profiler is configured" to "every profiler
here records through an ETW kernel session and this shell is not elevated". Two
of the four candidates sharpened: the adopt path's second read is real and
uncounted, and the packed-object materialization path uses a buffer sixteen times
smaller than every other path. Neither accounts for a factor of four.

**Whether TLS sessions are actually resumed.** They should be, by construction,
and nothing measures it. The method that would, without elevation, is written
down in F92.

**What a real cache holds.** The pack-index cost is quadratic in the number of
packed objects and the extrapolation to a million is arithmetic rather than
measurement. Four thousand objects were measured on both platforms; the shape is
not in doubt, the ceiling a real user reaches is.

**Whether `--threads` is reachable on the two paths that discard it.** F88 is a
code reading. Whether a user can actually set `--threads` and have `verify` or
the local-tree digest path ignore it depends on call-site wiring that was not
traced to a conclusion.

**Whether the zip data-descriptor and ZIP64 gaps are reachable from a real
publisher's archive.** Both are read from the reader against the format
specification. Neither was reproduced with a real zip, and the hostile corpus
cannot produce the ZIP64 case because it only writes the variant whose legacy
fields stay valid.

**Whether anything in features.md is false that this pass did not think to
check.** Five sentences were found false by running the binary against them.
Nothing enforces the convention mechanically, which the phase 7.5 record said,
the phase 8 record repeated, and this record says a third time. Three audits
have now found false sentences in that file by hand. The fourth should not have
to.

---

## Dispositions

Empty. Part A of this pass changed no code. Every finding above gets a
disposition line when Part B acts on it, in the form audit.md uses: fixed and
where, or deferred and what would clear it.

| Finding | Disposition | Where |
|---|---|---|
| F29 offline reaches the network | | |
| F30 byte counters wrong in both directions | | |
| F31 block cloning cannot succeed | | |
| F32 raw location beside the redacted one | | |
| F33 SigV4 canonical URI and query | | |
| F34 index keeps links outside the prefix | | |
| F35 `--color` honored nowhere | | |
| F36 two config keys parsed and dropped | | |
| F37 five filesystem-failure deciders | | |
| F38 packed index cloned per lookup | | |
| F39 Windows never answers `absent` | | |
| F40 zip data descriptor and ZIP64 | | |
| F41 `plan` reports `[` as an IPv6 host | | |
| F42 idle age never set | | |
| F43 sequential connect | | |
| F44 the S3 help record is false | | |
| F45 three unfulfilled lint expectations | | |
| F46 two stray committed files | | |
| F47 eleven docstrings carry rationale | | |
| F48 view isolation defeated by a grouped import | | |
| F49 three bomb guards | | |
| F50 the timing gate does not exist | | |
| F51 the deterministic gate only fails upward | | |
| F52 the no-op regime measures `explain` | | |
| F53 resident memory measured nowhere | | |
| F54 six of eight regimes cannot exercise the controller | | |
| F55 rounds overwritten rather than compared | | |
| F56 error-kind reachability test cannot fail | | |
| F57 config-key test cannot fail | | |
| F58 decider walk cannot catch its class | | |
| F59 no test asserts a run emits each event | | |
| F60 two error kinds have no producing test | | |
| F61 sixteen test files clear no environment | | |
| F62 two wall-clock ratio tests | | |
| F63 two-second bound under load | | |
| F64 a test reaches the resolver | | |
| F65 the network lane reaches the internet | | |
| F66 tests sleep | | |
| F67 nine names for one harness | | |
| F68 a test re-implements the pack layout | | |
| F69 five false sentences in features.md | | |
| F70 the `zenodo:` example does not resolve | | |
| F71 human lines versus structured logging | | |
| F72 roadmap.md has no phase 7.5 | | |
| F73 twelve stale line references | | |
| F74 six stale reference transcripts | | |
| F75 build-protocol.md describes three platforms | | |
| F76 audit.md is stale on `--no-cache` | | |
| F77 run.rs and main.rs | | |
| F78 the fault corpus file | | |
| F79 duplicated functions and constants | | |
| F80 transfer.rs holds five policies | | |
| F81 what each seam cannot say | | |
| F82 pack rewrite per delete | | |
| F83 an eprintln and a misused event | | |
| F84 path length hardcoded | | |
| F85 three decider bypasses in the cache | | |
| F86 what was checked and found correct | not a finding | |
| F87 the profiler measured a debug build | | |
| F88 `--threads` ignored on two paths | | |
| F89 one buffer sixteen times smaller | | |
| F90 two unsafe blocks with safe equivalents | | |
| F91 no release profile | | |
| F92 what the research confirmed | not a finding | |

## What this pass closes in audit.md

**F21, settled.** A cold local fetch writes the bytes exactly twice. Measured on
Linux by disk occupancy: 268,435,456 into the destination and 268,452,609 into
the cache, 536,888,065 against the 536,887,240 audit.md recorded before the
counters were touched. It still needs a cloning volume to say what removing the
second write would buy, and F31 says the volume was never the only obstacle.

**F26, settled, and not by the experiment it asked for.** The whole-list seam
reads are bounded and modest: about 110 to 160 MB for a million archive members
and about 33 MB for a million cache digests, each held for one call. The cost
that does not survive a million objects is F38's clone-per-lookup, which is
quadratic and was measured at four thousand objects on both platforms. The
million-member corpus was never the missing evidence.

**F5 and F6, reopened.** Both were closed at `ad5cd0b`. F30 shows the fix
introduced a double count on the write side and left the read side of the same
copy uncounted, so both metrics are wrong at `e5d6f09` in a way neither was
before. The counters are now consistent, cross-platform, and wrong, which is the
state the first audit's own framing did not anticipate.

**F19's memory half, still open, with a reason rather than a deferral.**
`resident_memory` is enforceable only approximately on either platform, and the
approximations bound different things than the field's name promises. F92 states
what each platform can and cannot do.

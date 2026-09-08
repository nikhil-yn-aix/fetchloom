# Contracts

What Fetchloom promises. If a behavior is not written here it is not defined and nothing should rely on it.

Before 1.0 there is one format and no compatibility code. From 1.0 the portable set below is readable forever.

## Compatibility

Two categories with different obligations.

**Portable** artifacts travel between machines, people and time: manifests, locks, receipts, plans, bundles, the command surface, flag names, exit codes, JSON result keys, event names. From 1.0 changes to these are additive only. A field is never removed, renamed, or given a new meaning. A removed capability leaves its field readable and reports it unsupported.

**Derived** data carries no obligation: cache objects, outboard trees, staging, per host measurements, resolution metadata. All of it regenerates from portable artifacts and sources, so a format change discards it rather than migrating it. The cache format fingerprint makes that discard explicit and loud.

Four decisions make that cheap, and all four hold from the first commit. Identity is content addressed, so a digest computed today is valid forever. Surface syntax is separate from the canonical model, so new syntax never changes identity. Digest and tree algorithms are domain separated and named where they are used, so a future algorithm is an addition rather than a reinterpretation. Unknown keys are refused before 1.0 and reserved after, and the `x-` prefix is reserved now, which keeps the strict to extensible move a one way door.

## Identity

| Term | Meaning |
|---|---|
| Content digest | BLAKE3 over the object bytes. The cache key and the resume authority |
| Interop digest | SHA-256 over the same bytes. Recorded to match publisher claims. Never the cache key |
| Outboard tree | Stored BLAKE3 chunk tree. Enables range verification and localized repair |
| Tree digest | BLAKE3 over the canonical entry stream of a materialized directory |
| Manifest digest | BLAKE3 over the canonical JSON form of a manifest, never over its source text |

Written as `blake3:<hex>` or `sha256:<hex>`. Both are computed on every transfer, in one pass over the bytes.

Three digests are domain separated by derived key so a value from one can never be mistaken for another: object content, tree digest, manifest digest.

An object at or below the outboard threshold stores no tree, because its content digest already authenticates it whole. A tree is derived: the content digest is the only authority, and a tree that disagrees with it is discarded and rebuilt rather than believed.

## Canonical form

Every portable artifact has one canonical form, and every digest covers that form rather than the text anyone typed.

A timestamp is written and read in one form: the date, `T`, the time to the second, and `Z`. Nothing else is written and nothing else is read as one.

A manifest is accepted as YAML, TOML or JSON. A lock, receipt and plan are written in the YAML subset and read back from any of the three.

The YAML subset is block mappings, block sequences, flow sequences, flow mappings, and plain, single quoted and double quoted scalars. Anchors, aliases, merge keys, tags, directives, block scalars, document separators, and tabs as indentation are each refused by name. `true`, `false` and `null` are the only words read as anything but text, and a run of digits with an optional sign is the only text read as a number, so `yes` is a word.

Canonical JSON is what every digest covers. Mappings ordered by the raw bytes of their keys, absent values omitted rather than written null, no insignificant whitespace and no line ending anywhere, so the bytes are identical on every platform.

The canonical text form written to a file uses the same ordering, writes every scalar as a double quoted JSON string, writes a mapping key plain when it is alphanumeric with underscores, hyphens and dots and quoted otherwise, indents by two spaces, and ends every line with one line feed on every platform. A carriage return before a line feed is read and never written. A byte order mark is read and never written.

## References

Resolution order is deterministic: explicit scheme, then a local path if it exists, then each configured source in order. A name matching none of the configured sources fails. It is never guessed at.

A name with no source configured is searched for across every registry that offers search, run in parallel, and a registry that fails is a registry that found nothing rather than a run that failed. Exactly one record carrying the name resolves to it; several refuse, naming each with its size, where it came from, what it states about its bytes, and the command that would take it; none fails, naming the nearest names it did find. There is no fourth outcome and nothing is chosen for the user.

A name is never written to a lock. What it resolved to is, so the first run searches and every run after is exact. Writing the name would trade pinning away to save typing, and a lock exists to say what was fetched rather than what was asked for.

A name matches a record when both fold to the same letters and digits, so case and punctuation do not separate them. A record within three edits of the term is suggested and never resolved to, because a near name is a question and not an answer.

A local path is read as a manifest when its extension is one a manifest is written in, and as data otherwise. Nothing else decides it, and a directory is never searched for one.

A reference naming no manifest resolves to a synthesized one: the dataset name, and one artifact carrying that name and, when the reference names a network location, that location. A local path is never recorded in it, so its digest is the same on every machine holding the same data.

A reference naming one object materializes a directory holding that one entry under the object's own name. A reference naming a container materializes every entry it holds. The two differ only in what is walked, never in what a destination is.

Nothing there fails with `reference.unresolved` saying nothing is there. Something there that cannot be read fails with `reference.unresolved` saying to make it readable. The two are never reported as each other.

## The project file

`fetchloom.toml` is found by walking up from the working directory to the root of the volume, and the first one found is the one used. There is one project file, one syntax and one discovery rule; `--config` names one instead and `--no-config` uses none.

Every relative path that file states resolves against the directory holding it, and never against the working directory. That covers the `datasets` table's destinations and the cache and library directories it names. The lock a run writes lands beside it too, unless `--lock` names somewhere else. A run from a subdirectory therefore writes what a run from the top writes, which is the difference between a project file that is usable from a script and one that is not.

`get` with no reference fetches every entry of the `datasets` table, in the order the table sorts. An entry is either a reference or a table stating `ref` and optionally `output`, `select`, `exclude` and `layout`. A string is never read as a table and a table always states `ref`, so one shape never means the other. An unknown key inside an entry is an error naming the key, as every other key in the file is.

An entry with no `output` lands at the entry's own name beside the project file. Two entries that would write to one destination fail with exit 2 naming both, before anything is resolved or fetched, and two spellings of one path are one destination. Every entry of the table is read and checked before the first is fetched, so a value no run can take — a destination this platform cannot name, a `layout` that is neither `keep` nor `flatten:<n>` — fails with nothing fetched rather than after the entries before it landed.

A dataset that fails does not stop the ones after it, because each is its own destination and each publishes whole or not at all. The run exits with the code of the first failure.

`get` with no reference and `--output`, `--select`, `--exclude`, `--layout` or `--library` is refused, because one destination and one selection cannot describe several datasets and the table already states each. `--locked` is the reproducible install: every dataset pinned exactly, refusing where resolution differs.

## Probe and list

`probe <ref>` resolves the reference, asks the source, and moves no payload bytes. It reports the resolved location redacted, the size, every digest the source states with the algorithm each one is, the trust class a fetch would land in, whether the source serves ranges, and whether the cache already holds the object. A fact the source does not state is unknown, unknown is an answer, and probe exits 0. Under `--offline` it answers from what the cache holds, or fails with `policy.offline`.

The trust class a probe reports is `verified` when a digest a run could compare against is already stated, and `tofu` otherwise. It never reports `corroborated`, because corroboration is a fact about witnesses of bytes this machine has seen and a probe has seen none.

`list <ref>` enumerates what a container holds, moving as few bytes as the format allows.

| What is listed | What it costs |
|---|---|
| An object the cache holds | The cache read, and no request |
| A directory or a prefix | The walk a run would do, or the listing seam the source already offers |
| A zip over a source serving ranges | The end of central directory record, the zip64 one behind it where the classic fields state a sentinel, the central directory, and the local header of each member, which is checked against it |
| A zip over a source refusing ranges | The whole object |
| A tar under any compression | The whole object, because a tar states no index |

Where the cheap path is not available, the run emits a `degrade` naming what was requested, what it cost instead and why, before the bytes move. It then proceeds rather than refusing: the caller asked what is inside, a refusal would need a flag to override it and there is no such flag, and a refusal that cannot be overridden is a question with no answer. What it read is kept in the cache, so the second listing of one object costs nothing.

`list` is bounded by the listing limits rather than the archive limits, and an archive past them fails while it is being enumerated rather than after it has been buffered.

Output is stable and machine readable: one entry per member with its path, its size, its entry type, and the digest where one is known. An archive states no digest for its members, so that field is absent there and present for a listing that states one.

## The library

The library is one directory holding materialized datasets, separate from the cache. Its default is the platform's data location, not the cache location, and `--library-dir`, `FETCHLOOM_LIBRARY_DIR` and `library = { dir = "..." }` move it in that order.

An entry's path is `<library>/<sanitized name>/<identity>`. The identity is derived from what the reference resolved to and from nothing about this machine: the manifest digest, the release, and the selection, folded under a key of their own so a value from one domain can never be mistaken for another. `where <ref>` therefore answers without an index to consult, two versions of one dataset coexist, and two runs of one reference land in one place.

A name is a convenience component and is sanitized deterministically: every byte outside letters, digits, `-`, `_` and `.` becomes `_`, trailing dots and spaces are dropped, an empty result becomes `dataset`, and a name whose stem is a Windows device is prefixed with `_`. Two names that sanitize identically stay separate, because the identity component differs.

A library file is never a hard link to a cache object. It is a copy-on-write clone where the volume offers one and a plain copy where it does not, with a `degrade` naming the refusal. One in-place edit through a hard link would corrupt the content addressed store for every dataset sharing that object.

A library tree is a destination like any other. It carries a record, so `status`, `diff`, `revert`, `promote` and `verify` work against it unchanged, and a second run into it reconciles rather than rebuilds.

Nothing is removed from the library on its own, and no run prunes it. `library rm <path>` removes one entry and the record of the run that wrote it, and refuses a path outside the library. Inside is decided after `.` and `..` are resolved, so a path that climbs out and back in is refused rather than followed. `library ls` states what the library holds and what it takes.

`--library` and `--output` together are refused, because two destinations is not a run.

## Selection

Selection is part of identity. Changing it changes the lock entry, not the dataset name. Selection is a sequence rather than a set: a locked run compares the patterns in the order they were written, so reordering a list is a change, and a lock made before the reorder no longer describes the run.

Globs match the canonical `/` separated member path, on raw bytes, case sensitive, with no normalization. Four rules and nothing else. `*` matches any run of bytes within one component, including none. `**` as a whole component matches any number of components, including none. `?` matches exactly one byte within one component. Every other byte is literal, including the bracket, the brace and the backslash.

A pattern matches member paths, not subtrees. Every ancestor directory of a selected member is included whether or not a pattern matched it.

No pattern at all selects every member. Exclusion is applied to what inclusion chose, so a member an exclude names is gone however many includes matched it.

A pattern is decided in time bounded by the pattern and the path, never by the number of ways its wildcards could line up, so a pattern of many recursive wildcards answers rather than running.

An empty selection is an error, not a no-op. It fails with `reference.unresolved` naming how many members were considered and which patterns matched none.

Under `--layout flatten:<n>` a file left with no path fails with `destination.unrepresentable` naming the member and the count. A directory left with no path names the destination itself and is dropped, because the directories surviving members need are synthesized whether or not the container declared them. One logical tree therefore flattens the same way whether or not its container wrote directory entries.

## Trust classes

Mechanical definitions. No other meaning is implied.

| Class | Condition |
|---|---|
| `verified` | A digest computed from the bytes matched one of the same algorithm supplied before this run, by the manifest or the lock |
| `corroborated` | No prior digest, but the observed digest matches at least two independent recorded witnesses. Contracted and unreachable in this build |
| `tofu` | No prior digest and no witnesses. The observed digest is recorded for future runs |
| `unverified` | Content could not be digested, or the user disabled verification |

Either algorithm satisfies `verified`, because a publisher's SHA-256 checked against the SHA-256 of the bytes is the same evidence as a publisher's BLAKE3 checked against theirs. A BLAKE3 that was stated and does not match fails inside the store, which names objects by that digest. A SHA-256 that was stated and does not match fails where the run compares what it got against what was claimed, because the store names an object by the digest of its own bytes and a SHA-256 claim names no address. Either way the run fails with `integrity.mismatch` and the destination is not published.

A digest a provider's listing states is a prior of the same standing as one a manifest states. A digest in an algorithm this build does not compute is not a prior at all: it is not carried, and the trust class says `tofu` rather than implying evidence the run cannot recheck.

`tofu` is allowed by default for a first fetch and never for a locked run. `unverified` requires an explicit flag on every invocation.

Publisher identity, manifest authenticity and content integrity are recorded as three separate facts. One never implies another.

### Witnesses

A witness is one recorded observation that an artifact hashed to a digest. It holds the digest observed, the machine that observed it, the origin that served the bytes, the run that recorded it, and when.

A witness is written only by a run that transferred the bytes in full and verified them as they arrived. A cache hit writes none, because it observed nothing. Nothing read from a source, bundle, lock, receipt or plan ever becomes a witness, so no remote party can manufacture one.

Two witnesses are independent only when they differ in all three of machine, origin and run. Two observations from one machine are one observation.

Witnesses are kept per artifact of a manifest, under a key derived from the manifest digest and the artifact identifier with each length stated, so no two names can fold into one key and no artifact reads another's witnesses.

`corroborated` is in the taxonomy and nothing in this build produces it. Every witness is written under this machine, and the one channel a foreign witness could arrive by is a shared cache directory, which is reached over a network and refused with `cache.locking_unsupported` before it is opened. A build that makes it reachable changes this paragraph in the same change.

## Locked runs

`--locked` holds a run to what the lock states.

| Field | Compared | Difference |
|---|---|---|
| dataset present in the lock | before the run | `policy.trust_refused`, exit 40 |
| `manifest`, `release` | before the run | `alias.unstable`, exit 10 |
| `select`, `layout` | before the run | `alias.unstable`, exit 10 |
| artifact present in both | after resolution | `alias.unstable`, exit 10 |
| `digest`, `interop`, `size` | during transfer | `integrity.mismatch`, exit 30 |
| `tree` | after materialization | `integrity.mismatch`, exit 30 |

Everything compared before the run is compared before a byte moves, so a run the lock does not describe publishes nothing.

A locked run never accepts `tofu`. A dataset the lock does not pin would be a first use, which is why it is refused on policy rather than resolved.

A locked run hands the transfer the digest the lock pins, so a cache already holding those bytes issues no request at all, and a source serving other bytes fails on integrity without falling back. It writes no lock, because a run that may not differ from the lock has nothing to add to it.

An unlocked run records what it resolved, including a run that changed nothing: the lock is rewritten with the bytes it already held, so what it says never depends on whether the destination needed touching. A run that resolved no object writes no lock entry and emits `degrade` naming which of two reasons it was: the reference resolved to a tree, which is what a directory and a container image do, or no interop digest is recorded for the object. A run that failed writes no lock entry and emits no `degrade`, because it gave nothing up that the failure it already reports does not say.

## Resume

The rung used is always reported.

| Rung | Condition | Behavior |
|---|---|---|
| 1 | Outboard tree known for the expected digest | Verify what is on disk by range, resume from the first bad or missing chunk |
| 2 | Immutable content address or provider version identity | Resume, full verify at completion |
| 3 | Strong ETag unchanged | Resume, full verify at completion |
| 4 | Weak validator only | Resume, full verify at completion, quarantine on mismatch |
| 5 | No validator | Restart from zero and report why |

A recorded identity differing from the current response is answered by the rung the partial stood on. On rungs three, four and five the partial is discarded, the transfer restarts, and a `degrade` names the rung it stood on and the rung it fell to. On rung two the source declared an immutable identity and has since served a different one for the same location, which contradicts the promise that rung stands on, so the transfer fails with `source.identity_changed`, is not retried, and exits 20. A source that merely stopped stating an immutable identity has broken no promise and restarts like any other rung.

A partial is named by what it is a transfer of. Where the digest is known it is named by that, and where it is not it is named by the location, the host and the identity the source stated, so a transfer whose identity moved appends to nothing the earlier one wrote. A partial carries a record beside it holding the redacted location, host, stated length, stated identity, entity tag and last modified as received, whether ranges were accepted, how many bytes are known to have arrived, and the rung. It is written when the partial is opened and removed with it.

A partial is preallocated to its full length, so its size on disk says nothing about how much arrived. The recorded byte count is the only offset a resume may append at, and anything past it is discarded first. A count behind what arrived costs a refetch; one ahead would be corruption, so it is only written after the bytes are.

`If-Range` is sent only on rung three and carries only a strong entity tag. A range answered `200` rather than `206` means the server ignored it, so the partial is discarded and the rung it fell to is reported. A `416` is answered once by rereading the length and remaking the request; a second is terminal.

Over FTP the rung is four. `MDTM` is a weak validator, `REST` states the offset the transfer restarts at, and a server whose `FEAT` does not offer `REST` reports that it accepts no ranges, so a partial is never appended to at an offset the server would ignore.

## Verification

| Setting | On a cache hit |
|---|---|
| `--verify always` | Reread every byte. An object with an outboard tree is walked against it, naming damaged ranges rather than only the object |
| `--verify fingerprint` | Trust the object if the recorded filesystem fingerprint matches. Default |
| `--verify never` | Trust it unconditionally. Result trust class becomes `unverified` |

`always` reads the same bytes either way. What the tree buys is not fewer bytes, it is a failure that names ranges, which is the difference between an object that must be fetched again and one that can be repaired.

`never` promises nothing about a cache hit and is not a claim the bytes are right or wrong. It does not verify at all, which is why the result is `unverified`. It never disables verification during transfer.

A fingerprint is volume identifier, file identifier, size, modification time and change time. It is a cache of the phrase probably unchanged. It is never evidence of content and never appears in a lock.

Verification during transfer is always on and is not configurable.

A reference no digest pins names bytes that may change, so a warm run asks with one conditional request built from the recorded validator. `304` reuses what the cache holds and leaves the trust class unchanged, because the source restated a validator and said nothing new about the bytes. `200` means the bytes changed and the response is the transfer. Anything else follows the retry classification. A run with no recorded validator cannot ask and transfers. A run whose digest is pinned asks nothing, because the cache holding those bytes is already the answer.

## Retry and politeness

Politeness is measured per host, and the host of a location is the name or address it carries: a bracketed literal is the address inside the brackets, a userinfo component belongs to no host, and a location naming none has no host to be polite to.

A transient failure is retried with exponential backoff and full jitter, up to the attempt limit and never past the retry ceiling.

`Retry-After` is a floor on the wait, never a replacement. The wait is the longer of the run's own backoff and what the source asked for, because politeness is never lowered by what a measurement says. A `Retry-After` longer than the ceiling is not waited out: the source is left for the next candidate, and a run with none left fails with `network.status` reporting the wait asked for.

Sources are ordered in the manifest, and order expresses preference, not a race. The same object is never transferred from more than one source at a time. Candidates are probed in parallel up to the probe limit, a probe being a bounded metadata request costing kilobytes.

Scored in fixed priority: reachable, supports ranges, exposes immutable identity, recorded throughput, time to first byte, egress cost, remaining politeness headroom. Ties break by manifest order, so selection is deterministic when measurements are equal. A losing candidate is never asked for bytes.

A fact a source did not state is not evidence about it. A candidate silent about egress cost is neither preferred over one that stated a charge nor refused for its silence, and the pair falls through to whatever separates them next.

A transfer moves to the next candidate when it stalls past the idle timeout, exhausts retries, returns a terminal error, or sustains throughput far below what was measured. Verified bytes are kept and the resume rung is recomputed. A switch emits `source.failover` and a `degrade` naming the source left, the source taken, and the failure that ended the first.

Selection may change speed. It may never change bytes. That is arithmetic rather than a promise: the digest is taken over the bytes and not over the location, so any mirror serving those bytes produces the same digest and is correct, and a mirror serving different bytes fails with `integrity.mismatch` exactly as one source would. The lock records the digest, so a rerun is exact whichever mirror answers it. Nothing about which mirror was taken can change what the user gets, which is why choosing the fastest one needs no further justification.

A candidate is served by whichever adapter states it serves it, decided per candidate. A mirror list may therefore name a location and a provider identifier for the same bytes, and the run falls through from one to the other.

Choosing costs nothing when there is nothing to choose. A single source is taken without a probe. A list is probed up to the probe limit, in parallel, each probe a bounded metadata request, so the cost of choosing is one round trip rather than one per candidate. Beyond the probe limit the remaining candidates keep manifest order and are never probed.

## Splitting

One object is fetched as several ranges at once only when four conditions hold together: the source states an immutable identity, it serves ranges, the object is longer than the split threshold, and this run has recorded a per host concurrency above one for that host. An object whose length the source did not state is not longer than the threshold, because a length nobody stated is not a length. The width is that count, bounded by what politeness permits at the moment.

Spans cover the missing bytes once, in order, with no gap and no overlap, and are written in the order they cover, so the digests taken as bytes arrive are the digests of the object. A source serving one span under a different identity than another fails with `source.identity_changed`.

A split that was wanted and did not happen emits `degrade` naming the condition that failed. An object at or below the threshold wants no split and reports nothing.

Splitting may change timing. It may never change bytes or digests.

## Cache

```
<cache>/
  objects/     completed immutable objects above the pack threshold
  packs/       objects at or below it, each preceded by its content digest,
               interop digest and length, so a pack states what it holds
  outboard/    chunk trees for objects above the outboard threshold
  partial/     in progress transfers with recorded source identity
  staging/     extraction trees not yet published
  quarantine/  objects that failed verification, with a diagnosis beside each
  receipts/    one receipt per materialized destination
  meta/        resolution metadata, per host measurements, witnesses, and the
               longest path each volume accepted, recorded against the boot
               that measured it
  locks/       advisory single writer locks
  pins/        pin records
  format       cache format fingerprint
```

Every object the cache holds has been fully verified. There is no other way for one to appear. A compressed object is verified against its content digest over its uncompressed bytes, because that is what the digest names.

An object lives in one of two placements decided by its size alone, and exactly one lookup answers where: a caller asks the cache for an object and is given its bytes, never a path it opens itself. A pack is self describing, so it is the only authority on what it holds and no index beside it can disagree.

### Compression

Compression is a storage decision. It changes what is on disk and never what anything hashes to, so no digest, lock, receipt, plan or bundle manifest differs because of it.

`--compress` decides what is written. `none` stores every object raw. `zstd:n` stores every object at that level. `auto`, the default, decides per object by compressing its first 1 MiB at level 1, both as it stands and byte shuffled at stride 4, and storing the object raw when neither measurement reaches 1.10. The decision comes from the bytes and never from a file name, an extension or a media type, and the shuffle is taken only when it measured better than not shuffling on that object's own head.

A compressed object is written as zstd frames of exactly one outboard chunk group of input each, the last one short, followed by the table of their compressed lengths. A range is read by decompressing only the frames covering it, which is why the frame size is the chunk group rather than a number of its own: a ranged verify or a localized repair pays for the bytes it asked for and no others.

Which form an object is in is stated by where it is filed, `objects/<hex>` raw and `objects/<hex>.z` compressed, and inside a pack by that entry's own header. It is never inferred from an object's bytes, because an object's bytes are arbitrary and a guess about them would eventually be wrong. A name carrying the compressed suffix is not a digest and is never read as one.

A partial is always raw, because a resume appends at a byte offset and verifies by range against the outboard tree. An object is therefore compressed when it is published rather than as it arrives.

Compression changes when a failure surfaces, never what anything hashes to. Decoding is not verifying, so an object whose stored form no longer decodes is refused even under a verification policy that would otherwise serve it unread, and it is refused as corruption rather than served as bytes. An object that cannot be decoded is not the bytes its digest names, so `cache verify` quarantines it rather than failing the run, and quarantine keeps it exactly as it was stored, because bytes that cannot be decoded are still the only ones a localized repair has.

Compression that was asked for and did not happen emits `degrade` naming the object, what was asked, and what was measured. Two cases: the probe found the object was not worth compressing, and the volume already compresses what is written to it. The probe reports its decision for an object that gets a file of its own, and not for one appended to a pack. A packed object is at or below one frame and most of them are too small to compress at all, so reporting each one would put a line in every run for a decision nobody can act on, and a run that degrades nothing would become impossible to have.

A pack belongs to the process and boot that writes it and is only appended to by that writer, so two writers never contend for one pack. Compaction is the one other writer, and it rewrites a pack whole under the same lock that removing a packed object already takes, never appending to one. An entry is committed by its bytes reaching the pack; an entry whose length runs past the end was cut short by a crash and is not one the cache holds. Removing a packed object rewrites its pack without it, under a lock, because a tombstone would be a second authority on what a pack holds.

Publication is write to `partial/`, flush according to the durability tier, then atomic rename. Renames are same volume only; a cross volume rename is an error, never a copy.

The three tiers promise three different things and none of them promises less than a torn object cannot appear, which is a property of the rename and of the pack format rather than of any flush.

`strict` is durable per object. Every object is pushed to the volume before it is published, and a pack entry is pushed as it is appended. A caller that reads an object back and then loses power still has it.

`normal`, the default, is durable per run. Every object of its own is pushed before it is published, exactly as under `strict`. A pack is pushed once, for every entry appended to it, before anything durable names what it holds: the receipt, or the manifest a promote writes. So an object that was appended to a pack and read back mid run can be lost to power failure before the run ends, and after the run ends it cannot. This is the one place the tiers differ in what they promise rather than only in what they cost.

`fast` is not durable. No flush is issued at all, so an object can be lost to power failure whether or not the run finished.

Under every tier a crash leaves a pack whose last entry may be short, and an entry whose length runs past the end of the pack is not one the cache holds. Nothing is ever served from bytes that are not there.

One writer per digest, held by an advisory lock recording machine, process, boot and start time. Liveness is decided by those values, never by modification time. A second process wanting an object being written waits and reuses the result rather than starting a second transfer.

The lock exists to deduplicate transfer, so it is taken when there is a transfer to deduplicate and not otherwise. Bytes already local, whether from a `file:` source, an archive being extracted, or a bundle member, are written to a name of this process's own and published with no lock and no partial. A content addressed write is idempotent and a rename already makes a torn object impossible, so coordinating two processes writing identical bytes costs more than it saves. This is a rule about whether bytes cross a network, never about how many of them there are, and it never becomes a size threshold.

Orphaned staging and partial entries from a previous boot are removed at startup. An entry another machine created is not this machine's to recover and is left where it is.

Prune marks, waits out a grace period, then sweeps. Pinned objects, objects leased by a running process, and objects referenced by a lock in the working directory always survive. The grace period is a race window, not a retention policy: it exists so an object claimed between mark and sweep is not removed under the process claiming it. It is not configurable, because shorter is a corruption and longer is a wait with no benefit. Retention, meaning a rule about how long an unused object is kept, does not exist.

A mismatched object moves to `quarantine/` rather than being deleted, because leaving it in `objects/` would break the invariant and deleting it would discard the bytes a localized repair needs. Quarantined objects are never served, are reported by `cache status`, and are removed only by prune or clear. Every quarantine writes a diagnosis beside the object holding what was found, the ranges that failed, and the command that fetches those bytes again.

A cache is refused outright on a volume reached over a network, with `cache.locking_unsupported`, because advisory locks there cannot be trusted across the machines sharing it. There is no conservative lock path. The refusal is the answer. A destination on such a volume is not refused.

A cache directory may be used by several users at once and Fetchloom never assumes otherwise, so there is one behavior rather than two and no way to select the unsafe one by mistake. Objects are readable by every user and writable only by their creator. Prune removes only objects the invoking user created and reports what it skipped.

If `format` does not match the running binary, every cache operation fails with instructions to run `cache clear`. There is no migration. `cache clear` is the one command the check does not apply to, because removing a directory does not depend on what wrote it.

A cache that is missing, read only, or out of space does not stop a run. It emits `degrade` and continues in `--no-cache` behavior, which is a scratch store beside the destination rather than no store at all. A format mismatch does stop it, because the user can always fix that with one command, and continuing would silently refetch everything the unusable cache already held.

### Compaction

`cache compact` rewrites packs. It reads every object a pack holds, trains one zstd dictionary over them, recompresses each against that dictionary at a higher level than the append path uses, and writes the dictionary into the pack it belongs to. It is a maintenance command and is never on the fetch path, so the level it compresses at is not bound by the rate the append path has to keep up with. A frame decompresses at the same rate whatever level wrote it, so nothing a read does gets slower.

A dictionary belongs to exactly one pack and is stored in it. No dictionary is shared between packs, because a pack is already the unit that prune, repair and compaction rewrite and forget whole, and a shared dictionary would be a thing that outlives the pack that needs it. Objects above the pack threshold are stored loose, carry no dictionary, and need none: an object that fills a frame already has the context a dictionary would have supplied.

Compaction respects `--compress`. Under `none` it rewrites packs without compressing them and trains no dictionary, because a run that was told never to compress does not get compressed packs from a maintenance command.

A pack whose objects cannot train a dictionary is rewritten without one and emits `degrade` naming what was wanted and why it did not happen. Too few objects and too little total content are ordinary outcomes rather than errors.

A pack records the BLAKE3 of the dictionary it holds beside it, and refuses a dictionary that no longer hashes to it. Every other byte the cache serves is covered by a content digest; a dictionary is not content addressed, so without its own digest a damaged one decompresses into plausible bytes that are silently wrong. A damaged dictionary loses every object in its pack, where a damaged frame loses one object. That is a real loss of failure granularity and it is accepted deliberately: a pack holds only objects at or below the pack threshold, which are small and can be fetched again. A pack whose dictionary no longer reads is reported as corrupt naming the pack and the command that rebuilds it, never as a single missing object.

### Repair

`repair <ref>` resolves the reference to a digest exactly as `get` does, finds which ranges do not match the outboard tree, and fetches those and no others.

Localization is a claim about which bytes are wrong, and a tree is derived data that could itself be wrong, so it is never the last word. A repair rewrites the named ranges, then rereads the whole object and hashes it. It enters `objects/` only when it hashes to the digest it is named by. A repair whose localization was wrong fails loudly rather than publishing bytes that were never checked whole.

| Condition | Behavior |
|---|---|
| No tree stored | Build one from the object's own bytes and localize against it |
| The tree does not check out | Discard it, refetch whole, rebuild, emit `degrade` |
| Adjacent damaged groups | Merged into one span so one request serves them |
| More spans than the limit | Fetch whole, emit `degrade` naming both counts |
| Damage above the whole refetch share | Fetch whole, emit `degrade` naming both sizes |
| The source cannot serve a range | Fetch whole, emit `degrade` |
| Nothing damaged | Report `unchanged`, issue no request |

## Materialization

Canonical entry stream for the tree digest. Entries sorted by raw path bytes. Fields are determined by type, and a field that does not apply is absent, not empty.

| Type | Fields |
|---|---|
| File | path, type, mode, size, content |
| Directory | path, type |
| Symlink | path, type, target |

Path is the entry path with `/` separators and no normalization. Paths must be valid UTF-8; an entry whose path is not is rejected, because a tree that cannot be named identically on both platforms cannot be reproduced on them.

Mode is `0644` or `0755` only, taken from the source archive or manifest rather than from a destination stat, so a platform that cannot represent an executable bit still produces the same tree digest. A bare filesystem tree states no mode, so every file found by walking one is `0644` on every platform and the run reports with `degrade` that it read no mode.

Content is the BLAKE3 of the file bytes. Target is the symlink target bytes. Every directory is an entry, including one holding only other directories.

Timestamps are excluded. Ownership, ACLs, extended attributes and alternate data streams are excluded and their presence in an archive is an error.

### Rejected during extraction

Every rejection stops the whole run, emits `extract.reject`, and publishes nothing. No member is ever skipped: an archive holding one rejected member cannot be fetched, and excluding that member with `--select` does not change it.

| Rejected | Kind |
|---|---|
| Absolute member path | `archive.unsafe_path` |
| A `..` component anywhere | `archive.unsafe_path` |
| A backslash or drive letter in the path | `archive.unsafe_path` |
| A zip whose paths hold both a forward slash and a backslash | `archive.unsafe_path` |
| A path that is not valid UTF-8, or holds a NUL | `archive.unsafe_path` |
| A path longer or deeper than allowed | `archive.unsafe_path` |
| A link target resolving outside the destination | `archive.link_escape` |
| A hard link to a member the archive does not hold | `archive.link_escape` |
| A device, FIFO or socket entry | `archive.unsupported` |
| A setuid or setgid bit | `archive.unsupported` |
| Ownership, an ACL, an xattr, or an alternate data stream | `archive.unsupported` |
| Two members on one path | `archive.collision` |
| Two members colliding under the volume's folding or normalization | `archive.collision` |
| A local header disagreeing with the central directory | `archive.unsafe_path` or `archive.unsupported` |
| A zip64 locator pointing at bytes that are not a zip64 end of central directory record | `archive.unsupported` |
| A zip compression method that is not store or deflate | `archive.unsupported` |
| A container or compression this build does not carry | `archive.unsupported` |
| A header the format does not permit, or a truncated archive | `archive.unsupported` |
| More members, expanded bytes, or ratio than allowed | `archive.bomb` |
| A name the target volume refuses | `destination.unrepresentable` |

A hard link to a member the archive does hold materializes the target's bytes as
a second file. The tree entry types are file, directory and symbolic link, so a
hard link has no entry of its own, and two names sharing an inode is a fact about
one filesystem rather than about the bytes a run delivered. Two entries, one
digest, and a tree digest that says the same thing on a filesystem that has no
hard links at all.

The normalization half of that collision row is unprovable on the platforms this
project ships. It needs a volume that stores a normalized form of the name it is
given, and neither NTFS nor any Linux filesystem the verify lanes can build does
that, so no lane observes the rejection and the suite says so by name rather than
passing in silence. The row stands because a normalizing volume exists elsewhere,
and the code measures the volume rather than assuming an answer.

A backslash is a legal byte in a member name and is never a separator, with one exception decided from the archive itself. When no member path in a zip holds a forward slash and at least one holds a backslash, that zip states its structure with backslashes and nothing else, so every backslash becomes a forward slash before any rejection is applied, and `..\..\x` is refused as `../../x` rather than accepted as a name. The run emits `degrade`. A zip holding both is ambiguous and refused. A tar is never translated.

A zip states how many members it holds in sixteen bits and where its central directory starts in thirty-two. Where a true value does not fit, the format writes a sentinel into that field and the real one into a zip64 end of central directory record, found through a locator sitting immediately before the classic record. Fetchloom reads that record whenever either field states its sentinel and a locator is there, so the count and the location every later decision is made from are the archive's own rather than a truncation of them. An archive holding exactly 65,535 members states the same sixteen bits and carries no locator, which is not zip64 and is read as it stands. A locator pointing at bytes that are not a zip64 end of central directory record, or at one shorter than the format's smallest, is refused naming zip64. A member stating its own sizes in zip64 form is still refused, because reading those is a different thing from finding the directory.

Collisions are found by creating each entry exclusively in staging, so the target filesystem's own folding decides rather than a table Fetchloom would have to keep correct.

Staging for a materialization lives on the destination volume, so publication is a rename rather than a copy. Replacing an existing destination renames the old tree aside first, which leaves the destination briefly absent but never partial, and leaves the previous tree recoverable until the new one is published.

Copy on write clone is attempted first and falls back to a byte copy. The fallback emits a `degrade` naming the volume that refused, and that volume is not asked again in the same run. A clone that succeeded degrades nothing, because nothing was lowered.

## Reconcile

Running a request against an existing destination produces one outcome per entry, decided against the tree the run resolved. A receipt is a cached copy of that answer, never a second authority for it.

| Outcome | Condition | Action |
|---|---|---|
| `unchanged` | Fingerprint or hash matches the resolved entry | Nothing |
| `restored` | Entry missing | Materialize from cache |
| `modified` | Entry differs from the resolved entry | Stop, name every path, require `--force` or `--adopt` |
| `foreign` | Present, not in the resolved tree | Stop and name it, require `--force` or `--adopt` |

An entry the destination holds as another kind of thing than the one that was resolved is `modified`, because what an entry is belongs to the entry.

Which answer `unchanged` is decided by comes from `--verify`, so one policy governs a destination entry and a cache hit rather than two. A destination with no receipt has no recorded fingerprint, so every file is hashed. A fingerprint answers only what the record it was written with states about that path, so it stands for that record's entry and never for the tree this run resolved. That is what keeps `--adopt` from letting the next run report a tree the destination does not hold.

Every entry unchanged writes nothing: no staging, no rename, status `unchanged`, exit 0. Entries missing and nothing modified or foreign builds only the missing entries and publishes them one at a time. Anything modified or foreign stops the run before staging and exits 60.

Numbered directories are never created. A destination is never partially reconciled: it is never left holding a tree that is neither the one it held nor the one that was resolved.

## The record

A run into a destination writes one record beside the cache, keyed by that destination. It states two trees and the filesystem facts that make comparing them cheap.

| Field | Holds |
|---|---|
| `entries` | The canonical entry stream of what was materialized, which is what `tree` digests |
| `resolved` | The canonical entry stream of what the reference resolved to, absent when it is the same |
| `fingerprints` | Volume, file, size, modification and change time, per file, as they stood after publication |
| `artifacts` | Per artifact, the digest, where it came from, its trust class, and the name, length, selection, layout and archive format that decide where its members land |

The two trees differ only where a run kept your version of an entry over upstream's. `entries` is what the destination holds, so it is what a fingerprint answers about and what `verify <path>` folds. `resolved` is what upstream last gave, so it is the merge base and what `status`, `diff` and `revert` compare against.

A record naming no entry is not a record. Every command that works against one refuses with `reference.unresolved` naming the destination rather than comparing against nothing.

## Status

`status <path>` and `diff <path>` decide one state per entry, against `resolved` in the record. Both are read only: they change no entry in the destination and issue no request.

| State | Condition |
|---|---|
| `unchanged` | The destination holds what the record states |
| `modified` | It holds something else |
| `deleted` | The record states it and the destination does not hold it |
| `added` | The destination holds it and the record does not state it |

There is no fifth state and no summary beyond these. An entry that is unchanged is not printed, so a destination that is exactly what the run left prints nothing at all. `diff` prints the same states and adds what the record states and what the destination holds, as a digest and a length. Neither ever compares the inside of a file.

A file is answered by its recorded fingerprint when `--verify` is not `always` and that fingerprint still matches; otherwise its bytes are read. That is a decision about cost and never about correctness: a fingerprint that matches stands for the digest the record holds for that path, and any difference in volume, file identifier, length, modification time or change time reads the bytes again.

A fingerprint is recorded only for a file whose modification and change times are already behind the instant the run began recording them, so a file written while the record was being taken carries no fingerprint and is always read. One window remains and is not closed by anything short of hashing: a file rewritten to the same length within the same filesystem timestamp tick as the run's own write, before the record was taken. `--verify always` is the answer for a caller who cannot accept that window.

## Three way

`get` against a destination that has a record, when what the reference resolves to now differs from the `resolved` tree that record holds, is a three way compare. The record is the merge base, the destination is your side, and what the reference resolves to is upstream. The comparison is per entry and never per line: a parquet file and a JPEG have no lines, so an entry is the smallest thing that can differ.

| Base | You | Upstream | Outcome |
|---|---|---|---|
| present | unchanged | changed | Take upstream |
| present | changed | unchanged | Keep yours |
| present | changed | changed, differently | Conflict |
| present | changed | changed, identically | Unchanged, and nothing is written |
| present | deleted | unchanged | Stays deleted |
| present | deleted | changed | Conflict |
| present | deleted | deleted | Stays deleted |
| present | unchanged | deleted | Take upstream, which removes it |
| present | changed | deleted | Conflict |
| absent | added | absent | Keep yours |
| absent | absent | added | Take upstream |
| absent | added | added, differently | Conflict |
| absent | added | added, identically | Unchanged |

An entry is one value, so a change of what it is is a change like any other: a path that is a file on one side and a directory on the other has moved on that side, and a mode upstream changed is upstream changing that entry.

A conflict writes upstream's version beside yours as `<name>.upstream`, leaves yours exactly as it is, names every conflicting path, and exits 60 with `destination.conflict`. Nothing merges the contents of a file, nothing prompts, and nothing chooses for you. A run whose `<name>.upstream` would land on a name either side already holds fails with `destination.conflict` before anything is written, because there is no second name and a numbered one would be a guess.

A conflicted run still writes its record, so the next run compares against what this one left rather than conflicting again on the same entry.

An entry missing from the destination is taken to be a deletion you made, because the record cannot tell a deliberate deletion from a file that vanished and deletion is a change like any other. The other reading is reachable: `--force` rebuilds the destination as upstream states it, and `revert <path> <entry>` puts one entry back. In a two way run, where upstream has not moved, a missing entry is `restored` as it always was, because a run that has nothing new to give has nothing to do but put back what it wrote.

Every one of these publishes the way the rest of the product does. The merged tree is built whole in staging beside the destination and published by rename, so a run killed halfway leaves the old tree or the new one and never a half merged one.

## Revert

`revert <path>` restores the entries the record names, and `revert <path> <entry>...` restores only those. An entry you modified is rewritten, one you deleted comes back, and one you added is removed. Every other entry is left exactly as it is.

The bytes come from the cache and never from the network, so a revert offline is a revert. An entry the cache no longer holds fails with `cache.corrupt` naming the object and the command that would bring it back; it never quietly refetches. A symbolic link the record names by its target's digest rather than by the text of the target cannot be restored from the record alone, and fails naming it.

Revert publishes the way a three way run does: whole, by rename, old tree or new tree.

Revert writes no record. What it restored is what the record already stated, and what it left alone is still yours.

## Promote

`promote <path>` makes the destination as it stands a dataset of its own. It reads every file, keeps the bytes in the cache, and writes a manifest naming one artifact per file with both digests, plus a lock pinning them.

Ingestion happens at promote and at no other time. A run never copies your edits into the cache as you make them, because the bytes are already on disk and the cost belongs at the moment someone asks for it.

The manifest states `derived_from`: the dataset, the manifest digest and the tree digest the record names. A promoted dataset that forgets what it was derived from is worth less than one that remembers.

Promote pins bytes; it does not publish them. The manifest names each artifact by a path relative to the tree, and an artifact whose stated digest the cache already holds resolves from the cache without that path existing at all. `cache export` is how the objects reach another machine.

Promote refuses a destination with no record, because a directory nothing wrote is what `init` describes.

## Partial success

Artifacts are independent. Objects that verified stay in the cache and are reused next run.

The destination is all or nothing. If any selected artifact fails, nothing is published and the previous destination is untouched. The result reports each artifact separately with its own status and error.

A run where one artifact of several failed records every artifact that verified in the lock and no `tree`, because a lock without a tree still pins bytes. It writes no receipt at all, because a receipt records what a run materialized and nothing was.

## Documents and their bounds

A document is bounded by what wrote it, not by which parser reads it.

A manifest, listing, lock, plan and metadata document are written by strangers, so they are bounded to refuse hostile input by the manifest size and node limits. A receipt is written by this machine about work it already did, and its length is a function of how many files the run materialized rather than of anything a stranger controls, so it is bounded by the record limits, set to hold a tree of the largest size the archive entry limit permits.

A document past either bound is refused with `resource.limit` and is never read in part, wherever it came from. A document nested deeper than the nesting depth allows is refused as `manifest.invalid` naming the depth, because a document shaped to exhaust a reader is malformed rather than large. Bounding a receipt by the manifest limits would let this build materialize a tree it cannot verify, which is the one thing a receipt exists to prevent.

## Credentials

A credential has one of two shapes. A bearer credential is one opaque value that is sent. A signing credential is an access key, secret key, region and optional session token, and its secret is never sent: it derives a key that signs a canonical request. Which shape a host needs is decided by the adapter serving it, never by the user.

Lookup order per host: the matching environment variable, then the provider's own environment variable where the provider defines one, then the platform credential store, then the provider's own helper. First match wins and the source is reported without the secret. There is one credential path and a provider's own variable is a second place it is read from, never a second path: a user who already set the variable the provider's own tools read should not have to restate it. The host-named variable holds the whole `Authorization` header, so a header other than `Bearer` can be sent. A provider's own variable holds the bare token that provider prints, and is sent as `Bearer` followed by it, because the provider defines the contents of its own variable. A signing credential found without a region fails `policy.credential_invalid` naming the region, because a guessed region produces a refusal the user cannot act on.

A credential is bound to the host it was resolved for. It is dropped on any redirect to a different host, and the drop is reported.

Never written anywhere: bearer tokens, API keys, passwords, `Authorization` and `Cookie` headers, the userinfo component of a URL, and the value of every query parameter. Query values are redacted as a class rather than by a list of known sensitive names, because a list is a thing that can be incomplete. Redaction applies identically to logs, events, receipts, plans and error messages, replaced with the fixed text `[redacted]`.

Fetchloom never asks at startup and never as setup. A credential is requested only at the moment it changes the outcome, and the request states that outcome. Required means no reachable source can serve the request without it, so the run stops with `policy.credential_missing` and prints the provider's setup steps. Optional means a source needing one scored better than every reachable alternative, so both options are named with the measured difference and the run proceeds with the alternative if the user declines. An optional credential is offered only when the difference exceeds the offer threshold.

Non interactive runs never block. A required credential fails immediately with the steps on stderr. An optional one is reported as an unused opportunity and the alternative is used.

Every provider ships one fixed record and Fetchloom never improvises the wording: the name the user recognizes, what it unlocks stated concretely, whether it is required, numbered steps naming the page to open and the button to press, where to put the value, the command that confirms it works, and the narrowest permissions that suffice. Steps assume no prior knowledge of the provider, of tokens, or of the terminal.

Terms are never accepted automatically. If a manifest records `requires_acceptance`, Fetchloom refuses to transfer until the user asserts acceptance, records that the assertion was made, and makes no legal determination about what it means.

## Listing

A reference naming a container is expanded by listing it. Fetchloom lists. It does not crawl.

Supported: object store listing APIs, provider repository and record APIs, WebDAV `PROPFIND`, standard generated HTML indexes, and FTP directories.

An FTP directory is listed with `MLSD`, which states each member's type and size as fields rather than as a line meant for a person. A server that refuses `MLSD` is listed with `LIST` and a `degrade` names the fallback, because reading a name and a size out of an `ls` line is guesswork where a machine-readable listing is not. A listing is walked into the directories it names, and a member name that is absolute, holds a separator, or is `.` or `..` is refused with `reference.unresolved`, because a listing is a stranger's document.

Only entries at or below the given prefix are considered. Links pointing outside the prefix are ignored and counted in the result. Nothing is discovered from the contents of files. No script is executed. Entry count is bounded. An index that is not recognized fails with `reference.unresolved` and is never guessed at.

A provider repository or record API is described once by an endpoint and a field mapping, and every provider reached that way is one description against that shape rather than one adapter of its own. A description states where the files are in the answer, which field names each one, which field locates it or how a location is built when the provider states none, which field sizes it, and which field states a digest.

A member path a listing states is refused with `reference.unresolved` when it is absolute, escapes the record with `..`, or is not one path under the record. A listing is a stranger's document and is bounded and checked as one.

A provider whose reference names the host it reaches carries that host as the first segment of the reference, over HTTPS and with no way to write anything else, so one description reaches every installation. One description therefore serves data.gov.uk and every other CKAN, and Harvard and every other Dataverse.

A DOI names a registration, and a registration names a landing page rather than files, so a DOI is routed and never fetched. The registration is read once from DataCite, and the landing page it states decides which provider holds the record. The reference the router answers with is one an adapter of this build already serves, and it is reported as a resolution alias so the run says what it decided. A DOI resolving to a provider no adapter serves fails with `reference.unresolved`, naming the registrant, where the DOI resolves, and that serving it needs a description of that provider's own listing API. It is never guessed at.

Routing reads the landing page rather than the registrant identity, because a registrant enumerates who paid for the prefix, one per installation, where the landing page names the installation that holds the record, which is the thing a multiplier's reference has to carry.

A file inside a record is named by the record's reference and the path the listing gave it. The listing remembers where each of its own members is fetched from, so naming one costs no further request. A reference that names a file without the listing having been read is resolved by reading the record it names.

## FTP

FTP is spoken here rather than taken as a dependency. `USER` and `PASS` with anonymous as the default, `TYPE I`, `PASV`, `SIZE`, `MDTM`, `REST` then `RETR`, `MLSD` with a `LIST` fallback, and `QUIT`. Nothing else is sent.

The data connection is passive and is never active, so no run listens for an inbound connection. A `PASV` reply names a host and a port. The port is used and the host is not: the data connection is opened to the address the control connection is already talking to, and a reply naming any other host is refused with `network.refused` naming both addresses. A server that could redirect a client's data connection to a third party is the bounce attack, and it is refused by name rather than by luck.

`ftps://` secures the control connection with explicit `AUTH TLS`, then `PBSZ 0` and `PROT P` so the data connection is secured too. There is no flag that turns that off, and no implicit FTPS on port 990, which is deprecated. A server that refuses `AUTH TLS` fails an `ftps://` reference with `network.tls`, and no user name or password is sent to it.

`ftp://` attempts `AUTH TLS` first and falls back to the clear when it is refused, emitting a `degrade` that says every command and every byte travels unencrypted. A credential resolved for the host is never sent over a control connection that could not be secured: the run fails with `policy.credential_invalid` naming `ftps://` as the way to send it, because a password in the clear is worse than a run that did not happen.

A bearer credential for an FTP host is read as `user:password`, and a value with no separator is the user name with an empty password. `SIZE` and `MDTM` are the probe. `MDTM` is a weak validator, so an FTP resume stands on rung four: `REST` states the offset and the whole object is verified at completion.

An FTP source states no checksum, so a first fetch is `tofu` and a locked run compares the digest the lock pins.

SFTP is not spoken, and is refused as a scheme this build does not serve rather than being attempted over FTP.

## Output

| Stream | Content |
|---|---|
| stdout | Final result only. JSON under `--json`, otherwise nothing for machine consumption |
| stderr | Progress, logs, prompts, diagnostics |

Progress is written only when stderr is a terminal. Prompts appear only when stdin and stderr are both terminals. Otherwise a required prompt is a policy failure.

An operation with nothing to do exits 0 with status `unchanged`.

The result's `status` is one of exactly four values and no other is ever written: `materialized`, `unchanged`, `restored`, `adopted`.

The JSON result carries a `work` object holding `bytes_read`, `bytes_written`, `requests` and `file_operations`. Bytes read and written count content only, so on a run into an empty destination and empty cache that stored every object raw, bytes written is exactly what the run left on disk. A run that compressed an object wrote it twice, once to the partial as it arrived and once compressed as it was published, and both are counted, because compressing an object is moving content rather than bookkeeping about it. A packed object is written twice for the same reason, once to the partial and once into the pack. What a pack states about itself, its preamble and the header before each entry, is bookkeeping about content rather than content, and is not counted. An object stored raw and unpacked is counted once and is exactly what it left. Neither counts the cache's own records, its fingerprint, its locks, or the receipt: those are bookkeeping about a run rather than the content it moved. Requests counts every request including retries and probes. File operations counts every file or directory created, every rename, and every flush, bookkeeping included. All four are identical on identical inputs, which is what a benchmark gates on. None is a duration.

### The JSON a command prints

Other tools parse this, so the field names are a contract rather than a convenience. They are additive only: a field may be added, and one that is present is never renamed, retyped or removed. There is no version field anywhere in this build, so a rename is a break with nothing to negotiate it, and the field names of every result are asserted by a test rather than by care.

| Command | Object |
|---|---|
| `get`, `apply` | `status`, `dataset`, `tree`, `destination`, `entries`, `bytes`, `trust`, `work` |
| `probe` | `dataset`, `artifacts[]` of `artifact`, `location`, `size`, `digests[]` of `algorithm` and `value`, `trust`, `ranges`, `cached` |
| `list` | `entries[]` of `path`, `type`, `size`, and `digest` where one is known; `skipped` |
| `where` | `dataset`, `path`, `held` |
| `library ls` | `entries[]` of `dataset`, `path`, `held`; `bytes` |
| `library rm` | `path`, `bytes` |
| `status` | `entries[]` of `path` and `state` |
| `diff` | `entries[]` of `path`, `state`, `record`, `found` |
| `revert` | `restored`, `path` |
| `promote` | `dataset`, `artifacts`, `lock`, `derived_from` |
| `verify` | `status`, `tree`, `entries`, `path` |

`work` holds `bytes_read`, `bytes_written`, `requests` and `file_operations`. A field whose value is unknown is absent rather than null, and a field whose value is empty is written empty.

A run that failed prints the error object instead, on stdout, and the process exit code is what the kind says it is.

Terminal output is dense, aligned and quiet. It is not a user interface. One accent color, and beyond it color carries meaning only. Color is never the only way a fact is conveyed. `NO_COLOR` and `--color` are honored and all styling is stripped when the stream is not a terminal. An explicit `--color always` wins over `NO_COLOR`, because a flag on this invocation is a narrower instruction than an environment the shell was started with. One progress renderer for the whole run, aggregated, redrawn at a fixed rate, never one indicator per file. A value the source did not supply is shown as `?` and never estimated to make a line look complete. No emoji, no box drawing, no full screen mode.

A hint is one line about the run that just happened, naming something the user could do differently. It qualifies only if it could have changed this run. At most one per run, printed after the result, never during transfer, never repeated to the same user, and suppressed rather than repeated when nothing can record that it was said. Hints go to stderr only and never appear in the JSON result or the event stream.

The live view is a consumer of the event stream and has no other input. It cannot display a fact the stream does not carry, cannot influence the run, and can be removed without changing the engine. No display mode changes bytes, digests, tree digests, exit codes or the machine readable result.

## Platform capabilities

Detected per destination and per cache volume, reported in plans and results: case sensitivity, normalization behavior, clone support, sparse file support, symlink permission, hard link support, maximum path length, whether the volume is network backed, and whether an on access scanner inspects writes.

A volume's capability answer is decided once. The first detection decides it and every later question in the same run gets that answer, so two callers asking at the same time are never given different ones. Case folding and normalization are measured per directory, so an answer carries the pair measured for the directory asked about.

A capability the platform reports is queried. One it does not report is probed inside Fetchloom's own staging directory, never by writing into the user's destination. A probe never uses a fixed name, because an already exists result is how a probe reads the filesystem's answer and another probe's file would be read as that answer.

The longest path a volume accepts is measured by building one, which costs more than the run it precedes, so a cache records the answer against the volume that gave it and the boot that measured it. A run reaching a cache whose record names this boot and this volume reads that answer instead of measuring again; a record from any other boot is refused and replaced. The probe stays the authority, and nothing is assumed from the platform's name or from a machine wide setting, because what a volume accepts is a property of the volume and of the running executable rather than of Windows or Linux. A cache is what holds the record, so a run with no cache measures.

An on access scanner is reported, never worked around.

| Answer | Carries | Given when |
|---|---|---|
| present | The product's name and the cost ratio | The platform enumerated and one of them is a scanner |
| absent | Nothing | The platform enumerated and none is, or it cannot enumerate and small writes cost no more than the ratio |
| unknown | The cost ratio | The platform cannot enumerate and small writes cost more |

The cost ratio is how many times longer writing many small files took than writing the same bytes to one file. It is a cost, never a detection: a volume simply slow at small writes measures the same as one behind a scanner, so present is never reported from the ratio alone.

Normalization is measured on the target volume, never inferred from the platform: sensitive, insensitive preserving, normalizing, or unknown when the volume refused the probe name.

Processor capabilities are detected the same way: the thread budget actually available after affinity, container and job limits, the vector level chosen for the content digest, and whether hardware acceleration is present and usable for the interop digest. A target where that acceleration exists but cannot be detected emits `degrade` rather than claiming it. A thread ceiling requested above the detected budget is clamped and the clamp is reported.

## Write path

`--io buffered` writes through the operating system page cache. `--io uncached` asks it to release written bytes once they are durable. A platform with no way to do that without constraining every write to sector alignment uses buffered and emits `degrade`.

`--io auto` chooses from the volume's capability answers, never from the platform name. It chooses `uncached` only on a volume whose backing is local and whose scanner answer is absent, on a platform that can release written pages, and `buffered` otherwise with no `degrade`, because `auto` requested nothing in particular.

The write path never changes what is written. Both modes produce the same bytes, digests and tree digest.

## Cancellation

First interrupt: stop new work and exit 130. Second: abort immediately.

Cache invariants hold either way, because nothing enters `objects/` without a completed verification and an atomic rename, so an interrupted run leaves either a valid object or a resumable partial.

## Offline

`--offline` forbids DNS resolution, connection attempts, source probes, remote credential lookups and update checks. Any operation requiring one fails with `policy.offline` before doing avoidable work. Plans produced offline mark every network derived field unknown.

A reference is taken to require the network unless it names something this machine already holds, so a reference form added later is refused offline until it is shown to be local rather than permitted until someone remembers to name it. The refusal is decided once before a reference is resolved and enforced again where a request would be issued, so no adapter can reach the network by resolving to a location the first decision never saw.

## Bundles

A bundle carries objects between machines that do not trust each other. It is a tar whose every member is named by the lowercase hex of the content digest of its own bytes. A tar has no index, so there is nothing in a bundle that could be trusted instead of its bytes.

The tar is compressed whole under `--compress`, at the level it names, and `none` writes it uncompressed. Which one a bundle is is decided when it is read from its own first bytes: a tar opens with a member name, which here is the hexadecimal of a digest, so it can never open with a compression frame magic and the two are never confused. Compression changes nothing a bundle states. Every member still hashes to its own name, import still derives every digest from the bytes it reads, a member name is still never used as a path, and a bundle that fails anywhere still publishes nothing.

Import derives every digest from the bytes it reads. A member name is a claim and never an instruction: it is compared against what the bytes hash to and never used as a path. Every member is staged and checked before anything is published, so a bundle that fails anywhere publishes nothing.

| Input | Failure |
|---|---|
| A member named like a path | `archive.unsafe_path` |
| A member named anything but a digest | `cache.corrupt` |
| A header that does not check out | `cache.corrupt` |
| Bytes that do not hash to the name | `integrity.mismatch` |
| A bundle that ends early | `integrity.truncated` |
| A compressed bundle whose frames do not decompress | `cache.corrupt` |

## Manifests

Accepted as YAML, TOML or JSON, all parsing into one model. The digest covers the canonical JSON form, so reformatting does not change identity.

```yaml
name: silesia
release: "2024-06"
artifacts:
  - id: corpus
    sources:
      - https://host/silesia.tar.zst
      - https://mirror/silesia.tar.zst
    size: 68132864
    digest:
      blake3: "..."
      sha256: "..."
    media_type: application/zstd
    archive:
      format: tar+zstd
    select: ["**/*.txt"]
    layout: keep
license:
  spdx: CC-BY-4.0
  requires_acceptance: false
```

`name` and at least one artifact are required. `sources` is ordered, earlier entries preferred and later ones failover only. `digest` may be absent, which forces a weak trust class. Unknown top level keys are an error.

No key may express a command, a script, or a path to execute. Manifests are declarative. No shell, no hooks, no generators. That is what makes it safe to point Fetchloom at a manifest written by someone you have never met.

Both digests a manifest states are compared to the bytes, `blake3` to the content digest and `sha256` to the interop digest, both taken in the pass that reads the bytes. Either difference fails `integrity.mismatch` naming the algorithm, what was stated and what was found. A manifest stating one and not the other is compared on the one it stated.

## Disk accounting

Four requirements computed separately: partial transfer bytes, cache object bytes, extraction staging bytes, destination bytes. Each attributed to its volume, requirements on a shared volume summed and checked against that volume. Insufficient space fails before transfer begins.

## The surface is not a placeholder

A command, flag or value exists in the binary only once it performs what is written here. There is no state in which something is present and unable to act, because that is a placeholder and because a caller cannot distinguish it from a usage error.

Anything in this document not yet in the binary is listed under Not built in [reference.md](reference.md).

# contracts

what fetchloom promises. if a behavior is not written here it is not defined and
nothing should rely on it. this is the specification every change implements
exactly. for why a promise is what it is, see [internals.md](internals.md). for
the surface as it stands, see [reference.md](reference.md).

before 1.0 there is one format and no compatibility code. from 1.0 the portable
set below is readable forever.

## compatibility

two categories with different obligations.

portable artifacts travel between machines, people and time: manifests, locks,
receipts, plans, bundles, the command surface, flag names, exit codes, JSON
result keys, event names. from 1.0 changes to these are additive only. a field
is never removed, renamed, or given a new meaning. a removed capability leaves
its field readable and reports it unsupported.

derived data carries no obligation: cache objects, outboard trees, staging, per
host measurements, resolution metadata. all of it regenerates from portable
artifacts and sources, so a format change discards it rather than migrating it.
the cache format fingerprint makes that discard explicit and loud.

four decisions make that cheap, and all four hold from the first commit.
identity is content addressed, so a digest computed today is valid forever.
surface syntax is separate from the canonical model, so new syntax never changes
identity. digest and tree algorithms are domain separated and named where they
are used, so a future algorithm is an addition rather than a reinterpretation.
unknown keys are refused before 1.0 and reserved after, and the `x-` prefix is
reserved now, which keeps the strict to extensible move a one way door.

## identity

| term | meaning |
|---|---|
| content digest | BLAKE3 over the object bytes. the cache key and the resume authority |
| interop digest | SHA-256 over the same bytes. recorded to match publisher claims. never the cache key |
| outboard tree | stored BLAKE3 chunk tree. enables range verification and localized repair |
| tree digest | BLAKE3 over the canonical entry stream of a materialized directory |
| manifest digest | BLAKE3 over the canonical JSON form of a manifest, never over its source text |

written as `blake3:<hex>` or `sha256:<hex>`. both are computed on every
transfer, in one pass over the bytes.

three digests are domain separated by derived key so a value from one can never
be mistaken for another: object content, tree digest, manifest digest.

an object at or below the outboard threshold stores no tree, because its content
digest already authenticates it whole. a tree is derived: the content digest is
the only authority, and a tree that disagrees with it is discarded and rebuilt
rather than believed.

## canonical form

every portable artifact has one canonical form, and every digest covers that
form rather than the text anyone typed.

a timestamp is written and read in one form: the date, `T`, the time to the
second, and `Z`. nothing else is written and nothing else is read as one.

a manifest is accepted as YAML, TOML or JSON. a lock, receipt and plan are
written in the YAML subset and read back from any of the three.

the YAML subset is block mappings, block sequences, flow sequences, flow
mappings, and plain, single quoted and double quoted scalars. anchors, aliases,
merge keys, tags, directives, block scalars, document separators, and tabs as
indentation are each refused by name. `true`, `false` and `null` are the only
words read as anything but text, and a run of digits with an optional sign is
the only text read as a number, so `yes` is a word.

canonical JSON is what every digest covers. mappings ordered by the raw bytes of
their keys, absent values omitted rather than written null, no insignificant
whitespace and no line ending anywhere, so the bytes are identical on every
platform.

the canonical text form written to a file uses the same ordering, writes every
scalar as a double quoted JSON string, writes a mapping key plain when it is
alphanumeric with underscores, hyphens and dots and quoted otherwise, indents by
two spaces, and ends every line with one line feed on every platform. a carriage
return before a line feed is read and never written. a byte order mark is read
and never written.

## references

resolution order is deterministic: explicit scheme, then a local path if it
exists, then each configured source in order. a name matching none of the
configured sources fails. it is never guessed at.

a name with no source configured is searched for across every registry that
offers search, run in parallel, and a registry that fails is a registry that
found nothing rather than a run that failed. exactly one record carrying the
name resolves to it. several refuse, naming each with its size, where it came
from, what it states about its bytes, and the command that would take it. none
fails, naming the nearest names it did find. there is no fourth outcome and
nothing is chosen for the user.

a name is never written to a lock. what it resolved to is, so the first run
searches and every run after is exact. writing the name would trade pinning away
to save typing, and a lock exists to say what was fetched rather than what was
asked for.

a name matches a record when both fold to the same letters and digits, so case
and punctuation do not separate them. a record within three edits of the term is
suggested and never resolved to, because a near name is a question and not an
answer.

a local path is read as a manifest when its extension is one a manifest is
written in, and as data otherwise. nothing else decides it, and a directory is
never searched for one.

a reference naming no manifest resolves to a synthesized one: the dataset name,
and one artifact carrying that name and, when the reference names a network
location, that location. a local path is never recorded in it, so its digest is
the same on every machine holding the same data.

a reference naming one object materializes a directory holding that one entry
under the object's own name. a reference naming a container materializes every
entry it holds. the two differ only in what is walked, never in what a
destination is.

nothing there fails with `reference.unresolved` saying nothing is there.
something there that cannot be read fails with `reference.unresolved` saying to
make it readable. the two are never reported as each other.

## the project file

`fetchloom.toml` is found by walking up from the working directory to the root
of the volume, and the first one found is the one used. there is one project
file, one syntax and one discovery rule. `--config` names one instead and
`--no-config` uses none.

every relative path that file states resolves against the directory holding it,
and never against the working directory. that covers the `datasets` table's
destinations and the cache and library directories it names. the lock a run
writes lands beside it too, unless `--lock` names somewhere else. a run from a
subdirectory therefore writes what a run from the top writes, which is the
difference between a project file that is usable from a script and one that is
not.

`get` with no reference fetches every entry of the `datasets` table, in the
order the table sorts. an entry is either a reference or a table stating `ref`
and optionally `output`, `select`, `exclude` and `layout`. a string is never
read as a table and a table always states `ref`, so one shape never means the
other. an unknown key inside an entry is an error naming the key, as every other
key in the file is.

an entry with no `output` lands at the entry's own name beside the project file.
two entries that would write to one destination fail with exit 2 naming both,
before anything is resolved or fetched, and two spellings of one path are one
destination. every entry of the table is read and checked before the first is
fetched, so a value no run can take fails with nothing fetched rather than after
the entries before it landed. a destination this platform cannot name and a
`layout` that is neither `keep` nor `flatten:<n>` are both such values.

a dataset that fails does not stop the ones after it, because each is its own
destination and each publishes whole or not at all. the run exits with the code
of the first failure.

`get` with no reference and `--output`, `--select`, `--exclude`, `--layout` or
`--library` is refused, because one destination and one selection cannot
describe several datasets and the table already states each. `--locked` is the
reproducible install: every dataset pinned exactly, refusing where resolution
differs.

## probe and list

`probe <ref>` resolves the reference, asks the source, and moves no payload
bytes. it reports the resolved location redacted, the size, every digest the
source states with the algorithm each one is, the trust class a fetch would land
in, whether the source serves ranges, and whether the cache already holds the
object. a fact the source does not state is unknown, unknown is an answer, and
probe exits 0. under `--offline` it answers from what the cache holds, or fails
with `policy.offline`.

the trust class a probe reports is `verified` when a digest a run could compare
against is already stated, and `tofu` otherwise. it never reports
`corroborated`, because corroboration is a fact about witnesses of bytes this
machine has seen and a probe has seen none.

`list <ref>` enumerates what a container holds, moving as few bytes as the
format allows.

| what is listed | what it costs |
|---|---|
| an object the cache holds | the cache read, and no request |
| a directory or a prefix | the walk a run would do, or the listing seam the source already offers |
| a zip over a source serving ranges | the end of central directory record, the zip64 one behind it where the classic fields state a sentinel, the central directory, and the local header of each member, which is checked against it |
| a zip over a source refusing ranges | the whole object |
| a tar under any compression | the whole object, because a tar states no index |

where the cheap path is not available, the run emits a `degrade` naming what was
requested, what it cost instead and why, before the bytes move. it then proceeds
rather than refusing: the caller asked what is inside, a refusal would need a
flag to override it and there is no such flag, and a refusal that cannot be
overridden is a question with no answer. what it read is kept in the cache, so
the second listing of one object costs nothing.

`list` is bounded by the listing limits rather than the archive limits, and an
archive past them fails while it is being enumerated rather than after it has
been buffered.

output is stable and machine readable: one entry per member with its path, its
size, its entry type, and the digest where one is known. an archive states no
digest for its members, so that field is absent there and present for a listing
that states one.

## the library

the library is one directory holding materialized datasets, separate from the
cache. its default is the platform's data location, not the cache location, and
`--library-dir`, `FETCHLOOM_LIBRARY_DIR` and `library = { dir = "..." }` move it
in that order.

an entry's path is `<library>/<sanitized name>/<identity>`. the identity is
derived from what the reference resolved to and from nothing about this machine:
the manifest digest, the release, and the selection, folded under a key of their
own so a value from one domain can never be mistaken for another. `where <ref>`
therefore answers without an index to consult, two versions of one dataset
coexist, and two runs of one reference land in one place.

a name is a convenience component and is sanitized deterministically: every
character outside ASCII letters, digits, `-`, `_` and `.` becomes one `_`,
trailing dots and spaces are dropped, an empty result becomes `dataset`, and a
name whose stem is a Windows device is prefixed with `_`. two names that
sanitize identically stay separate, because the identity component differs.

a library file is never a hard link to a cache object. it is a copy-on-write
clone where the volume offers one and a plain copy where it does not, with a
`degrade` naming the refusal. one in-place edit through a hard link would
corrupt the content addressed store for every dataset sharing that object.

a library tree is a destination like any other. it carries a record, so
`status`, `diff`, `revert`, `promote` and `verify` work against it unchanged,
and a second run into it reconciles rather than rebuilds.

nothing is removed from the library on its own, and no run prunes it. `library
rm <path>` removes one entry and the record of the run that wrote it, and
refuses a path outside the library. inside is decided after `.` and `..` are
resolved, so a path that climbs out and back in is refused rather than followed.
`library ls` states what the library holds and what it takes.

`--library` and `--output` together are refused, because two destinations is not
a run.

## selection

selection is part of identity. changing it changes the lock entry, not the
dataset name. selection is a sequence rather than a set: a locked run compares
the patterns in the order they were written, so reordering a list is a change,
and a lock made before the reorder no longer describes the run.

globs match the canonical `/` separated member path, on raw bytes, case
sensitive, with no normalization. four rules and nothing else. `*` matches any
run of bytes within one component, including none. `**` as a whole component
matches any number of components, including none. `?` matches exactly one byte
within one component. every other byte is literal, including the bracket, the
brace and the backslash.

a pattern matches member paths, not subtrees. every ancestor directory of a
selected member is included whether or not a pattern matched it.

no pattern at all selects every member. exclusion is applied to what inclusion
chose, so a member an exclude names is gone however many includes matched it.

a pattern is decided in time bounded by the pattern and the path, never by the
number of ways its wildcards could line up, so a pattern of many recursive
wildcards answers rather than running.

an empty selection is an error, not a no-op. it fails with
`reference.unresolved` naming how many members were considered and which
patterns matched none.

under `--layout flatten:<n>` a file left with no path fails with
`destination.unrepresentable` naming the member and the count. a directory left
with no path names the destination itself and is dropped, because the
directories surviving members need are synthesized whether or not the container
declared them. one logical tree therefore flattens the same way whether or not
its container wrote directory entries.

## trust classes

mechanical definitions. no other meaning is implied.

| class | condition |
|---|---|
| `verified` | a digest computed from the bytes matched one of the same algorithm supplied before this run, by the manifest or the lock |
| `corroborated` | no prior digest, but the observed digest matches at least two independent recorded witnesses. contracted and unreachable in this build |
| `tofu` | no prior digest and no witnesses. the observed digest is recorded for future runs |
| `unverified` | content could not be digested, or the user disabled verification |

either algorithm satisfies `verified`, because a publisher's SHA-256 checked
against the SHA-256 of the bytes is the same evidence as a publisher's BLAKE3
checked against theirs. a BLAKE3 that was stated and does not match fails inside
the store, which names objects by that digest. a SHA-256 that was stated and
does not match fails where the run compares what it got against what was
claimed, because the store names an object by the digest of its own bytes and a
SHA-256 claim names no address. either way the run fails with
`integrity.mismatch` and the destination is not published.

a digest a provider's listing states is a prior of the same standing as one a
manifest states. a digest in an algorithm this build does not compute is not a
prior at all: it is not carried, and the trust class says `tofu` rather than
implying evidence the run cannot recheck.

`tofu` is allowed by default for a first fetch and never for a locked run.
`unverified` requires an explicit flag on every invocation.

publisher identity, manifest authenticity and content integrity are recorded as
three separate facts. one never implies another.

### witnesses

a witness is one recorded observation that an artifact hashed to a digest. it
holds the digest observed, the machine that observed it, the origin that served
the bytes, the run that recorded it, and when.

a witness is written only by a run that transferred the bytes in full and
verified them as they arrived. a cache hit writes none, because it observed
nothing. nothing read from a source, bundle, lock, receipt or plan ever becomes
a witness, so no remote party can manufacture one.

two witnesses are independent only when they differ in all three of machine,
origin and run. two observations from one machine are one observation.

witnesses are kept per artifact of a manifest, under a key derived from the
manifest digest and the artifact identifier with each length stated, so no two
names can fold into one key and no artifact reads another's witnesses.

`corroborated` is in the taxonomy and nothing in this build produces it. every
witness is written under this machine, and the one channel a foreign witness
could arrive by is a shared cache directory, which is reached over a network and
refused with `cache.locking_unsupported` before it is opened. a build that makes
it reachable changes this paragraph in the same change.

## locked runs

`--locked` holds a run to what the lock states.

| field | compared | difference |
|---|---|---|
| dataset present in the lock | before the run | `policy.trust_refused`, exit 40 |
| `manifest`, `release` | before the run | `alias.unstable`, exit 10 |
| `select`, `layout` | before the run | `alias.unstable`, exit 10 |
| artifact present in both | after resolution | `alias.unstable`, exit 10 |
| `digest`, `interop`, `size` | during transfer | `integrity.mismatch`, exit 30 |
| `tree` | after materialization | `integrity.mismatch`, exit 30 |

everything compared before the run is compared before a byte moves, so a run the
lock does not describe publishes nothing.

a locked run never accepts `tofu`. a dataset the lock does not pin would be a
first use, which is why it is refused on policy rather than resolved.

a locked run hands the transfer the digest the lock pins, so a cache already
holding those bytes issues no request at all, and a source serving other bytes
fails on integrity without falling back. it writes no lock, because a run that
may not differ from the lock has nothing to add to it.

an unlocked run records what it resolved, including a run that changed nothing:
the lock is rewritten with the bytes it already held, so what it says never
depends on whether the destination needed touching. a run that resolved no
object writes no lock entry and emits `degrade` naming which of two reasons it
was: the reference resolved to a tree, which is what a directory and a container
image do, or no interop digest is recorded for the object. a run that failed
writes no lock entry and emits no `degrade`, because it gave nothing up that the
failure it already reports does not say.

`plan` states resolved digests and moves no bytes to learn one, so a reference
the lock does not pin is refused with `policy.trust_refused` exactly as a locked
run refuses it.

## resume

the rung used is always reported.

| rung | condition | behavior |
|---|---|---|
| 1 | outboard tree known for the expected digest | verify what is on disk by range, resume from the first bad or missing chunk |
| 2 | immutable content address or provider version identity | resume, full verify at completion |
| 3 | strong ETag unchanged | resume, full verify at completion |
| 4 | weak validator only | resume, full verify at completion, quarantine on mismatch |
| 5 | no validator | restart from zero and report why |

a recorded identity differing from the current response is answered by the rung
the partial stood on. on rungs three, four and five the partial is discarded,
the transfer restarts, and a `degrade` names the rung it stood on and the rung
it fell to. on rung two the source declared an immutable identity and has since
served a different one for the same location, which contradicts the promise that
rung stands on, so the transfer fails with `source.identity_changed`, is not
retried, and exits 20. a source that merely stopped stating an immutable
identity has broken no promise and restarts like any other rung.

a partial is named by what it is a transfer of. where the digest is known it is
named by that, and where it is not it is named by the location, the host and the
identity the source stated, so a transfer whose identity moved appends to
nothing the earlier one wrote. a partial carries a record beside it holding the
redacted location, host, stated length, stated identity, entity tag and last
modified as received, whether ranges were accepted, how many bytes are known to
have arrived, and the rung. it is written when the partial is opened and removed
with it.

a partial is preallocated to its full length, so its size on disk says nothing
about how much arrived. the recorded byte count is the only offset a resume may
append at, and anything past it is discarded first. a count behind what arrived
costs a refetch. one ahead would be corruption, so it is only written after the
bytes are.

`If-Range` is sent only on rung three and carries only a strong entity tag. a
range answered `200` rather than `206` means the server ignored it, so the
partial is discarded and the rung it fell to is reported. a `416` is answered
once by rereading the length and remaking the request. a second is terminal.

over FTP the rung is four. `MDTM` is a weak validator, `REST` states the offset
the transfer restarts at, and a server whose `FEAT` does not offer `REST`
reports that it accepts no ranges, so a partial is never appended to at an
offset the server would ignore.

## verification

| setting | on a cache hit |
|---|---|
| `--verify always` | reread every byte. an object with an outboard tree is walked against it, naming damaged ranges rather than only the object |
| `--verify fingerprint` | trust the object if the recorded filesystem fingerprint matches. default |
| `--verify never` | trust it unconditionally. result trust class becomes `unverified` |

`always` reads the same bytes either way. what the tree buys is not fewer bytes,
it is a failure that names ranges, which is the difference between an object
that must be fetched again and one that can be repaired.

`never` promises nothing about a cache hit and is not a claim the bytes are
right or wrong. it does not verify at all, which is why the result is
`unverified`. it never disables verification during transfer.

a fingerprint is volume identifier, file identifier, size, modification time and
change time. it is a cache of the phrase probably unchanged. it is never
evidence of content and never appears in a lock.

verification during transfer is always on and is not configurable.

a reference no digest pins names bytes that may change, so a warm run asks with
one conditional request built from the recorded validator. `304` reuses what the
cache holds and leaves the trust class unchanged, because the source restated a
validator and said nothing new about the bytes. `200` means the bytes changed
and the response is the transfer. anything else follows the retry
classification. a run with no recorded validator cannot ask and transfers. a run
whose digest is pinned asks nothing, because the cache holding those bytes is
already the answer.

## retry and politeness

politeness is measured per host, and the host of a location is the name or
address it carries: a bracketed literal is the address inside the brackets, a
userinfo component belongs to no host, and a location naming none has no host to
be polite to.

every request carries `User-Agent: fetchloom/<version>` and nothing else about
the machine. the operators whose rate limits this section exists to honor ask
for one so they can tell one client from another and write to a maintainer
before they ban an address, and a tool that will not say who it is has no claim
on being treated politely. no operating system, no hostname, no architecture: a
name and a version is what identifies the client, and everything else is only
about the person running it.

a transient failure is retried with exponential backoff and full jitter, up to
the attempt limit and never past the retry ceiling.

`Retry-After` is a floor on the wait, never a replacement. the wait is the
longer of the run's own backoff and what the source asked for, because
politeness is never lowered by what a measurement says. a `Retry-After` longer
than the ceiling is not waited out: the source is left for the next candidate,
and a run with none left fails with `network.status` reporting the wait asked
for.

sources are ordered in the manifest, and order expresses preference, not a race.
the same object is never transferred from more than one source at a time.
candidates are probed in parallel up to the probe limit, a probe being a bounded
metadata request costing kilobytes.

scored in fixed priority: reachable, supports ranges, exposes immutable
identity, recorded throughput, time to first byte, egress cost, remaining
politeness headroom. ties break by manifest order, so selection is deterministic
when measurements are equal. a losing candidate is never asked for bytes.

a fact a source did not state is not evidence about it. a candidate silent about
egress cost is neither preferred over one that stated a charge nor refused for
its silence, and the pair falls through to whatever separates them next.

a transfer moves to the next candidate when it stalls past the idle timeout,
exhausts retries, returns a terminal error, or sustains throughput far below
what was measured. verified bytes are kept and the resume rung is recomputed. a
switch emits `source.failover` and a `degrade` naming the source left, the
source taken, and the failure that ended the first.

selection may change speed. it may never change bytes. that is arithmetic rather
than a promise: the digest is taken over the bytes and not over the location, so
any mirror serving those bytes produces the same digest and is correct, and a
mirror serving different bytes fails with `integrity.mismatch` exactly as one
source would. the lock records the digest, so a rerun is exact whichever mirror
answers it. nothing about which mirror was taken can change what the user gets,
which is why choosing the fastest one needs no further justification.

a candidate is served by whichever adapter states it serves it, decided per
candidate. a mirror list may therefore name a location and a provider identifier
for the same bytes, and the run falls through from one to the other.

choosing costs nothing when there is nothing to choose. a single source is taken
without a probe. a list is probed up to the probe limit, in parallel, each probe
a bounded metadata request, so the cost of choosing is one round trip rather
than one per candidate. beyond the probe limit the remaining candidates keep
manifest order and are never probed.

## splitting

one object is fetched as several ranges at once only when four conditions hold
together: the source states an immutable identity, it serves ranges, the object
is longer than the split threshold, and this run has recorded a per host
concurrency above one for that host. an object whose length the source did not
state is not longer than the threshold, because a length nobody stated is not a
length. the width is that count, bounded by what politeness permits at the
moment.

spans cover the missing bytes once, in order, with no gap and no overlap, and
are written in the order they cover, so the digests taken as bytes arrive are
the digests of the object. a source serving one span under a different identity
than another fails with `source.identity_changed`.

a split that was wanted and did not happen emits `degrade` naming the condition
that failed. an object at or below the threshold wants no split and reports
nothing.

splitting may change timing. it may never change bytes or digests.

## cache

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

every object the cache holds has been fully verified. there is no other way for
one to appear. a compressed object is verified against its content digest over
its uncompressed bytes, because that is what the digest names.

an object lives in one of two placements decided by its size alone, and exactly
one lookup answers where: a caller asks the cache for an object and is given its
bytes, never a path it opens itself. a pack is self describing, so it is the
only authority on what it holds and no index beside it can disagree.

### modes and who may write

on unix a cache is created either private or shared, and what decides it is the
directory the person put it in rather than a flag. a root whose parent no other
user may write, which is every default under a home directory, is created `0700`
along with every directory inside it. a root under a directory another user may
already write, which is what choosing `/srv` or `/tmp` says, is created `1777`
so any of them may add an entry and only its owner may remove it. a root that is
already there keeps whichever it already is, because whoever made it that way
meant it. objects are published `0444` and packs `0644` in both.

on windows a directory inherits its parent's access control entries and
fetchloom sets none of its own, so a cache under the per-user default is private
and a cache an administrator made for several users carries what that
administrator chose.

no write inside the cache opens a name that already exists. every one creates a
scratch file exclusively beside its target and renames it on, which fails on a
symlink rather than writing through it, and a name that is meant to be an empty
marker is created exclusively or found to be a plain file already. the sticky
bit was never the guarantee here: it stops one user removing another's entry and
says nothing about a name that does not exist yet.

a stat fingerprint proves an object was not changed since this run published it.
it proves nothing about an object another user could have written, because every
field in it is a field that user chose. on a shared cache an object this process
does not own is read and hashed rather than stat'd, whatever the verification
policy says, and `--verify always` is what a caller who cannot accept even the
owned case asks for.

### compression

compression is a storage decision. it changes what is on disk and never what
anything hashes to, so no digest, lock, receipt, plan or bundle manifest differs
because of it.

`--compress` decides what is written. `none` stores every object raw. `zstd:n`
stores every object at that level. `auto`, the default, decides per object by
compressing its first 1 MiB at level 1 once as it stands and once byte shuffled
at each of the strides the limits name, and storing the object raw when the best
of those measurements does not reach 1.10. the decision comes from the bytes and
never from a file name, an extension or a media type, and the shuffle is taken
only when it measured better than not shuffling on that object's own head.

a compressed object is written as zstd frames of exactly one outboard chunk
group of input each, the last one short, followed by the table of their
compressed lengths. a range is read by decompressing only the frames covering
it, which is why the frame size is the chunk group rather than a number of its
own: a ranged verify or a localized repair pays for the bytes it asked for and
no others.

which form an object is in is stated by where it is filed, `objects/<hex>` raw
and `objects/<hex>.z` compressed, and inside a pack by that entry's own header.
it is never inferred from an object's bytes, because an object's bytes are
arbitrary and a guess about them would eventually be wrong. a name carrying the
compressed suffix is not a digest and is never read as one.

a partial is always raw, because a resume appends at a byte offset and verifies
by range against the outboard tree. an object is therefore compressed when it is
published rather than as it arrives.

compression changes when a failure surfaces, never what anything hashes to.
decoding is not verifying, so an object whose stored form no longer decodes is
refused even under a verification policy that would otherwise serve it unread,
and it is refused as corruption rather than served as bytes. an object that
cannot be decoded is not the bytes its digest names, so `cache verify`
quarantines it rather than failing the run, and quarantine keeps it exactly as
it was stored, because bytes that cannot be decoded are still the only ones a
localized repair has.

compression that was asked for and did not happen emits `degrade` naming the
object, what was asked, and what was measured. two cases: the probe found the
object was not worth compressing, and the volume already compresses what is
written to it. the probe reports its decision for an object that gets a file of
its own, and not for one appended to a pack. a packed object is at or below one
frame and most of them are too small to compress at all, so reporting each one
would put a line in every run for a decision nobody can act on, and a run that
degrades nothing would become impossible to have.

a pack belongs to the process and boot that writes it and is only appended to by
that writer, so two writers never contend for one pack. compaction is the one
other writer, and it rewrites a pack whole under the same lock that removing a
packed object already takes, never appending to one. an entry is committed by
its bytes reaching the pack. an entry whose length runs past the end was cut
short by a crash and is not one the cache holds. removing a packed object
rewrites its pack without it, under a lock, because a tombstone would be a
second authority on what a pack holds.

publication is write to `partial/`, flush according to the durability tier, then
atomic rename. renames are same volume only. a cross volume rename is an error,
never a copy.

the three tiers promise three different things and none of them promises less
than a torn object cannot appear, which is a property of the rename and of the
pack format rather than of any flush.

`strict` is durable per object. every object is pushed to the volume before it
is published, and a pack entry is pushed as it is appended. a caller that reads
an object back and then loses power still has it.

`normal`, the default, is durable per run. every object of its own is pushed
before it is published, exactly as under `strict`. a pack is pushed once, for
every entry appended to it, before anything durable names what it holds: the
receipt, or the manifest a promote writes. so an object that was appended to a
pack and read back mid run can be lost to power failure before the run ends, and
after the run ends it cannot. this is the one place the tiers differ in what
they promise rather than only in what they cost.

`fast` is not durable. no flush is issued at all, so an object can be lost to
power failure whether or not the run finished.

under every tier a crash leaves a pack whose last entry may be short, and an
entry whose length runs past the end of the pack is not one the cache holds.
nothing is ever served from bytes that are not there.

one writer per digest, held by an advisory lock recording machine, process, boot
and start time. liveness is decided by those values, never by modification time.
a second process wanting an object being written waits and reuses the result
rather than starting a second transfer.

the lock exists to deduplicate transfer, so it is taken when there is a transfer
to deduplicate and not otherwise. bytes already local, whether from a `file:`
source, an archive being extracted, or a bundle member, are written to a name of
this process's own and published with no lock and no partial. a content
addressed write is idempotent and a rename already makes a torn object
impossible, so coordinating two processes writing identical bytes costs more
than it saves. this is a rule about whether bytes cross a network, never about
how many of them there are, and it never becomes a size threshold.

orphaned staging and partial entries from a previous boot are removed at
startup. an entry another machine created is not this machine's to recover and
is left where it is.

prune marks, waits out a grace period, then sweeps. pinned objects, objects
leased by a running process, and objects referenced by a lock in the working
directory always survive. the grace period is a race window, not a retention
policy: it exists so an object claimed between mark and sweep is not removed
under the process claiming it. it is not configurable, because shorter is a
corruption and longer is a wait with no benefit. retention, meaning a rule about
how long an unused object is kept, does not exist.

a mismatched object moves to `quarantine/` rather than being deleted, because
leaving it in `objects/` would break the invariant and deleting it would discard
the bytes a localized repair needs. quarantined objects are never served, are
reported by `cache status`, and are removed only by prune or clear. every
quarantine writes a diagnosis beside the object holding what was found, the
ranges that failed, and the command that fetches those bytes again.

a cache is refused outright on a volume reached over a network, with
`cache.locking_unsupported`, because advisory locks there cannot be trusted
across the machines sharing it. there is no conservative lock path. the refusal
is the answer. a destination on such a volume is not refused.

a cache directory may be used by several users at once and fetchloom never
assumes otherwise, so there is one behavior rather than two and no way to select
the unsafe one by mistake. objects are readable by every user and writable only
by their creator. prune removes only objects the invoking user created and
reports what it skipped.

if `format` does not match the running binary, every cache operation fails with
instructions to run `cache clear`. there is no migration. `cache clear` is the
one command the check does not apply to, because removing a directory does not
depend on what wrote it.

a cache that is missing, read only, or out of space does not stop a run. it
emits `degrade` and continues in `--no-cache` behavior, which is a scratch store
beside the destination rather than no store at all. a format mismatch does stop
it, because the user can always fix that with one command, and continuing would
silently refetch everything the unusable cache already held.

### compaction

`cache compact` rewrites packs. it reads every object a pack holds, trains one
zstd dictionary over them, recompresses each against that dictionary at a higher
level than the append path uses, and writes the dictionary into the pack it
belongs to. it is a maintenance command and is never on the fetch path, so the
level it compresses at is not bound by the rate the append path has to keep up
with. a frame decompresses at the same rate whatever level wrote it, so nothing
a read does gets slower.

a dictionary belongs to exactly one pack and is stored in it. no dictionary is
shared between packs, because a pack is already the unit that prune, repair and
compaction rewrite and forget whole, and a shared dictionary would be a thing
that outlives the pack that needs it. objects above the pack threshold are
stored loose, carry no dictionary, and need none: an object that fills a frame
already has the context a dictionary would have supplied.

compaction respects `--compress`. under `none` it rewrites packs without
compressing them and trains no dictionary, because a run that was told never to
compress does not get compressed packs from a maintenance command.

a pack whose objects cannot train a dictionary is rewritten without one and
emits `degrade` naming what was wanted and why it did not happen. too few
objects and too little total content are ordinary outcomes rather than errors.

a pack records the BLAKE3 of the dictionary it holds beside it, and refuses a
dictionary that no longer hashes to it. every other byte the cache serves is
covered by a content digest. a dictionary is not content addressed, so without
its own digest a damaged one decompresses into plausible bytes that are silently
wrong. a damaged dictionary loses every object in its pack, where a damaged
frame loses one object. that is a real loss of failure granularity and it is
accepted deliberately: a pack holds only objects at or below the pack threshold,
which are small and can be fetched again. a pack whose dictionary no longer
reads is reported as corrupt naming the pack and the command that rebuilds it,
never as a single missing object.

### repair

`repair <ref>` resolves the reference to a digest exactly as `get` does, finds
which ranges do not match the outboard tree, and fetches those and no others.

localization is a claim about which bytes are wrong, and a tree is derived data
that could itself be wrong, so it is never the last word. a repair rewrites the
named ranges, then rereads the whole object and hashes it. it enters `objects/`
only when it hashes to the digest it is named by. a repair whose localization
was wrong fails loudly rather than publishing bytes that were never checked
whole.

| condition | behavior |
|---|---|
| no tree stored | build one from the object's own bytes and localize against it |
| the tree does not check out | discard it, refetch whole, rebuild, emit `degrade` |
| adjacent damaged groups | merged into one span so one request serves them |
| more spans than the limit | fetch whole, emit `degrade` naming both counts |
| damage above the whole refetch share | fetch whole, emit `degrade` naming both sizes |
| the source cannot serve a range | fetch whole, emit `degrade` |
| nothing damaged | report `unchanged`, issue no request |

## materialization

canonical entry stream for the tree digest. entries sorted by raw path bytes.
fields are determined by type, and a field that does not apply is absent, not
empty.

| type | fields |
|---|---|
| file | path, type, mode, size, content |
| directory | path, type |
| symlink | path, type, target |

path is the entry path with `/` separators and no normalization. paths must be
valid UTF-8. an entry whose path is not is rejected, because a tree that cannot
be named identically on both platforms cannot be reproduced on them.

mode is `0644` or `0755` only, taken from the source archive or manifest rather
than from a destination stat, so a platform that cannot represent an executable
bit still produces the same tree digest. a bare filesystem tree states no mode,
so every file found by walking one is `0644` on every platform and the run
reports with `degrade` that it read no mode.

content is the BLAKE3 of the file bytes. target is the symlink target bytes.
every directory is an entry, including one holding only other directories.

timestamps are excluded. ownership, ACLs, extended attributes and alternate data
streams are excluded and their presence in an archive is an error.

### rejected during extraction

every rejection stops the whole run, emits `extract.reject`, and publishes
nothing. no member is ever skipped: an archive holding one rejected member
cannot be fetched, and excluding that member with `--select` does not change it.

| rejected | kind |
|---|---|
| absolute member path | `archive.unsafe_path` |
| a `..` component anywhere | `archive.unsafe_path` |
| a backslash or drive letter in the path | `archive.unsafe_path` |
| a zip whose paths hold both a forward slash and a backslash | `archive.unsafe_path` |
| a path that is not valid UTF-8, or holds a NUL | `archive.unsafe_path` |
| a path longer or deeper than allowed | `archive.unsafe_path` |
| a link target resolving outside the destination | `archive.link_escape` |
| a hard link to a member the archive does not hold | `archive.link_escape` |
| a device, FIFO or socket entry | `archive.unsupported` |
| a setuid or setgid bit | `archive.unsupported` |
| ownership, an ACL, an xattr, or an alternate data stream | `archive.unsupported` |
| two members on one path | `archive.collision` |
| two members colliding under the volume's folding or normalization | `archive.collision` |
| a local header disagreeing with the central directory | `archive.unsafe_path` or `archive.unsupported` |
| a zip64 locator pointing at bytes that are not a zip64 end of central directory record | `archive.unsupported` |
| a zip compression method that is not store or deflate | `archive.unsupported` |
| a container or compression this build does not carry | `archive.unsupported` |
| a header the format does not permit, or a truncated archive | `archive.unsupported` |
| more members, expanded bytes, or ratio than allowed | `archive.bomb` |
| a name the target volume refuses | `destination.unrepresentable` |
| a member producing more bytes than the size it declared | `archive.unsupported` |

a member's declared size is what the expansion guard is shown while the archive
is listed, so it is also what the member's stream is held to while its bytes
move. a compressed member that produces more than it declared stops at the byte
that passes the declaration, rather than after the bytes have landed. deflate
reaches about 1032 to 1, so the compressed size bounds nothing about the output
and the declaration is the only figure both the guard and the stream agree on.

a hard link to a member the archive does hold materializes the target's bytes as
a second file. the tree entry types are file, directory and symbolic link, so a
hard link has no entry of its own, and two names sharing an inode is a fact
about one filesystem rather than about the bytes a run delivered. two entries,
one digest, and a tree digest that says the same thing on a filesystem that has
no hard links at all.

the normalization half of that collision row is unprovable on the platforms this
project ships. it needs a volume that stores a normalized form of the name it is
given, and neither NTFS nor any Linux filesystem the verify lanes can build does
that, so no lane observes the rejection and the suite says so by name rather
than passing in silence. the row stands because a normalizing volume exists
elsewhere, and the code measures the volume rather than assuming an answer.

a backslash is a legal byte in a member name and is never a separator, with one
exception decided from the archive itself. when no member path in a zip holds a
forward slash and at least one holds a backslash, that zip states its structure
with backslashes and nothing else, so every backslash becomes a forward slash
before any rejection is applied, and `..\..\x` is refused as `../../x` rather
than accepted as a name. the run emits `degrade`. a zip holding both is
ambiguous and refused. a tar is never translated.

a zip states how many members it holds in sixteen bits and where its central
directory starts in thirty-two. where a true value does not fit, the format
writes a sentinel into that field and the real one into a zip64 end of central
directory record, found through a locator sitting immediately before the classic
record. fetchloom reads that record whenever either field states its sentinel
and a locator is there, so the count and the location every later decision is
made from are the archive's own rather than a truncation of them. an archive
holding exactly 65,535 members states the same sixteen bits and carries no
locator, which is not zip64 and is read as it stands. a locator pointing at
bytes that are not a zip64 end of central directory record, or at one shorter
than the format's smallest, is refused naming zip64. a member stating its own
sizes in zip64 form is still refused, because reading those is a different thing
from finding the directory.

collisions are found by creating each entry exclusively in staging, so the
target filesystem's own folding decides rather than a table fetchloom would have
to keep correct.

staging for a materialization lives on the destination volume, so publication is
a rename rather than a copy. replacing an existing destination renames the old
tree aside first, which leaves the destination briefly absent but never partial,
and leaves the previous tree recoverable until the new one is published.

copy on write clone is attempted first and falls back to a byte copy. the
fallback emits a `degrade` naming the volume that refused, and that volume is
not asked again in the same run. a clone that succeeded degrades nothing,
because nothing was lowered.

## reconcile

running a request against an existing destination produces one outcome per
entry, decided against the tree the run resolved. a receipt is a cached copy of
that answer, never a second authority for it.

| outcome | condition | action |
|---|---|---|
| `unchanged` | fingerprint or hash matches the resolved entry | nothing |
| `restored` | entry missing | materialize from cache |
| `modified` | entry differs from the resolved entry | stop, name every path, require `--force` or `--adopt` |
| `foreign` | present, not in the resolved tree | stop and name it, require `--force` or `--adopt` |

an entry the destination holds as another kind of thing than the one that was
resolved is `modified`, because what an entry is belongs to the entry.

which answer `unchanged` is decided by comes from `--verify`, so one policy
governs a destination entry and a cache hit rather than two. a destination with
no receipt has no recorded fingerprint, so every file is hashed. a fingerprint
answers only what the record it was written with states about that path, so it
stands for that record's entry and never for the tree this run resolved. that is
what keeps `--adopt` from letting the next run report a tree the destination
does not hold.

every entry unchanged writes nothing: no staging, no rename, status `unchanged`,
exit 0. entries missing and nothing modified or foreign builds only the missing
entries and publishes them one at a time. anything modified or foreign stops the
run before staging and exits 60.

numbered directories are never created. a destination is never partially
reconciled: it is never left holding a tree that is neither the one it held nor
the one that was resolved.

## the record

a run into a destination writes one record beside the cache, keyed by that
destination. it states two trees and the filesystem facts that make comparing
them cheap.

| field | holds |
|---|---|
| `entries` | the canonical entry stream of what was materialized, which is what `tree` digests |
| `resolved` | the canonical entry stream of what the reference resolved to, absent when it is the same |
| `fingerprints` | volume, file, size, modification and change time, per file, as they stood after publication |
| `artifacts` | per artifact, the digest, where it came from, its trust class, and the name, length, selection, layout and archive format that decide where its members land |

the two trees differ only where a run kept your version of an entry over
upstream's. `entries` is what the destination holds, so it is what a fingerprint
answers about and what `verify <path>` folds. `resolved` is what upstream last
gave, so it is the merge base and what `status`, `diff` and `revert` compare
against.

a record naming no entry is not a record. every command that works against one
refuses with `reference.unresolved` naming the destination rather than comparing
against nothing.

## status

`status <path>` and `diff <path>` decide one state per entry, against `resolved`
in the record. both are read only: they change no entry in the destination and
issue no request.

| state | condition |
|---|---|
| `unchanged` | the destination holds what the record states |
| `modified` | it holds something else |
| `deleted` | the record states it and the destination does not hold it |
| `added` | the destination holds it and the record does not state it |

there is no fifth state and no summary beyond these. an entry that is unchanged
is not printed, so a destination that is exactly what the run left prints
nothing at all. `diff` prints the same states and adds what the record states
and what the destination holds, as a digest and a length. neither ever compares
the inside of a file.

a file is answered by its recorded fingerprint when `--verify` is not `always`
and that fingerprint still matches. otherwise its bytes are read. that is a
decision about cost and never about correctness: a fingerprint that matches
stands for the digest the record holds for that path, and any difference in
volume, file identifier, length, modification time or change time reads the
bytes again.

a fingerprint is recorded only for a file whose modification and change times
are already behind the instant the run began recording them, so a file written
while the record was being taken carries no fingerprint and is always read. one
window remains and is not closed by anything short of hashing: a file rewritten
to the same length within the same filesystem timestamp tick as the run's own
write, before the record was taken. `--verify always` is the answer for a caller
who cannot accept that window.

## three way

`get` against a destination that has a record, when what the reference resolves
to now differs from the `resolved` tree that record holds, is a three way
compare. the record is the merge base, the destination is your side, and what
the reference resolves to is upstream. the comparison is per entry and never per
line: a parquet file and a JPEG have no lines, so an entry is the smallest thing
that can differ.

| base | you | upstream | outcome |
|---|---|---|---|
| present | unchanged | changed | take upstream |
| present | changed | unchanged | keep yours |
| present | changed | changed, differently | conflict |
| present | changed | changed, identically | unchanged, and nothing is written |
| present | deleted | unchanged | stays deleted |
| present | deleted | changed | conflict |
| present | deleted | deleted | stays deleted |
| present | unchanged | deleted | take upstream, which removes it |
| present | changed | deleted | conflict |
| absent | added | absent | keep yours |
| absent | absent | added | take upstream |
| absent | added | added, differently | conflict |
| absent | added | added, identically | unchanged |

an entry is one value, so a change of what it is is a change like any other: a
path that is a file on one side and a directory on the other has moved on that
side, and a mode upstream changed is upstream changing that entry.

a conflict writes upstream's version beside yours as `<name>.upstream`, leaves
yours exactly as it is, names every conflicting path, and exits 60 with
`destination.conflict`. nothing merges the contents of a file, nothing prompts,
and nothing chooses for you. a run whose `<name>.upstream` would land on a name
either side already holds fails with `destination.conflict` before anything is
written, because there is no second name and a numbered one would be a guess.

a conflicted run still writes its record, so the next run compares against what
this one left rather than conflicting again on the same entry.

an entry missing from the destination is taken to be a deletion you made,
because the record cannot tell a deliberate deletion from a file that vanished
and deletion is a change like any other. the other reading is reachable:
`--force` rebuilds the destination as upstream states it, and `revert <path>
<entry>` puts one entry back. in a two way run, where upstream has not moved, a
missing entry is `restored` as it always was, because a run that has nothing new
to give has nothing to do but put back what it wrote.

every one of these publishes the way the rest of the product does. the merged
tree is built whole in staging beside the destination and published by rename,
so a run killed halfway leaves the old tree or the new one and never a half
merged one.

## revert

`revert <path>` restores the entries the record names, and `revert <path>
<entry>...` restores only those. an entry you modified is rewritten, one you
deleted comes back, and one you added is removed. every other entry is left
exactly as it is.

the bytes come from the cache and never from the network, so a revert offline is
a revert. an entry the cache no longer holds fails with `cache.corrupt` naming
the object and the command that would bring it back. it never quietly refetches.
a symbolic link the record names by its target's digest rather than by the text
of the target cannot be restored from the record alone, and fails naming it.

revert publishes the way a three way run does: whole, by rename, old tree or new
tree.

revert writes no record. what it restored is what the record already stated, and
what it left alone is still yours.

## promote

`promote <path>` makes the destination as it stands a dataset of its own. it
reads every file, keeps the bytes in the cache, and writes a manifest naming one
artifact per file with both digests, plus a lock pinning them.

ingestion happens at promote and at no other time. a run never copies your edits
into the cache as you make them, because the bytes are already on disk and the
cost belongs at the moment someone asks for it.

the manifest states `derived_from`: the dataset, the manifest digest and the
tree digest the record names. a promoted dataset that forgets what it was
derived from is worth less than one that remembers.

promote pins bytes. it does not publish them. the manifest names each artifact
by a path relative to the tree, and an artifact whose stated digest the cache
already holds resolves from the cache without that path existing at all. `cache
export` is how the objects reach another machine.

promote refuses a destination with no record, because a directory nothing wrote
is what `init` describes.

## partial success

artifacts are independent. objects that verified stay in the cache and are
reused next run.

the destination is all or nothing. if any selected artifact fails, nothing is
published and the previous destination is untouched. the result reports each
artifact separately with its own status and error.

a run where one artifact of several failed records every artifact that verified
in the lock and no `tree`, because a lock without a tree still pins bytes. it
writes no receipt at all, because a receipt records what a run materialized and
nothing was.

## documents and their bounds

a document is bounded by what wrote it, not by which parser reads it.

a manifest, listing, lock, plan and metadata document are written by strangers,
so they are bounded to refuse hostile input by the manifest size and node
limits. a receipt is written by this machine about work it already did, and its
length is a function of how many files the run materialized rather than of
anything a stranger controls, so it is bounded by the record limits, set to hold
a tree of the largest size the archive entry limit permits.

a document past either bound is refused with `resource.limit` and is never read
in part, wherever it came from. a document nested deeper than the nesting depth
allows is refused as `manifest.invalid` naming the depth, because a document
shaped to exhaust a reader is malformed rather than large. bounding a receipt by
the manifest limits would let this build materialize a tree it cannot verify,
which is the one thing a receipt exists to prevent.

## credentials

a credential has one of two shapes. a bearer credential is one opaque value that
is sent. a signing credential is an access key, secret key, region and optional
session token, and its secret is never sent: it derives a key that signs a
canonical request. which shape a host needs is decided by the adapter serving
it, never by the user.

lookup order per host: the matching environment variable, then the provider's
own environment variable where the provider defines one, then the platform
credential store, then the provider's own helper. first match wins and the
source is reported without the secret. there is one credential path and a
provider's own variable is a second place it is read from, never a second path:
a user who already set the variable the provider's own tools read should not
have to restate it. the host-named variable holds the whole `Authorization`
header, so a header other than `Bearer` can be sent. a provider's own variable
holds the bare token that provider prints, and is sent as `Bearer` followed by
it, because the provider defines the contents of its own variable. a signing
credential found without a region fails `policy.credential_invalid` naming the
region, because a guessed region produces a refusal the user cannot act on.

a credential is bound to the host it was resolved for. it is dropped on any
redirect to a different host, and the drop is reported.

never written anywhere: bearer tokens, API keys, passwords, `Authorization` and
`Cookie` headers, the userinfo component of a URL, and the value of every query
parameter. query values are redacted as a class rather than by a list of known
sensitive names, because a list is a thing that can be incomplete. redaction
applies identically to logs, events, receipts, plans and error messages,
replaced with the fixed text `[redacted]`.

fetchloom never asks at startup and never as setup. a credential is requested
only at the moment it changes the outcome, and the request states that outcome.
required means no reachable source can serve the request without it, so the run
stops with `policy.credential_missing` and prints the provider's setup steps.
optional means a source needing one scored better than every reachable
alternative, so both options are named with the measured difference and the run
proceeds with the alternative if the user declines. an optional credential is
offered only when the difference exceeds the offer threshold.

non interactive runs never block. a required credential fails immediately with
the steps on stderr. an optional one is reported as an unused opportunity and
the alternative is used.

every provider ships one fixed record and fetchloom never improvises the
wording: the name the user recognizes, what it unlocks stated concretely,
whether it is required, numbered steps naming the page to open and the button to
press, where to put the value, the command that confirms it works, and the
narrowest permissions that suffice. steps assume no prior knowledge of the
provider, of tokens, or of the terminal.

terms are never accepted automatically. if a manifest records
`requires_acceptance`, fetchloom refuses to transfer until the user asserts
acceptance, records that the assertion was made, and makes no legal
determination about what it means.

a credential resolved for a host is never sent over a connection that is not
secured. `https` carries one and `http` does not, and no flag moves that. the
one exception is a host whose every address is on this machine, where there is
no wire between the two ends for anyone to read. everything else is refused with
`policy.credential_invalid` naming the host and the scheme, before a request is
issued, which is what ftp already did for a control connection it could not
secure.

a redirect that leaves `https` for `http` is refused with `network.tls` naming
both origins. a run that started secured stays secured or stops, because a
downgrade is a thing a network position arranges rather than a thing a publisher
means.

## addresses

every address a run would connect to after following an http redirect is checked,
and one inside this machine or inside the network this machine sits in is refused
with `policy.address_refused` naming the host, the address, and the class.
loopback, the unspecified address, the private ranges, the carrier-grade range,
the link-local range which holds `169.254.169.254`, the protocol assignment,
documentation, benchmarking, multicast and reserved ranges, and their ipv6
equivalents including a v4 address embedded in a v6 one.

there is no flag. a flag that re-enables fetching the cloud metadata endpoint
has one user and it is not the person running the command.

ftp has no redirect to follow and its data connection is opened to the address
the control connection is already talking to, so there is no second address for
a server to choose and nothing for this check to guard there.

the location a person names on the command line is theirs to name, so it is not
checked: they can already open any address on their own machine without this
tool. what a stranger's document or a stranger's redirect names is checked,
which is where the reach an attacker gains actually is. a redirect that stays on
this machine from a start that was already on this machine crosses no boundary
and is allowed.

the check reads the addresses the name resolves to at the moment it is made, and
the transport resolves the name again when it connects. a name that answers
publicly to the check and privately to the transport is not closed by this, and
closing it needs the resolver the transport itself uses.

## listing

a reference naming a container is expanded by listing it. fetchloom lists. it
does not crawl.

supported: object store listing APIs, provider repository and record APIs,
WebDAV `PROPFIND`, standard generated HTML indexes, and FTP directories.

an FTP directory is listed with `MLSD`, which states each member's type and size
as fields rather than as a line meant for a person. a server that refuses `MLSD`
is listed with `LIST` and a `degrade` names the fallback, because reading a name
and a size out of an `ls` line is guesswork where a machine-readable listing is
not. a listing is walked into the directories it names, and a member name that
is absolute, holds a separator, or is `.` or `..` is refused with
`reference.unresolved`, because a listing is a stranger's document.

only entries at or below the given prefix are considered. links pointing outside
the prefix are ignored and counted in the result. nothing is discovered from the
contents of files. no script is executed. entry count is bounded. an index that
is not recognized fails with `reference.unresolved` and is never guessed at.

a provider repository or record API is described once by an endpoint and a field
mapping, and every provider reached that way is one description against that
shape rather than one adapter of its own. a description states where the files
are in the answer, which field names each one, which field locates it or how a
location is built when the provider states none, which field sizes it, and which
field states a digest.

a member path a listing states is refused with `reference.unresolved` when it is
absolute, escapes the record with `..`, or is not one path under the record. a
listing is a stranger's document and is bounded and checked as one.

a provider whose reference names the host it reaches carries that host as the
first segment of the reference, over HTTPS and with no way to write anything
else, so one description reaches every installation. one description therefore
serves data.gov.uk and every other CKAN, and Harvard and every other Dataverse.

a DOI names a registration, and a registration names a landing page rather than
files, so a DOI is routed and never fetched. the registration is read once from
DataCite, and the landing page it states decides which provider holds the
record. the reference the router answers with is one an adapter of this build
already serves, and it is reported as a resolution alias so the run says what it
decided. a DOI resolving to a provider no adapter serves fails with
`reference.unresolved`, naming the registrant, where the DOI resolves, and that
serving it needs a description of that provider's own listing API. it is never
guessed at.

routing reads the landing page rather than the registrant identity, because a
registrant enumerates who paid for the prefix, one per installation, where the
landing page names the installation that holds the record, which is the thing a
multiplier's reference has to carry.

a file inside a record is named by the record's reference and the path the
listing gave it. the listing remembers where each of its own members is fetched
from, so naming one costs no further request. a reference that names a file
without the listing having been read is resolved by reading the record it names.

## FTP

FTP is spoken here rather than taken as a dependency. `USER` and `PASS` with
anonymous as the default, `TYPE I`, `PASV`, `SIZE`, `MDTM`, `REST` then `RETR`,
`MLSD` with a `LIST` fallback, and `QUIT`. nothing else is sent.

the data connection is passive and is never active, so no run listens for an
inbound connection. a `PASV` reply names a host and a port. the port is used and
the host is not: the data connection is opened to the address the control
connection is already talking to, and a reply naming any other host is refused
with `network.refused` naming both addresses. a server that could redirect a
client's data connection to a third party is the bounce attack, and it is
refused by name rather than by luck.

`ftps://` secures the control connection with explicit `AUTH TLS`, then `PBSZ 0`
and `PROT P` so the data connection is secured too. there is no flag that turns
that off, and no implicit FTPS on port 990, which is deprecated. a server that
refuses `AUTH TLS` fails an `ftps://` reference with `network.tls`, and no user
name or password is sent to it.

`ftp://` attempts `AUTH TLS` first and falls back to the clear when it is
refused, emitting a `degrade` that says every command and every byte travels
unencrypted. a credential resolved for the host is never sent over a control
connection that could not be secured: the run fails with
`policy.credential_invalid` naming `ftps://` as the way to send it, because a
password in the clear is worse than a run that did not happen.

a bearer credential for an FTP host is read as `user:password`, and a value with
no separator is the user name with an empty password. `SIZE` and `MDTM` are the
probe. `MDTM` is a weak validator, so an FTP resume stands on rung four: `REST`
states the offset and the whole object is verified at completion.

an FTP source states no checksum, so a first fetch is `tofu` and a locked run
compares the digest the lock pins.

an FTP control connection is refused under `--offline` at the latch, before the
host is looked up.

SFTP is not spoken, and is refused as a scheme this build does not serve rather
than being attempted over FTP.

## output

| stream | content |
|---|---|
| stdout | final result only. JSON under `--json`, otherwise nothing for machine consumption |
| stderr | progress, logs, prompts, diagnostics |

progress is written only when stderr is a terminal. prompts appear only when
stdin and stderr are both terminals. otherwise a required prompt is a policy
failure.

an operation with nothing to do exits 0 with status `unchanged`.

the result's `status` is one of exactly four values and no other is ever
written: `materialized`, `unchanged`, `restored`, `adopted`.

the JSON result carries a `work` object holding `bytes_read`, `bytes_written`,
`requests` and `file_operations`. bytes read and written count content only, so
on a run into an empty destination and empty cache that stored every object raw,
bytes written is exactly what the run left on disk. a run that compressed an
object wrote it twice, once to the partial as it arrived and once compressed as
it was published, and both are counted, because compressing an object is moving
content rather than bookkeeping about it. a packed object that arrived over a
network is written twice for the same reason, once to the partial and once into
the pack. one taken from a local source has no partial, so it is written once.
what a pack states about itself, its preamble and the header before each entry,
is bookkeeping about content rather than content, and is not counted. an object
stored raw and unpacked is counted once and is exactly what it left. neither
counts the cache's own records, its fingerprint, its locks, or the receipt:
those are bookkeeping about a run rather than the content it moved. requests
counts every request including retries and probes. file operations counts every
file or directory created, every rename, and every flush, bookkeeping included.
all four are identical on identical inputs, which is what a benchmark gates on.
none is a duration.

### the JSON a command prints

other tools parse this, so the field names are a contract rather than a
convenience. they are additive only: a field may be added, and one that is
present is never renamed, retyped or removed. there is no version field anywhere
in this build, so a rename is a break with nothing to negotiate it, and the
field names of every result are asserted by a test rather than by care.

| command | object |
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

`work` holds `bytes_read`, `bytes_written`, `requests` and `file_operations`. a
field whose value is unknown is absent rather than null, and a field whose value
is empty is written empty.

a run that failed prints the error object instead, on stdout, and the process
exit code is what the kind says it is.

terminal output is dense, aligned and quiet. it is not a user interface. one
accent color, and beyond it color carries meaning only. color is never the only
way a fact is conveyed. `NO_COLOR` and `--color` are honored and all styling is
stripped when the stream is not a terminal. an explicit `--color always` wins
over `NO_COLOR`, because a flag on this invocation is a narrower instruction
than an environment the shell was started with. one progress renderer for the
whole run, aggregated, redrawn at a fixed rate, never one indicator per file. a
value the source did not supply is shown as `?` and never estimated to make a
line look complete. no emoji, no box drawing, no full screen mode.

a hint is one line about the run that just happened, naming something the user
could do differently. it qualifies only if it could have changed this run. at
most one per run, printed after the result, never during transfer, never
repeated to the same user, and suppressed rather than repeated when nothing can
record that it was said. hints go to stderr only and never appear in the JSON
result or the event stream.

the live view is a consumer of the event stream and has no other input. it
cannot display a fact the stream does not carry, cannot influence the run, and
can be removed without changing the engine. no display mode changes bytes,
digests, tree digests, exit codes or the machine readable result.

## platform capabilities

detected per destination and per cache volume, reported in plans and results:
case sensitivity, normalization behavior, clone support, sparse file support,
symlink permission, hard link support, maximum path length, whether the volume
is network backed, and whether an on access scanner inspects writes.

a volume's capability answer is decided once. the first detection decides it and
every later question in the same run gets that answer, so two callers asking at
the same time are never given different ones. case folding and normalization are
measured per directory, so an answer carries the pair measured for the directory
asked about.

a capability the platform reports is queried. one it does not report is probed
inside fetchloom's own staging directory, never by writing into the user's
destination. a probe never uses a fixed name, because an already exists result
is how a probe reads the filesystem's answer and another probe's file would be
read as that answer.

the longest path a volume accepts is measured by building one, which costs more
than the run it precedes, so a cache records the answer against the volume that
gave it and the boot that measured it. a run reaching a cache whose record names
this boot and this volume reads that answer instead of measuring again. a record
from any other boot is refused and replaced. the probe stays the authority, and
nothing is assumed from the platform's name or from a machine wide setting,
because what a volume accepts is a property of the volume and of the running
executable rather than of Windows or Linux. a cache is what holds the record, so
a run with no cache measures.

an on access scanner is reported, never worked around.

| answer | carries | given when |
|---|---|---|
| present | the product's name and the cost ratio | the platform enumerated and one of them is a scanner |
| absent | nothing | the platform enumerated and none is, or it cannot enumerate and small writes cost no more than the ratio |
| unknown | the cost ratio | the platform cannot enumerate and small writes cost more |

the cost ratio is how many times longer writing many small files took than
writing the same bytes to one file. it is a cost, never a detection: a volume
simply slow at small writes measures the same as one behind a scanner, so
present is never reported from the ratio alone.

normalization is measured on the target volume, never inferred from the
platform: sensitive, insensitive preserving, normalizing, or unknown when the
volume refused the probe name.

processor capabilities are detected the same way: the thread budget actually
available after affinity, container and job limits, the vector level chosen for
the content digest, and whether hardware acceleration is present and usable for
the interop digest. a target where that acceleration exists but cannot be
detected emits `degrade` rather than claiming it. a thread ceiling requested
above the detected budget is clamped and the clamp is reported.

## write path

`--io buffered` writes through the operating system page cache. `--io uncached`
asks it to release written bytes once they are durable. a platform with no way
to do that without constraining every write to sector alignment uses buffered
and emits `degrade`.

`--io auto` chooses from the volume's capability answers, never from the
platform name. it chooses `uncached` only on a volume whose backing is local and
whose scanner answer is absent, on a platform that can release written pages,
and `buffered` otherwise with no `degrade`, because `auto` requested nothing in
particular.

the write path never changes what is written. both modes produce the same bytes,
digests and tree digest.

## cancellation

first interrupt: stop new work and exit 130. second: abort immediately.

cache invariants hold either way, because nothing enters `objects/` without a
completed verification and an atomic rename, so an interrupted run leaves either
a valid object or a resumable partial.

## offline

`--offline` forbids DNS resolution, connection attempts, source probes, remote
credential lookups and update checks. any operation requiring one fails with
`policy.offline` before doing avoidable work. plans produced offline mark every
network derived field unknown.

a reference is taken to require the network unless it names something this
machine already holds, so a reference form added later is refused offline until
it is shown to be local rather than permitted until someone remembers to name
it. the refusal is decided once before a reference is resolved and enforced
again where a request would be issued, so no adapter can reach the network by
resolving to a location the first decision never saw.

## bundles

a bundle carries objects between machines that do not trust each other. it is a
tar whose every member is named by the lowercase hex of the content digest of
its own bytes. a tar has no index, so there is nothing in a bundle that could be
trusted instead of its bytes.

the tar is compressed whole under `--compress`, at the level it names, and
`none` writes it uncompressed. which one a bundle is is decided when it is read
from its own first bytes: a tar opens with a member name, which here is the
hexadecimal of a digest, so it can never open with a compression frame magic and
the two are never confused. compression changes nothing a bundle states. every
member still hashes to its own name, import still derives every digest from the
bytes it reads, a member name is still never used as a path, and a bundle that
fails anywhere still publishes nothing.

import derives every digest from the bytes it reads. a member name is a claim
and never an instruction: it is compared against what the bytes hash to and
never used as a path. every member is staged and checked before anything is
published, so a bundle that fails anywhere publishes nothing.

| input | failure |
|---|---|
| a member named like a path | `archive.unsafe_path` |
| a member named anything but a digest | `cache.corrupt` |
| a header that does not check out | `cache.corrupt` |
| bytes that do not hash to the name | `integrity.mismatch` |
| a bundle that ends early | `integrity.truncated` |
| a compressed bundle whose frames do not decompress | `cache.corrupt` |

## manifests

accepted as YAML, TOML or JSON, all parsing into one model. the digest covers
the canonical JSON form, so reformatting does not change identity.

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

`name` and at least one artifact are required. `sources` is ordered, earlier
entries preferred and later ones failover only. `digest` may be absent, which
forces a weak trust class. unknown top level keys are an error.

no key may express a command, a script, or a path to execute. manifests are
declarative. no shell, no hooks, no generators. that is what makes it safe to
point fetchloom at a manifest written by someone you have never met.

a source that names a path rather than a location resolves under the directory
holding the manifest and may not leave it. an absolute path, a rooted path, and
one that climbs out with `..` are refused with `manifest.invalid` naming the
directory it had to stay under. a document a stranger wrote does not get to name
a file on this machine, and the person who wants one names it on the command
line, which is a different path through the program.

both digests a manifest states are compared to the bytes, `blake3` to the
content digest and `sha256` to the interop digest, both taken in the pass that
reads the bytes. either difference fails `integrity.mismatch` naming the
algorithm, what was stated and what was found. a manifest stating one and not
the other is compared on the one it stated.

## disk accounting

four requirements computed separately: partial transfer bytes, cache object
bytes, extraction staging bytes, destination bytes. each attributed to its
volume, requirements on a shared volume summed and checked against that volume.
insufficient space fails with `resource.disk` before transfer begins, naming the
volume, what the run needs there and what is left.

the partial and the object it becomes are one requirement where they share a
volume, because publication is a rename rather than a copy, so the larger of the
two is what has to fit rather than their sum. a requirement no one stated a size
for is not counted, because a length nobody stated is not a length, and a volume
this build cannot ask about is not a refusal. an artifact the cache already
holds is not fetched and is not counted.

## the surface is not a placeholder

a command, flag or value exists in the binary only once it performs what is
written here. there is no state in which something is present and unable to act,
because that is a placeholder and because a caller cannot distinguish it from a
usage error.

anything in this document not yet in the binary is listed under Not built in
[reference.md](reference.md).

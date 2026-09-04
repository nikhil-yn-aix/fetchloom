# Contracts

Every input Fetchloom accepts and every output it produces. If behavior is not written here, it is not defined and must not be relied on.

Before 1.0 there is exactly one format and no compatibility code. From 1.0 onward, everything in the portable set below is read forever. The design decisions that make that possible are made now. See Compatibility.

## Compatibility

Two categories, with different obligations.

Portable artifacts travel between machines, people, and time: manifests, locks, receipts, plans, bundles, the command surface, flag names, exit codes, JSON result keys, and event names. From 1.0 these are read forever. Changes to them are additive only. A field is never removed, renamed, or given a new meaning. A removed capability leaves its field readable and reports it as unsupported.

Derived data does not travel and carries no obligation: cache objects, outboard trees, staging, per-host measurements, and resolution metadata. All of it can be regenerated from portable artifacts and sources, so a format change discards it rather than migrating it. The cache format fingerprint makes that discard explicit and loud.

Four decisions make later compatibility cheap, and all four are in force from the first commit.

Identity is content-addressed. A digest computed today is valid forever, independent of any format.

Surface syntax is separated from the canonical model. Manifests parse from YAML, TOML, or JSON into one model, and digests cover the canonical form. New surface syntax never changes identity.

Digest and tree algorithms are domain-separated and named at their point of use, so a future algorithm is a distinguishable addition rather than an ambiguous reinterpretation.

Unknown keys are rejected before 1.0 and reserved after it. The `x-` prefix is reserved now for future extension fields and is rejected today. This keeps the strict-to-extensible transition a one-way door.

## Identity

| Term | Meaning |
|---|---|
| Content digest | BLAKE3 root of the object bytes. The cache key and the resume authority. |
| Interop digest | SHA-256 of the same bytes. Recorded for matching publisher claims. Never the cache key. |
| Outboard tree | Stored BLAKE3 chunk tree for an object. Enables range verification and localized repair. |
| Tree digest | BLAKE3 over the canonical entry stream of a materialized directory. |
| Manifest digest | BLAKE3 over the canonical JSON form of a manifest, never over its source text. |

Digests are written as `blake3:<hex>` or `sha256:<hex>`. Both are computed on every transfer.

Three digests are domain-separated by derived key so a value from one can never be mistaken for another: object content, tree digest, and manifest digest.

The outboard tree groups chunks and stores a length followed by parent nodes in pre-order. Group size and threshold are in Limits. An object at or below the threshold never stores an outboard, because its content digest already authenticates it whole.

A tree is built in the pass that writes the bytes, so it never costs a second read. An object the cache already holds whose tree is absent has one built from its own bytes, because those bytes are already verified and reading them costs less than fetching them again. A tree is derived data: the content digest is the only authority, and a tree that disagrees with it is discarded and rebuilt rather than believed.

## Canonical form

Every portable artifact has one canonical form and every digest over one covers
that form rather than the text anyone typed.

A manifest is accepted as YAML, TOML, or JSON. A lock, a receipt, and a plan are
written in the YAML subset and read back from any of the three.

The YAML subset is block mappings, block sequences, flow sequences, flow
mappings, and plain, single-quoted and double-quoted scalars. Anchors, aliases,
merge keys, tags, directives, block scalars, document separators, and tabs used
as indentation are each refused by name. `true`, `false`, and `null` are the only
words read as anything but text, and a run of digits with an optional sign is the
only text read as a number, so `yes` is a word.

Canonical JSON is the form every digest covers. Every mapping is ordered by the
raw bytes of its keys. An absent value is omitted rather than written as null.
There is no insignificant whitespace and no line ending anywhere, so the bytes
are identical on every platform.

The canonical text form written to a file follows the same ordering, writes every
scalar as a double-quoted JSON string, writes a mapping key plain when it is
alphanumeric with underscores, hyphens and dots and quoted otherwise, indents by
two spaces, and ends every line with one line feed on every platform. A carriage
return before a line feed is read and never written.

A byte order mark is read and never written. Unknown keys and the `x-` prefix are
refused wherever a document is read.

## Reference grammar

| Form | Example |
|---|---|
| Bare name | `silesia` |
| Namespaced with release | `acme/imagenet@2012` |
| Local manifest | `./data.yaml` |
| Remote manifest | `https://lab.edu/eeg.yaml` |
| Direct file | `https://host/x.tar.zst` |
| Local file or directory | `file:///data/raw` |
| Object store prefix | `https://s3.amazonaws.com/bucket/prefix/` |
| Provider | `hf:datasets/org/name@rev`, `zenodo:10.5281/zenodo.1234567` |
| Metadata document | `croissant:https://host/metadata.json` |
| Content address | `blake3:<hex>` |

Resolution order is deterministic: explicit scheme, then local path if it exists, then configured source priority, which is the `sources` list under Configuration files. A bare name that matches nothing fails; it is never guessed.

A reference naming one object materializes a destination directory holding that one entry, under the object's own name. Its dataset name is that name and its tree is the one-entry tree. A reference naming a container materializes every entry the container holds. The two forms differ only in what is walked, never in what a destination is.

A local path is read as a manifest when its extension is one a manifest is
written in, and as data otherwise. Nothing else decides it, and a directory is
never searched for one.

A reference that names no manifest resolves to a synthesized one: the dataset
name and one artifact carrying that name and, when the reference names a network
location, that location. A local path is never recorded in it, so its digest is
the same on every machine holding the same data.

A reference naming nothing fails with `reference.unresolved` saying nothing is there. A reference naming something that cannot be read fails with `reference.unresolved` saying to make it readable. The two are never reported as each other.

## Selection

Selection is part of identity. Changing it changes the lock entry, not the dataset name.

| Input | Meaning |
|---|---|
| `--select <glob>` | Include matching member paths. Repeatable. |
| `--exclude <glob>` | Remove from the included set. Applied after all includes. |
| `--layout keep` | Preserve archive paths. Default. |
| `--layout flatten:<n>` | Drop the first n path components. Collision is an error. |

Globs match on the canonical `/`-separated member path. Matching is on raw bytes, case-sensitive, with no normalization.

A pattern is four rules and nothing else. `*` matches any run of bytes within one path component, including none. `**` as a whole component matches any number of components, including none. `?` matches exactly one byte within one component. Every other byte is literal, including the bracket, the brace, and the backslash.

A pattern matches member paths, not subtrees. `--select data` selects a member named `data`; `--select data/**` selects what is under it. Every ancestor directory of a selected member is included whether or not a pattern matched it.

An empty selection is an error, not a no-op. It fails with `reference.unresolved` naming how many members were considered and which patterns matched none of them.

Under `--layout flatten:<n>`, a file left with no path after dropping `n` components fails with `destination.unrepresentable` naming the member and the count. A directory left with no path names the destination itself and is dropped, because the directories the surviving members need are synthesized whether or not the container declared them. A selection that flattening leaves with no member at all fails with the same kind. One logical tree therefore flattens the same way whether or not its container wrote directory entries, which is what makes flattening deterministic across encodings.

## Manifest

Accepted as YAML, TOML, or JSON. All three parse into one model. The manifest digest covers the canonical JSON form, so reformatting does not change identity.

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
  url: https://...
  requires_acceptance: false
```

Rules. `name` and at least one artifact are required. `sources` is ordered; earlier entries are preferred and later entries are failover only. `digest` may be absent, which forces a weak trust class. Unknown top-level keys are an error. No key may express a command, a script, or a path to execute.

Both digests a manifest states are compared to the bytes. `blake3` is compared to the content digest and `sha256` to the interop digest, both taken in the pass that reads the bytes, and either difference fails `integrity.mismatch` naming the algorithm, what was stated, and what was found. A manifest stating one of them and not the other is compared on the one it stated.

YAML is parsed as a restricted subset: no anchors, no aliases, no merge keys, no implicit boolean coercion of strings. Depth and node count are bounded.

## Lock

Portable. Contains no absolute paths, no hostnames of the local machine, no usernames, no timestamps of the local run.

```yaml
datasets:
  silesia:
    manifest: blake3:...
    release: "2024-06"
    artifacts:
      corpus:
        digest: blake3:...
        interop: sha256:...
        size: 68132864
        select: ["**/*.txt"]
        layout: keep
    tree: blake3:...
```

`tree` is present only after a successful materialization. A lock without it still pins the bytes.

## Receipt

Local. May contain absolute paths. Never committed by Fetchloom, never read as an authority for identity.

```yaml
dataset: silesia
manifest: blake3:...
artifacts:
  corpus:
    digest: blake3:...
    source_used: https://host/silesia.tar.zst
    trust: verified
tree: blake3:...
executable:
  - bin/run.sh
accepted_terms: asserted
destination: D:\data\silesia
fetchloom: 0.1.0-dev
completed_at: 2026-08-29T04:11:02Z
```

`executable` lists the entry paths the run materialized with the executable mode,
ascending. Every other file entry carries the read and write mode, because a tree
digest records those two and no others.

`fingerprints` maps each file entry path to the fingerprint tuple the file carried
when the run published it. It is local, like everything else in a receipt, and it
is the same cache of the phrase probably unchanged that `--verify fingerprint`
applies to a cache hit. It is never evidence of content, never appears in a lock,
and is never compared against another machine's. Recording one does not make the
receipt an identity authority, because nothing is ever concluded from it except
whether bytes have to be read.

`accepted_terms` records that the user asserted acceptance, and is present only
when the manifest recorded `requires_acceptance`. It states that the assertion
was made and nothing about what the terms mean.

`fetchloom` is provenance. It says which build produced a result and nothing ever
branches on it.

A receipt is stored in the cache under the digest of the absolute destination it
describes, so it is found from any working directory and a destination that moved
is not found. A receipt is used only when its own `destination` is the path being
verified.

`verify <path>` recomputes the tree digest of a destination and compares it to the
receipt's. The receipt supplies the mode of each file, which a filesystem does not
state, and never supplies a digest: a record attesting to its own correctness
proves nothing. Without a receipt every file is `0644`, the run emits `degrade`
saying so, and nothing is compared. A tree that no longer matches its receipt
fails with `integrity.mismatch`. A cache that cannot be opened means no receipt
and never fails `verify`.

## Plan

A plan is a file and an execution input. It contains resolved digests, so it can be executed offline.

```yaml
dataset: silesia
release: "2024-06"
network:
  hosts: [host, mirror]
  required: true
artifacts:
  - id: corpus
    digest: blake3:...
    size: 68132864
    expanded: 211938583
    cached: false
    source: https://host/silesia.tar.zst
    cost:
      egress_charged: true
      requester_pays: false
trust: tofu
credentials: []
terms: []
disk:
  partial: {volume: D, bytes: 68132864}
  cache: {volume: C, bytes: 68132864}
  staging: {volume: C}
  destination: {volume: D}
destination: ./silesia
conflicts: []
unknown: [expanded, staging, destination]
```

An artifact also carries `select` and `layout`, because selection is part of
identity and apply must reproduce the tree the plan resolved.

`cost` carries two facts and never a figure: `egress_charged` says whether the
source charges the requester for the bytes leaving it, and `requester_pays` says
whether the source refuses to serve them until the requester accepts that
charge. Neither is a currency estimate, because a field is never estimated into a
number and a sum of money is exactly that. An artifact whose source states
neither omits `cost` and names it in `unknown`, like any other field a source
could not supply.

`plan` writes the plan to standard output, in the canonical text form or as JSON
under `--json`. Both read back as the same plan. The plan is the result of the
command, so nothing else is written there.

The digests in a plan come from the lock. A reference the lock pins nothing for
cannot be planned and fails with `policy.trust_refused`, because a plan states
resolved digests and moves no bytes to learn one.

`cached`, `disk`, `conflicts`, and `destination` describe the machine the plan was
made on. Apply reports them and acts on none of them, and `--output` names the
destination when the plan's own is not this machine's.

Volumes are reported separately. Requirements on the same volume are summed.

`unknown` lists every field the source could not supply. A field is never estimated into a number, and zero is a number: a requirement whose size is unknown omits its `bytes` and is named in `unknown` beside what it depends on. An archive whose expanded length no source stated therefore lists `expanded`, `staging` and `destination`, because the last two are that length and nothing else. `partial` and `cache` are the size the lock pins and are always stated.

`fetchloom apply` re-resolves the recorded digests. If a source now serves different content, it fails with an integrity error and does not fall back.

## Trust classes

Mechanical definitions. No other meaning is implied.

| Class | Condition |
|---|---|
| `verified` | A digest computed from the bytes matched a digest of the same algorithm supplied by the manifest or the lock before this run. Either algorithm satisfies it, because a publisher's SHA-256 checked against the SHA-256 of the bytes is the same evidence as a publisher's BLAKE3 checked against theirs. |
| `corroborated` | No prior digest, but the observed digest matches at least two independent recorded witnesses. |
| `tofu` | No prior digest and no witnesses. The observed digest is recorded for future runs. |
| `unverified` | Content could not be digested, or the user disabled verification. |

Accepting `tofu` is allowed by default for a first fetch and never for a locked run. Accepting `unverified` requires an explicit flag on every invocation.

Publisher identity, manifest authenticity, and content integrity are recorded as three separate facts.

### Witnesses

A witness is one recorded observation that an artifact hashed to a digest. It holds the digest observed, the machine that observed it, the origin that served the bytes, the run that recorded it, and when. It is stored in `meta/witness/` under the artifact key, which is the derived-key hash of the manifest digest and the artifact identifier, each length-prefixed. The key never involves the content, because a witness that identified its subject by the content would only ever agree with itself.

A witness is written only by a run that transferred the bytes in full and verified them as they arrived. A cache hit writes none, because it observed nothing. Nothing read from a source, a bundle, a lock, a receipt, or a plan ever becomes a witness, so no remote party can manufacture one.

Two witnesses are independent only when they differ in all three of the machine that observed them, the origin that served the bytes, and the run that recorded them. Two observations from one machine are one observation. So are two from one origin, and two from one run.

`corroborated` therefore requires the observed digest to be carried by at least two witnesses independent of each other under that rule, which means at least two machines, at least two origins, and at least two runs. A cache directory shared between machines is how a second machine's witness reaches this one; it is the only such channel, and it is exactly as trusted as the cache directory itself, which any writer could change directly.

A run that finds fewer than two independent witnesses is `tofu` and records its own observation. Trust is never raised by counting a witness twice under a different name.

## Locked runs

`--locked` holds a run to what the lock states. What is compared and what a
difference means:

| Field | Compared | Difference |
|---|---|---|
| dataset present in the lock | before the run | `policy.trust_refused`, exit 40 |
| `manifest`, `release` | before the run | `alias.unstable`, exit 10 |
| `select`, `layout` | before the run | `alias.unstable`, exit 10 |
| artifact present in both | after resolution | `alias.unstable`, exit 10 |
| `digest`, `interop`, `size` | during the transfer | `integrity.mismatch`, exit 30 |
| `tree` | after materialization | `integrity.mismatch`, exit 30 |

Everything compared before the run is compared before a byte moves, so a run the
lock does not describe publishes nothing.

A locked run never accepts `tofu`. A dataset the lock does not pin would be a
first use, which is why it is refused on policy rather than resolved.

A locked run hands the transfer the digest the lock pins. A cache already holding
those bytes therefore issues no request at all, and a source serving other bytes
fails on integrity without falling back.

A locked run writes no lock, because a run that may not differ from the lock has
nothing to add to it.

An unlocked run records what it resolved. A run that resolved no object writes no
lock entry and emits `degrade` naming which of the two reasons it was: the
reference resolved to a tree, which is what a directory and a container image do,
or no interop digest is recorded for the object it resolved to. A run that failed
writes no lock entry and emits no `degrade`, because it gave nothing up that the
failure it already reports does not say.

## Bundles

A bundle carries objects between machines that do not trust each other.

It is an uncompressed tar whose every member is named by the lowercase
hexadecimal of the content digest of its own bytes. A tar has no index, so there
is nothing in a bundle that could be trusted instead of its bytes.

`cache export <bundle>` writes every object the cache holds. `cache import
<bundle>` reads one.

Import derives every digest from the bytes it reads. A member name is a claim and
never an instruction: it is compared against what the bytes hash to and is never
used as a path. Every member is staged and checked before anything is published,
so a bundle that fails anywhere publishes nothing.

| Input | Failure |
|---|---|
| A member named like a path | `archive.unsafe_path` |
| A member named anything but a digest | `cache.corrupt` |
| A header that does not check out | `cache.corrupt` |
| Bytes that do not hash to the name | `integrity.mismatch` |
| A bundle that ends early | `integrity.truncated` |

## Resume ladder

The rung used is always reported.

| Rung | Condition | Behavior |
|---|---|---|
| 1 | Outboard tree known for the expected digest | Verify the bytes on disk by range, resume from the first bad or missing chunk |
| 2 | Immutable content address or provider version identity | Resume, full verify at completion |
| 3 | Strong ETag unchanged | Resume, full verify at completion |
| 4 | Weak validator only | Resume, full verify at completion, quarantine on mismatch |
| 5 | No validator | Restart from zero and report why |

Resume never appends to a partial file whose recorded source identity differs from the current response.

A recorded identity that differs from the current response is answered by the rung the partial stood on. On rungs three, four and five the partial is discarded, the transfer restarts, and a `degrade` names the rung it stood on and the rung it fell to. On rung two the source declared an immutable content address or version identity and has since served a different one for the same location, which contradicts the promise that rung stands on, so the transfer fails with `source.identity_changed`, is not retried, and exits 20. A source that has merely stopped stating an immutable identity has broken no promise and restarts like any other rung.

A partial file carries a record beside it holding the redacted location, the host, the length the source stated, what the source said identifies the bytes, the entity tag and last modified value as received, whether the source accepted ranges, how many bytes are known to have arrived, and the rung the transfer was on. The record is written when the partial is opened and removed with it.

A partial is preallocated to its full length, so its size on disk says nothing about how much of it arrived. The recorded byte count is the only offset a resume may append at, and anything past it is discarded before appending. A count that is behind what actually arrived costs a refetch; one that is ahead would be a corruption, so it is only ever written after the bytes are.

`If-Range` is sent only on rung three and carries only a strong entity tag. Rung four sends the range with no precondition and verifies in full at completion.

A range request answered `200` rather than `206` means the server ignored the range. The partial is discarded, the transfer restarts, and the rung it fell to is reported.

A `416` is answered once: the length is reread from the response and the request is remade against it. A second `416` is terminal.

## Verification policy

| Setting | Behavior on cache hit |
|---|---|
| `--verify always` | Reread every byte and check it. An object with an outboard tree is walked group by group against that tree, which reads the tree as well as the object and names the damaged ranges instead of only naming the object. One without a tree is rehashed whole. |
| `--verify fingerprint` | Trust the object if the recorded filesystem fingerprint matches. Default. |
| `--verify never` | Trust the object unconditionally. Result trust class becomes `unverified`. |

`always` reads the same object bytes either way. What the tree buys is not fewer bytes, it is a failure that names byte ranges, which is the difference between an object that must be fetched again and one that can be repaired.

`never` promises nothing about a cache hit and is not a claim that the bytes are wrong or right. It does not reach verification and fail to reject; it does not verify at all, which is why the result is `unverified` rather than any other class. It never disables verification during transfer, so bytes that entered the cache in this run were still checked as they arrived.

Fingerprint is the tuple of volume identifier, file identifier, size, modification time, and change time. A fingerprint is a cache of the phrase probably unchanged. It is never evidence of content and never appears in a lock.

Verification during transfer is always on and is not configurable.

### Revalidating a completed remote object

A reference no digest pins names bytes that may change, so a warm run has to ask whether what it holds is still what the reference names. It asks with one conditional request built from the validator recorded when the object was published: `If-None-Match` from the entity tag, `If-Modified-Since` from the last modified value, and both when both were recorded.

| Answer | Behavior |
|---|---|
| `304` | Reuse what the cache holds. One request, no body bytes read. |
| `200` | The bytes changed. The response is the transfer, from zero. |
| anything else | The retry classification that already governs a request |

A `304` leaves the trust class unchanged, because it is the source restating the validator it already gave and states nothing new about the bytes.

A run with no recorded validator for the reference cannot ask, and transfers. A run whose digest is pinned by a lock asks nothing at all, because the cache holding those bytes is already the answer.

## Retry

A transient failure is retried with exponential backoff and full jitter, up to the retry attempts limit and never past the retry ceiling.

`Retry-After` is a floor on the wait, never a replacement for it. The wait before the next attempt is the longer of the run's own computed backoff and the wait the source asked for, because politeness is never lowered by what a measurement or a source says. A `Retry-After` longer than the retry ceiling is not waited out: the source is left for the next candidate, and a run with none left fails with `network.status` reporting the wait that was asked for.

## Splitting one object

One object is fetched as several ranges at once only when four conditions hold together: the source states an immutable content address or version identity, it serves ranges, the object is longer than the split threshold, and this run has recorded a per-host concurrency above one for its host. That recorded count is the measurement, because the adaptive controller raises it only when the host answered more requests in flight cleanly, which is what makes a second stream to that host worth opening. The width is that count, bounded by what the politeness ceiling permits at the moment.

The spans cover the missing bytes once, in order, with no gap and no overlap, and are written in the order they cover, so the digests taken as the bytes arrive are the digests of the object. A source that serves one span under a different identity than another fails with `source.identity_changed`.

A split that was wanted and did not happen emits `degrade` naming the condition that failed. An object at or below the split threshold wants no split, so it reports nothing.

Splitting may change timing. It may never change bytes or digests.

## Cache

```
<cache>/
  objects/    completed immutable objects above the pack threshold, addressed
              by content digest
  packs/      objects at or below it, each preceded by its content digest, its
              interop digest and its length, so a pack states what it holds
  outboard/   chunk trees for objects above the outboard threshold
  partial/    in-progress transfers with recorded source identity
  staging/    extraction trees not yet published
  quarantine/ objects that failed verification, kept for diagnosis and repair
              <hex>.diagnosis is what was found, beside the object it describes
  receipts/   one receipt per materialized destination
  meta/       resolution metadata, per-host measurements, witnesses
              object/<hex> is one record per object, holding its interop
              digest and the fingerprint it was published with
              resolution/<hex> is one record per reference, holding the
              digest it last resolved to and the validator the source gave
              witness/<hex> is the witnesses recorded for one artifact key
              prune/ is what a prune marked before it removed anything
              recovered names the boot this cache was last swept after
  locks/      advisory single-writer locks
  pins/       pin records
  format      cache format fingerprint
```

Default locations: `%LOCALAPPDATA%\Fetchloom\Cache` on Windows, and `$XDG_CACHE_HOME/fetchloom` or `~/.cache/fetchloom` on Linux.

Invariants.

Every object the cache holds has been fully verified. There is no other way for one to appear.

An object lives in one of two placements, decided by its size alone: a file of its own under `objects/`, or a span of a pack under `packs/`. At or below the pack threshold it is packed, above it it is loose. Exactly one lookup answers where an object is, and every reader goes through it: a caller asks the cache for an object and is given its bytes, never a path it opens itself. A pack is self-describing, so it is the only authority on what it holds and no index beside it can disagree; the lookup builds its answer from the packs themselves.

A pack belongs to the process and boot that writes it, and is only ever appended to by that writer, so two writers never contend for one pack. An entry is committed by the bytes reaching the pack; an entry whose length runs past the end of the pack was cut short by a crash and is not one the cache holds. Removing a packed object rewrites its pack without it, under a lock on the pack, because a tombstone would be a second authority on what a pack holds.

Publication is write to `partial/`, flush according to the durability tier, then atomic rename into `objects/`. Renames are same-volume only; a cross-volume rename is an error, never a copy. Under `fast` no flush is issued, so an object can be lost to power failure before it is durable, but a torn or partial object still cannot appear.

One writer per digest, held by an advisory lock recording machine identity, process identity, boot identity, and start time. Liveness is decided by those values, never by file modification time. Machine identity is required because a cache directory can be shared across machines that reuse process identifiers.

Publication into the cache and into a destination is refused when source and target are on different volumes, with `cache.cross_volume` or `destination.cross_volume`. It is never silently completed as a copy.

A volume that cannot express advisory locking cannot host a shared cache. It is refused with `cache.locking_unsupported` rather than used unsafely.

A second process wanting an object being written waits and reuses the result. It never starts a second transfer of the same digest.

The lock exists to deduplicate transfer, so it is taken when there is a transfer to deduplicate and not otherwise. Bytes that are already local, whether from a `file:` source, an archive being extracted, or a member of an imported bundle, are written to a name of this process's own and published with no lock, no partial, and no owner record. A content-addressed write is idempotent and a rename already makes a torn object impossible, so coordinating two processes writing identical bytes costs more than the work it would save. This is a rule about whether bytes have to cross a network, never a rule about how many of them there are, and it never becomes a size threshold.

A partial is named by the key the run knows. A run that states a content digest names the partial by that digest, and the object it publishes must hash to it. A run that states no digest names the partial by the digest of the source identity, which is the redacted location, the host, and the identity the source published, each length-prefixed, and it publishes the object under the digest the bytes hash to. The lease, the partial, its source record and its owner record all carry one key, so there is one claim per key and never two.

Orphaned staging and partial entries from a previous boot are removed at startup.

Prune marks, waits out a grace period, then sweeps. Objects that are pinned, leased by a running process, or referenced by a lock in the working directory always survive.

The grace period is a race window, not a retention policy. It exists so an object claimed between the mark and the sweep is not removed underneath the process claiming it, and it is therefore a correctness parameter rather than a preference. It is not configurable, because a shorter one is a corruption and a longer one is a wait with no benefit. Retention, meaning a rule about how long an unused object is kept, does not exist.

`cache clear` removes every object. Because refetching can cost hours and, on metered sources, money, it reports what it will remove and confirms before acting. `cache verify` rereads and rehashes every object and reports each mismatch as `cache.corrupt`. A mismatched object is moved to `quarantine/`, because leaving it in `objects/` would break the invariant that everything there has been verified, and deleting it would discard the bytes a later repair needs to find the damaged range. Quarantined objects are never served, are reported by `cache status`, and are removed only by `prune` or `clear`. Every quarantine writes a diagnosis beside the object, and every removal of a quarantined object removes it too.

`cache pin` and `cache unpin` take a content digest. The cache is addressed by digest everywhere else, a digest needs no resolution and no network, and `cache ls` prints the digests to use.

`cache repair` rebuilds the derived data the cache can regenerate from what it already holds: an outboard tree that is missing or does not check out for an object whose bytes still verify, an object record whose fields are lost, a lock whose holder is not alive, and a partial or staging entry with no object behind it. It takes no argument, reaches no network, and resolves no reference. An object whose own bytes fail verification is not repairable from the cache, so it is left in quarantine and the report names `repair <ref>` as the command that can fetch those bytes again. It reports what it rebuilt and what it could not.

### Quarantine diagnostics

A quarantined object is kept because its bytes are the evidence of what went wrong and the input a localized repair needs. Beside it, `quarantine/<hex>.diagnosis` records what was found, so a person understands the damage without running anything again.

| Field | What it holds |
|---|---|
| `digest` | The digest the object is named by, which is what it should have hashed to |
| `found` | What the bytes actually hash to, or absent when they could not be read |
| `size` | The length of the object on disk |
| `damaged` | The byte ranges that failed against the outboard tree, ascending |
| `localized` | Why the damage could not be narrowed, when `damaged` is empty |
| `source` | The redacted location the bytes came from, when the cache recorded one |
| `validator` | What the source said identified those bytes, when it said anything |
| `quarantined_at` | When the object was moved |
| `next_action` | The command that fetches the damaged bytes again |

`damaged` is empty and `localized` states why when no outboard tree was stored, when the stored tree does not check out against the digest, or when the object could not be read at all. An empty `damaged` is never read as no damage: the object is in quarantine because it failed.

### Repair

`repair <ref>` restores a cached object the cache holds damaged bytes for. It resolves the reference to a digest exactly as `get` does, finds which byte ranges of the object it holds do not match the outboard tree, and fetches those ranges and no others.

Localization is a claim about which bytes are wrong, and a tree is derived data that could itself be wrong, so it is never the last word. A repair rewrites the named ranges, then rereads the whole object and hashes it. The object enters `objects/` only when it hashes to the digest it is named by, which is the same invariant every other publication holds. A repair whose localization was wrong therefore fails loudly rather than publishing bytes that were never checked whole.

| Condition | Behavior |
|---|---|
| No outboard tree is stored | Build one from the object's own bytes and localize against it |
| The stored tree does not check out against the digest | Discard it, refetch the object whole, rebuild the tree, and emit `degrade` |
| Adjacent damaged groups | Merged into one span, so one request serves them |
| More merged spans than the repair span limit | Fetch the object whole and emit `degrade` naming both counts |
| Damaged bytes above the whole-refetch share of the object | Fetch the object whole and emit `degrade` naming both sizes |
| The source cannot serve a range | Fetch the object whole and emit `degrade` |
| The object hashes correctly after the ranges are written | Publish it and remove its diagnosis |
| It does not | Leave it quarantined, rewrite its diagnosis, and fail with `integrity.mismatch` |

Bounding exists because a repair that issues one request per damaged group stops being cheaper than one request for the object. The two bounds are in Limits and neither is a preference: past either of them the ranged repair costs more than what it replaces.

A run that finds nothing damaged reports `unchanged` and issues no request.

If `format` does not match the running binary, every cache operation fails with instructions to run `cache clear`. There is no migration.

### Modes

The cache is an optimization and is never required. The destination is the real copy.

| Mode | Behavior |
|---|---|
| Cached | Default. Objects are retained and reused across projects |
| `--no-cache` | No object is retained. The partial transfer lives beside the destination and is discarded on success. Verification is unchanged. Resume works only within the run |
| Project cache | Opt-in through a relative `cache.dir` in project config |

`--no-cache` is where the retained objects go, never a second code path. The run
opens a store of its own beside the destination, named `.<destination>.fetchloom-scratch`,
uses it exactly as it uses the cache, and removes it when the run ends, whether
the run succeeded or not. Every behavior that reads or writes a store is
therefore unchanged: an archive is extracted, a partial resumes within the run,
a cache hit is verified under the same policy, and the tree digest is the one the
same request produces with the cache. What differs is only that nothing survives
the run.

A cache that is missing, read-only, or out of space does not stop a run. Fetchloom emits `degrade` naming the reason and continues in `--no-cache` behavior, which is the scratch store above rather than no store at all.

A format mismatch stops it. The difference is whether the user can act: no disk and no permission are conditions they often cannot fix now, while a format mismatch always has one command that fixes it. Continuing would silently refetch everything the unusable cache already held, which on a large or metered source costs far more than stopping. Every command that would touch the cache fails with `cache.format_mismatch` and exit 80, naming `cache clear` as the fix. `cache clear` is that fix and is therefore the one command the check does not apply to: it removes the directory without reading a format, and its confirmation says what it could not count.

Sharing is not a mode. A cache directory may be used by several users at once and Fetchloom never assumes otherwise, so there is one behavior rather than two and no way to select the unsafe one by mistake. Objects are readable by every user of the directory and writable only by their creator. Advisory locks must be honored across users, so a cache on a filesystem that cannot express cross-user locking is refused with `cache.locking_unsupported` rather than used. Prune removes only objects the invoking user created, and reports what it skipped and why.

## Materialization

Canonical entry stream for the tree digest. Entries sorted by raw path bytes.

Fields are determined by type. A field that does not apply to a type is absent, not empty.

| Type | Fields |
|---|---|
| File | path, type, mode, size, content |
| Directory | path, type |
| Symlink | path, type, target |

Path is the entry path with `/` separators and no normalization applied. Paths must be valid UTF-8; an entry whose path is not is rejected, because a tree that cannot be named identically on both platforms cannot be reproduced on them.

Mode is `0644` or `0755` only, taken from the source archive or manifest rather than from a destination stat, so a platform that cannot represent an executable bit still produces the same tree digest. On such a platform a mode change in the destination cannot be detected during reconcile, and that limit is reported.

A bare filesystem tree is neither an archive nor a manifest and states no mode. Every file found by walking one is `0644` on every platform, whether the walk reads a source directory or a destination, and the run reports with `degrade` that it read no mode. Reconcile therefore takes every entry's mode from the tree the run resolved and never from what it found, so a mode is never a reconcile signal on any platform rather than only on the platforms that cannot carry one. `verify` on a path holds no receipt in this build, so it reports a tree whose files are all `0644`; that tree is identical on both platforms and differs from the one `get` reports for an archive stating `0755`, and closing that gap is what a receipt does.

Content is the BLAKE3 of the file bytes. Target is the symlink target bytes.

Every directory is an entry, including one that contains only other directories.

Timestamps are excluded. Ownership, ACLs, extended attributes, and alternate data streams are excluded and their presence in an archive is an error.

Rejected during extraction. Every rejection stops the run, emits `extract.reject` naming what was rejected, and publishes nothing. A rejection is called per-entry when the error names a member and fatal when it names the archive.

| Rejected | Kind | Names |
|---|---|---|
| Absolute member path | `archive.unsafe_path` | the member |
| A `..` component anywhere in the path | `archive.unsafe_path` | the member |
| A backslash or drive letter in the path | `archive.unsafe_path` | the member |
| A zip whose member paths hold both a forward slash and a backslash | `archive.unsafe_path` | the member |
| A path that is not valid UTF-8 | `archive.unsafe_path` | the member, as bytes |
| A path holding a NUL | `archive.unsafe_path` | the member |
| A path longer than the volume's maximum | `archive.unsafe_path` | the member and both lengths |
| A path deeper than the nesting limit | `archive.unsafe_path` | the member and both depths |
| A link target resolving outside the destination | `archive.link_escape` | the member and the target |
| A hard link to a member the archive does not hold | `archive.link_escape` | the member and the target |
| A block device, character device, FIFO, or socket entry | `archive.unsupported` | the member and the type |
| A setuid or setgid bit | `archive.unsupported` | the member and the bits |
| Ownership, an access control list, an extended attribute, or an alternate data stream | `archive.unsupported` | the member and which one |
| Two members with the same path | `archive.collision` | both members |
| Two members colliding under the volume's case folding or normalization | `archive.collision` | both members |
| A local header whose path disagrees with the central directory | `archive.unsafe_path` | the member and both paths |
| A local header whose size or method disagrees with the central directory | `archive.unsupported` | the member and both values |
| A compression method inside a zip that is not store or deflate | `archive.unsupported` | the member and the method |
| A container or compression this build does not carry | `archive.unsupported` | the archive and the format |
| A header the format does not permit, or a truncated archive | `archive.unsupported` | the archive |
| More members than the entry limit | `archive.bomb` | the archive and both counts |
| More expanded bytes than the limit | `archive.bomb` | the archive and both counts |
| An expansion ratio above the limit | `archive.bomb` | the archive and both ratios |
| A name the target volume refuses | `destination.unrepresentable` | the member and what the volume said |

No member is ever skipped. An archive holding one rejected member cannot be fetched, and a selection excluding that member does not change that.

Separators. A backslash is a legal byte in a member name and is never a separator, with one exception decided from the archive itself rather than from an assumption about its writer. When no member path in a zip holds a forward slash and at least one holds a backslash, that zip states its structure with backslashes and nothing else, and every backslash in it becomes a forward slash before any rejection above is applied, so `..\..\x` is refused as `../../x` rather than accepted as a name. The run emits `degrade` naming the archive and what was done. A zip whose member paths hold both is ambiguous and is refused. A tar is never translated.

Formats. These are the only values `archive.format` takes and the only containers extraction recognizes.

| Name | What it is |
|---|---|
| `tar` | A POSIX ustar stream |
| `tar+gzip` | A tar wrapped in a gzip member |
| `tar+zstd` | A tar wrapped in a zstd frame |
| `tar+xz` | A tar wrapped in an xz stream |
| `tar+bzip2` | A tar wrapped in a bzip2 stream |
| `zip` | A zip container, store and deflate methods only |
| `gzip`, `zstd`, `xz`, `bzip2` | One compressed object, materialized as one file |

A recognized archive is extracted unless `--no-extract` is given, which materializes it as one file. A manifest states the format. A reference with no manifest takes the format from the location's final extensions, and the archive's own header must agree with it or the run fails with `archive.unsupported` naming what the name said and what the bytes said. Neither the name nor the bytes decide alone.

Collisions. Two entries that differ in raw bytes but collide under the target filesystem case-folding or Unicode normalization are an error. Both names are printed. Nothing is published. Collisions are found by creating each entry exclusively in staging, so the target filesystem's own folding decides rather than a table Fetchloom would have to keep correct.

Staging for a materialization lives on the destination volume, so publication is a rename rather than a copy. Replacing an existing destination renames the old tree aside first, which leaves the destination briefly absent but never partial, and leaves the previous tree recoverable until the new one is published.

Degenerate cases. A zero-byte file is a normal entry. An empty directory is a normal entry. Two entries with identical content are stored once in the cache and materialized twice. A single non-archive file materializes as one file inside the destination directory.

Publication. Staging is fully validated, then the destination is exposed. A partially materialized destination is never visible.

Clone or copy. Copy-on-write clone is attempted first and falls back to a byte copy. The fallback emits a `degrade` naming the volume that refused, and that volume is not asked again in the same run. A clone that succeeded degrades nothing, because nothing was lowered.

## Reconcile

Running a request against an existing destination produces one outcome per entry, decided against the tree the run resolved. A receipt is a cached copy of that answer, never a second authority for it.

| Outcome | Condition | Action |
|---|---|---|
| `unchanged` | Fingerprint or hash matches the resolved entry | Nothing |
| `restored` | Entry missing | Materialize from cache |
| `modified` | Entry differs from the resolved entry | Stop. Name every modified path. Require `--force` to overwrite or `--adopt` to accept as the new state |
| `foreign` | Entry present, not in the resolved tree | Stop and name it. Require `--force` to remove it or `--adopt` to accept it. Never deleted implicitly |

Which of the two answers `unchanged` is decided by comes from `--verify`, so one
policy governs a destination entry and a cache hit rather than two. Under
`fingerprint` a file whose recorded fingerprint still matches is `unchanged` with
its bytes unread, and one whose fingerprint has moved is hashed, exactly as a
cache hit falls through to hashing. Under `always` every file is hashed whatever
its fingerprint says. Under `never` the fingerprint alone decides. A destination
with no receipt has no recorded fingerprint, so every file is hashed. So does one
whose receipt describes a tree other than the one this run resolved: a recorded
fingerprint is a cached answer to the question the run that wrote it asked and
answers no other. That is what keeps `--adopt`, which records a tree the run did
not resolve, from letting the next run report a tree the destination does not
hold.

Every entry unchanged writes nothing: no staging directory, no rename, status `unchanged`, exit 0.

Entries missing and nothing modified or foreign builds only the missing entries in staging and publishes them into the destination one entry at a time. Anything modified or foreign stops the run before anything is staged and exits 60.

`--adopt` writes nothing and reports the tree the destination holds rather than the one that was resolved. `--force` overwrites through a full staging publication. Both are per-invocation and neither is ever implied.

Numbered directories are never created. A destination is never partially reconciled: it is never left holding a tree that is neither the one it held nor the one that was resolved.

## Partial success

Artifacts are independent. Objects that verified are kept in the cache and are reused on the next run.

The destination is all or nothing. If any selected artifact fails, nothing is published and the previous destination is left untouched.

The result reports each artifact separately with its own status and error.

A run where one artifact of several failed records every artifact that verified in
the lock and no `tree`, because a lock without a tree still pins bytes. It writes
no receipt at all, because a receipt records what a run materialized and nothing
was materialized.

## Command surface

```
get      <ref...>            resolve, transfer, verify, materialize, record
init     <url|dir>           infer and write a manifest
plan     <ref...>            resolve and report, move no bytes
apply    <plan>              execute a plan
verify   <path|ref>          recompute and compare against the receipt
repair   <ref>               refetch damaged ranges of a cached object
cache    <subcommand>        status ls verify prune clear repair, pin and unpin taking a digest,
                             import and export taking a bundle
watch    <events>            render a run's event stream, live or after the fact
completions <shell>          write a shell completion script to stdout
explain  [key]               effective settings and their origin
doctor                       environment checks, changes nothing
why      <ref>               resolution, source choice, and trust reasoning
```

`init` writes the manifest to standard output, in the canonical text form or as
JSON under `--json`. The manifest is the result of the command, so nothing else
is written there. `--output <path>` writes it to a file instead, and a path that
already exists fails with `destination.unrepresentable` unless `--force` is
given, because a manifest is edited after it is generated.

This surface describes the finished product. Before 1.0 a command, flag, or value exists in the binary only once it performs what is written here. There is no state in which something is present and unable to act, because that is a placeholder, and because a caller cannot distinguish it from a usage error. The roadmap says which phase delivers each one.

## Flags

Global flags apply to every command.

| Flag | Default | Meaning |
|---|---|---|
| `--config <path>` | discovered | Use this config file only |
| `--no-config` | off | Ignore all config files |
| `--cache-dir <path>` | platform default | Cache location |
| `--offline` | off | Forbid all network activity |
| `--json` | off | Machine-readable result on stdout |
| `--events <path\|->` | off | Newline-delimited event stream |
| `--quiet` | off | Suppress progress |
| `--verbose` | off | Raise log level one step. Repeatable |
| `--color <auto\|always\|never>` | auto | Color policy |
| `--display <plain\|live\|none>` | plain | Progress presentation |
| `--no-animation` | off | Disable redrawing |
| `--no-hints` | off | Never print hints |
| `--yes` | off | Answer every confirmation with yes, including acceptance of recorded terms |
| `--threads <n>` | detected | Ceiling on threads used for processor work |

Flags for `get` and `apply`.

| Flag | Default | Meaning |
|---|---|---|
| `--output <path>` | `./<name>` | Destination directory |
| `--select <glob>` | all | Include members. Repeatable |
| `--exclude <glob>` | none | Exclude members. Repeatable |
| `--layout <keep\|flatten:n>` | keep | Path rewriting |
| `--lock <path>` | `./fetchloom.lock` | Lock file location |
| `--locked` | off | Fail if resolution differs from the lock |
| `--no-cache` | off | Bypass the cache for this operation |
| `--verify <always\|fingerprint\|never>` | fingerprint | Cache-hit verification policy |
| `--force` | off | Overwrite modified destination entries |
| `--adopt` | off | Accept current destination contents as correct |
| `--no-extract` | off | Keep archives as files |
| `--concurrency <n>` | measured | Global in-flight transfers |
| `--per-host <n>` | measured | In-flight transfers per host |
| `--bandwidth <rate>` | unlimited | Aggregate ceiling |
| `--retries <n>` | 5 | Attempts per transient failure |
| `--timeout <duration>` | 30s | Idle timeout per connection |
| `--durability <strict\|normal\|fast>` | normal | `strict` flushes file and directory to the device before publication; `normal` flushes the file; `fast` flushes nothing and relies on the atomic rename alone |
| `--io <auto\|buffered\|uncached>` | auto | Write path |
| `--aggressive` | off | Raise politeness ceilings. Prints a warning |
| `--deterministic-io` | off | Disable adaptation. For benchmarking |

Flags for `init`.

| Flag | Default | Meaning |
|---|---|---|
| `--output <path>` | stdout | Write the manifest to this file |
| `--force` | off | Overwrite the file `--output` names |

### Log levels

A log level decides which of the events the run already emits are written to
standard error, each as one JSON object carrying fields and never a formatted
sentence. It never decides which events exist, and the stream `--events` writes
is byte-identical at every level.

| Level | Rendered |
|---|---|
| `error` | `error` and `degrade` |
| `info` | those, plus `run.start`, `run.end`, and the result. Default |
| `debug` | every event the stream carries, one line each |

`--verbose` raises the level one step and is repeatable. A step past `debug` is
clamped and the clamp is reported. `FETCHLOOM_LOG` names a level and sits at the
environment precedence level.

## Environment

| Variable | Effect |
|---|---|
| `FETCHLOOM_CACHE_DIR` | Cache location |
| `FETCHLOOM_CONFIG` | Config file path |
| `FETCHLOOM_OFFLINE` | Offline mode when set to `1` |
| `FETCHLOOM_CONCURRENCY` | Global concurrency |
| `FETCHLOOM_THREADS` | Processor thread ceiling |
| `FETCHLOOM_PER_HOST` | Per-host concurrency |
| `FETCHLOOM_BANDWIDTH` | Bandwidth ceiling |
| `FETCHLOOM_LOG` | Log level: `error`, `info`, or `debug` |
| `FETCHLOOM_TOKEN_<HOST>` | Bearer credential scoped to that host |
| `FETCHLOOM_ACCESS_KEY_<HOST>` | Access key of a signing credential scoped to that host |
| `FETCHLOOM_SECRET_KEY_<HOST>` | Secret key of the same signing credential |
| `FETCHLOOM_SESSION_TOKEN_<HOST>` | Session token of the same signing credential, when it has one |
| `FETCHLOOM_REGION_<HOST>` | Region the same signing credential signs for |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_REGION` | Read by the provider-native helper tier, never by the first tier |
| `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY` | Proxy policy |
| `XDG_CACHE_HOME` | Cache root on Linux |
| `NO_COLOR` | Disable color |

Precedence, highest first: command line, environment, project config, user config, built-in default. `explain` reports which level supplied each value.

## Configuration files

Configuration is TOML. Manifests accept three surface syntaxes because they are written by strangers; configuration accepts one because it is written by the user in front of us, and one format means one parser and one set of error messages.

Project configuration is `fetchloom.toml`, found by searching the working directory and then each parent until one is found or the filesystem root is reached. `explain` prints the path that was found, so the search is never a mystery.

User configuration is `config.toml` in the platform's own configuration location: `%APPDATA%\Fetchloom` on Windows and `$XDG_CONFIG_HOME/fetchloom` or `~/.config/fetchloom` on Linux. This is the configuration location, not the cache location, and the two are never the same directory.

`--config` names one file and disables the search. `--no-config` disables both levels.

`sources` is an ordered list of base locations a bare name and a namespaced
release resolve against.

```toml
sources = ["https://lab.edu/data/", "hf:datasets/acme/"]
```

The reference is appended to each base in order and the first that resolves wins.
A reference matching none fails `reference.unresolved` naming how many bases were
tried, and one given when no base is configured fails naming that none is.

Unknown keys are an error and `x-` is reserved, as everywhere else.

## Credentials

A credential has one of two shapes. A bearer credential is one opaque value that
is sent. A signing credential is an access key, a secret key, a region, and
optionally a session token, and its secret is never sent: it derives a key that
signs a canonical request. Which shape a host needs is decided by the adapter
serving it, never by the user.

Lookup order per host: matching `FETCHLOOM_TOKEN_<HOST>` or the matching
`FETCHLOOM_ACCESS_KEY_<HOST>` set, platform credential store, provider-native
helper. First match wins and the source is reported without the secret.

The provider-native helper is a provider's own convention for where its users
already keep a credential. For a source signing with SigV4 that is the `AWS_`
environment variables and the shared credentials file the provider defines. A
helper is asked only after the two tiers above answered nothing.

A signing credential found without a region fails `policy.credential_invalid`
naming the region as what is missing, because a signature is computed over one
and a guessed region produces a refusal the user cannot act on.

A credential is bound to the host it was resolved for. It is dropped on any redirect to a different host, and the drop is reported.

Never written anywhere: bearer tokens, API keys, passwords, `Authorization` and `Cookie` headers, the userinfo component of a URL, and the value of every query parameter. Query values are redacted as a class rather than by a list of known-sensitive names, because a list is a thing that can be incomplete.

Redaction applies identically to logs, events, receipts, plans, and error messages. Redacted values are replaced with the fixed text `[redacted]`.

### When Fetchloom asks

Never at startup and never as setup. A credential is requested only at the moment it changes the outcome, and the request states that outcome.

Required. No reachable source can serve the request without it. The run stops with `policy.credential_missing` and prints the provider's setup steps.

Optional. A source needing a credential scored better than every reachable alternative. Fetchloom names both options with the measured difference between them, prints the setup steps, and proceeds with the alternative if the user declines.

An optional credential is offered only when the difference exceeds the offer threshold. Below it there is no prompt and no message.

Declining is one keystroke, applies to the whole run, and can be made permanent for that provider through config. A declined optional credential is never re-offered inside the same run.

Non-interactive runs never block. A required credential fails immediately with the setup steps written to stderr. An optional one is reported as an unused opportunity and the alternative is used.

### Provider help records

Every provider ships one fixed record, and Fetchloom never improvises the wording.

| Field | Content |
|---|---|
| Provider | The name the user recognizes |
| Unlocks | What becomes possible or faster, stated concretely |
| Necessity | Required or optional |
| Steps | Numbered, exact, naming the page to open and the button to press |
| Placement | The environment variable or credential store entry to create |
| Verification | The command that confirms it works |
| Scope | The permissions to grant, always the narrowest that works |

Steps assume no prior knowledge of the provider, of tokens, or of the terminal. No abbreviations, no implied steps, no links standing in for instructions.

### Validity

A credential is checked before it is used for a transfer. An expired, revoked, or insufficiently scoped credential fails with `policy.credential_invalid`, naming the provider and how to renew or widen it. A raw authorization failure is never surfaced on its own.

## Source selection

Sources are ordered in the manifest. Order expresses preference, not a race. The same object is never transferred from more than one source at a time.

Probe. Candidates are probed in parallel up to the probe limit. A probe is a bounded metadata request costing kilobytes.

Score, in fixed priority: reachable, supports ranges, exposes immutable identity, recorded throughput for that host, time to first byte, egress cost, remaining politeness headroom. Ties are broken by manifest order, so selection is deterministic when measurements are equal.

Switch. A transfer moves to the next candidate when it stalls past the idle timeout, exhausts its retries, returns a terminal error, or sustains throughput far below what was measured. Verified bytes are kept and the resume rung is recomputed for the new source.

The chosen source and the reason are recorded in the receipt and in `source.selected`. A switch emits `source.failover` naming the source left, the source taken, and why.

Recorded throughput and time to first byte are read from the per-host measurements. A host with no measurement scores neither, so a candidate list behind no measurements comes out in manifest order.

A source abandoned for the next candidate records a `degrade` naming the source left, the source taken, and the failure that ended the first, as well as emitting `source.failover`.

Selection may change speed. It may never change bytes.

## Directory listing

A reference naming a container is expanded by listing that container. Fetchloom lists. It does not crawl.

Supported: object store listing APIs, provider repository and record APIs, WebDAV `PROPFIND`, and standard generated HTML directory indexes. FTP is not spoken, so an FTP directory is not a container this build lists.

Rules. Only entries at or below the given prefix are considered. Links pointing outside the prefix are ignored and counted in the result. Nothing is discovered from the contents of files. No script is executed. Entry count is bounded. An index that is not recognized fails with `reference.unresolved` and is never guessed at.

## Terms and licenses

If a manifest records `requires_acceptance`, Fetchloom refuses to transfer until the user asserts acceptance with `--yes` or an interactive confirmation. The assertion is recorded in the receipt. Fetchloom makes no legal determination.

## Disk accounting

Four requirements are computed separately: partial transfer bytes, cache object bytes, extraction staging bytes, and destination bytes. Each is attributed to its volume. Requirements on a shared volume are summed and checked against that volume. Insufficient space fails before transfer begins.

## Output streams

| Stream | Content |
|---|---|
| stdout | Final result only. JSON when `--json`, otherwise nothing for machine consumption |
| stderr | Progress, logs, prompts, diagnostics |

Progress is written only when stderr is a terminal. Prompts appear only when stdin and stderr are both terminals. Otherwise a required prompt is a policy failure.

An operation with nothing to do exits `0` with a result status of `unchanged`.

The result's `status` is one of exactly four values, and no other value is ever written.

| Status | Meaning |
|---|---|
| `materialized` | The destination did not exist and every selected entry was staged and published |
| `unchanged` | The destination held every selected entry already, and nothing was written |
| `restored` | The destination was missing entries and only those were written |
| `adopted` | `--adopt` was given, nothing was written, and the reported tree is the one the destination holds |

`restored` and `adopted` are run-level statuses. They share their names with the per-entry reconcile outcomes because they name the same fact at a different scale, and a run whose entries are all `restored` reports `restored`.

The JSON result carries a `work` object holding `bytes_read`, `bytes_written`, `requests`, and `file_operations`. Bytes read counts every byte of content the run read from a file and bytes written counts every byte of content it wrote to one, so that on a run into an empty destination and an empty cache, bytes written is exactly what the run left on disk. Neither counts the cache's own records, its format fingerprint, its locks, or the receipt: those are bookkeeping about a run rather than the content it moved, a run that writes no content still writes some of them, and the receipt's own length varies with where the run wrote, which would make a gated number depend on the path it was given. Requests counts every request the run issued to a source, retries and probes included, and file operations counts every file or directory the run created, every rename it performed, and every flush it issued, bookkeeping included. All four are identical on identical inputs, so they are what a benchmark gates on. None of them is a duration.

File operations is counted because the cost of holding many small objects is dominated by how many files each one takes rather than by how many bytes, and a count is the only form of that fact a gate can hold. It is counted where the platform performs the operation, so a new caller counts by construction.

### Presentation

Terminal output is dense, aligned, and quiet. It is not a user interface.

One accent color. Beyond it, color carries meaning only: success, warning, error, and dimmed detail. Color is never the only way a fact is conveyed.

`NO_COLOR` and `--color` are honored, and all styling is stripped when the stream is not a terminal.

One progress renderer for the whole run, aggregated, redrawn at a fixed rate. Never one indicator per file.

A value the source did not supply is shown as `?`. It is never estimated to make a line look complete.

No emoji, no box drawing, no animation beyond progress, no full-screen mode.

A run with nothing to do prints one line.

### Hints

A hint is one line about the run that just happened, naming something the user could do differently.

A hint qualifies only if it could have changed this run. Anything else is trivia and is not shown. Fetchloom does not tell the user facts about itself.

At most one hint per run, printed after the result, never during transfer.

The same hint is not repeated to the same user. When no cache is available to record that, hints are suppressed rather than repeated.

Hints go to stderr only. They never appear in the JSON result, in events, or in any machine-readable output, and they are suppressed when stderr is not a terminal, in continuous integration, and in non-interactive runs.

`--no-hints` and the equivalent config value disable them permanently.

### Display modes

| Mode | Behavior |
|---|---|
| `plain` | Default. One aggregated progress line and the final result |
| `live` | Opt-in. A redrawn view of the run's internals |
| `none` | No progress output |

`--display` selects the mode. The mode is forced to `plain` or `none` when stderr is not a terminal, when the terminal is `dumb`, or when a continuous integration environment is detected. `--no-animation` disables redrawing without disabling output.

The live view is a consumer of the event stream and has no other input. It cannot display a fact that the event stream does not carry, cannot influence the run, and can be removed without changing the engine.

A run writing its events to a file can be rendered from another terminal with `watch`, including after the run was placed in the background by the shell. Fetchloom never backgrounds itself and never runs a daemon.

No display mode changes bytes, digests, tree digests, exit codes, or the machine-readable result.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 2 | Usage error |
| 10 | Resolution failed |
| 20 | Network failure after retries |
| 30 | Integrity mismatch |
| 40 | Policy blocked: offline, missing credential, unaccepted terms, weak trust refused |
| 50 | Insufficient disk or resource limit exceeded |
| 60 | Destination conflict |
| 70 | Unsafe archive content |
| 80 | Cache or lock contention failure |
| 130 | Cancelled |

## Events

Newline-delimited JSON. Every event carries a monotonic sequence number, a timestamp, the dataset, and where applicable the artifact.

```
run.start run.end
resolve.start resolve.alias resolve.end
plan.ready
cache.hit cache.miss cache.wait
credential.required credential.offer credential.declined
listing.start listing.skipped listing.end
source.probe source.selected source.failover
transfer.start transfer.progress transfer.retry transfer.resume transfer.end
verify.start verify.range verify.mismatch verify.end
extract.start extract.reject extract.end
publish.commit
reconcile.outcome
degrade
error
```

A transfer emits `verify.start` before the bytes it wrote are checked against the digest they were expected to have, and then `verify.end` when they matched or `verify.mismatch` when they did not. `repair` emits the same three around the object it checks, and `verify.range` for each range it verified against the outboard tree.

`degrade` fires whenever any capability, optimization, or trust level is lower than requested, and names the reason. Silence is never used to signal degradation.

## Errors

Every error carries: layer, dataset, artifact, source, attempt count, whether it is retryable, and the recommended next action.

| Kind | Layer |
|---|---|
| `reference.unresolved`, `manifest.invalid`, `alias.unstable` | resolve |
| `network.timeout`, `network.refused`, `network.status`, `network.tls` | transfer |
| `source.unsupported_range`, `source.identity_changed` | transfer |
| `integrity.mismatch`, `integrity.truncated`, `integrity.range_mismatch` | verify |
| `archive.unsafe_path`, `archive.link_escape`, `archive.collision`, `archive.bomb`, `archive.unsupported` | extract |
| `destination.modified`, `destination.foreign`, `destination.unrepresentable`, `destination.cross_volume` | materialize |
| `cache.locked`, `cache.corrupt`, `cache.format_mismatch`, `cache.cross_volume`, `cache.locking_unsupported` | cache |
| `policy.offline`, `policy.credential_missing`, `policy.credential_invalid`, `policy.terms_required`, `policy.trust_refused` | policy |
| `resource.disk`, `resource.limit` | resource |

## Limits

Defaults. All configurable. None may be raised past a hard ceiling that would allow unbounded memory or disk use.

A document larger than the manifest size limit is refused with `resource.limit` and is never read in part, wherever it is read from: a served URL, a manifest on disk, or a plan file.

| Limit | Default |
|---|---|
| Archive entries | 1,000,000 |
| Expanded bytes | 1 TiB |
| Expansion ratio | 200 |
| Nesting depth | 64 |
| Resident memory | 1 GiB |
| Redirects followed | 10 |
| Manifest size | 16 MiB |
| Manifest node count | 100,000 |
| Retry attempts | 5 |
| Retry ceiling | 60 s |
| Outboard threshold | 64 MiB |
| Outboard chunk group | 1 MiB |
| Pack threshold | 1 MiB |
| Split threshold | 64 MiB |
| Repair spans | 64 |
| Repair whole-refetch share | 50 percent |
| Path length | the target platform's own maximum, queried per volume |
| Listing entries | 500,000 |
| Listing size | 16 MiB |
| Connections to one host | 4 |
| Connect timeout | 10 s |
| Response header timeout | 30 s |
| Idle timeout inside a body | 30 s |
| Probed candidates | 4 |
| Credential offer threshold | 2 minutes of projected transfer time, or a source supporting resume where the alternative does not |

## Platform capabilities

Detected per destination and per cache volume, reported in plans and results: case sensitivity, Unicode normalization behavior, clone support, sparse file support, symlink permission, hard link support, maximum path length, whether the volume is network-backed, and whether an on-access malware scanner is inspecting writes.

A volume's capability answer is decided once. The first detection for a volume decides it and every later question about that volume in the same run is given that answer, so two callers asking at the same time are never given different ones. Case folding and normalization are measured per directory, so an answer carries the pair measured for the directory asked about.

An on-access scanner is reported, never worked around. `doctor` states the measured cost and the exclusion the user may choose to configure.

The scanner answer has three values, and which of them a platform may give depends on whether it can enumerate what inspects a write.

| Answer | Carries | Given when |
|---|---|---|
| present | The product's name and the measured cost ratio | The platform enumerated what inspects writes and one of them is a scanner |
| absent | Nothing | The platform enumerated and none is a scanner, or the platform cannot enumerate and small writes cost no more than the ratio |
| unknown | The measured cost ratio | The platform cannot enumerate and small writes cost more than the ratio |

The cost ratio is how many times longer writing many small files took than writing the same bytes to one file. It is a cost, never a detection: a volume that is simply slow at small writes measures the same ratio as one behind a scanner. Present is therefore never reported from the ratio alone.

An unknown answer emits `degrade` naming the measured ratio, the answer left unknown, and that the cause cannot be determined on this platform.

Processor capabilities are detected and reported the same way: the thread budget actually available after affinity, container, and job limits, the vector instruction level chosen for the content digest, and whether hardware acceleration is present and usable for the interop digest. A target where that acceleration exists but cannot be detected at runtime emits `degrade` rather than claiming it. A thread ceiling requested above the detected budget is clamped rather than honored, and the clamp is reported.

Normalization behavior is measured on the target volume, never inferred from the platform. It has four values.

| Answer | Meaning |
|---|---|
| sensitive | Two spellings of one name are two names |
| insensitive preserving | Two spellings are one name, and the bytes written are the bytes stored |
| normalizing | Two spellings are one name, and the bytes stored are a normalized form |
| unknown | The volume refused the name the measurement uses, so nothing was learned |

An unknown answer emits `degrade` naming that the volume refused the probe name and why the volume gave for refusing it.

A capability probe never uses a fixed name. Two probes of one directory, in one process or in two, must not contend for a name, because an already-exists result is how a probe reads the filesystem's answer and another probe's file would be read as that answer.

A capability the platform reports is queried. One it does not report is probed inside Fetchloom's own staging directory, never by writing into the user's destination.

A capability that is absent is reported, never assumed. On a network filesystem, cloning is disabled and locking uses the conservative path.

## Write path

`--io buffered` writes through the operating system's page cache. `--io uncached` asks the operating system to release the written bytes from that cache once they are durable. A platform with no way to release them without constraining every write to sector alignment uses buffered writes and emits `degrade` naming `uncached` requested, `buffered` used, and that reason.

`--io auto` chooses from the volume's capability answers, never from the platform name. It chooses `uncached` only on a volume whose backing is local and whose scanner answer is absent, and on a platform that can release written pages. It chooses `buffered` otherwise and emits no `degrade`, because `auto` requested nothing in particular.

The write path never changes what is written. Both modes produce the same bytes, the same digests and the same tree digest.

## Cancellation

First interrupt: stop new work, finish flushing in-flight buffers, record resumable state, exit `130` within two seconds.

Second interrupt: abort immediately. Cache invariants still hold because nothing enters `objects/` without a completed verification and rename.

## Offline

`--offline` forbids DNS resolution, connection attempts, source probes, remote credential lookups, and update checks. Any operation requiring one of these fails with `policy.offline` before doing avoidable work. Plans produced offline mark every network-derived field as unknown.

A reference is taken to require the network unless it names something this machine already holds, so a reference form added to the grammar later is refused offline until it is shown to be local rather than permitted until someone remembers to name it. The refusal is decided once, before a reference is resolved, and enforced again at the point a request would be issued, so that no adapter can reach the network by resolving to a location the first decision never saw.

## Determinism

These must hold under any configuration.

Adaptive tuning, concurrency, protocol choice, clone versus copy, and I/O mode may change timing and never change resulting bytes, digests, or the tree digest.

The same lock produces the same tree digest on every platform, or the run fails and names the entries that cannot be represented.

No correctness decision depends on the wall clock, on file modification times, or on the internal layout of the cache.

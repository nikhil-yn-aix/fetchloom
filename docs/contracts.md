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

A manifest is accepted as YAML, TOML or JSON. A lock, receipt and plan are written in the YAML subset and read back from any of the three.

The YAML subset is block mappings, block sequences, flow sequences, flow mappings, and plain, single quoted and double quoted scalars. Anchors, aliases, merge keys, tags, directives, block scalars, document separators, and tabs as indentation are each refused by name. `true`, `false` and `null` are the only words read as anything but text, and a run of digits with an optional sign is the only text read as a number, so `yes` is a word.

Canonical JSON is what every digest covers. Mappings ordered by the raw bytes of their keys, absent values omitted rather than written null, no insignificant whitespace and no line ending anywhere, so the bytes are identical on every platform.

The canonical text form written to a file uses the same ordering, writes every scalar as a double quoted JSON string, writes a mapping key plain when it is alphanumeric with underscores, hyphens and dots and quoted otherwise, indents by two spaces, and ends every line with one line feed on every platform. A carriage return before a line feed is read and never written. A byte order mark is read and never written.

## References

Resolution order is deterministic: explicit scheme, then a local path if it exists, then each configured source in order. A bare name matching nothing fails. It is never guessed at.

A local path is read as a manifest when its extension is one a manifest is written in, and as data otherwise. Nothing else decides it, and a directory is never searched for one.

A reference naming no manifest resolves to a synthesized one: the dataset name, and one artifact carrying that name and, when the reference names a network location, that location. A local path is never recorded in it, so its digest is the same on every machine holding the same data.

A reference naming one object materializes a directory holding that one entry under the object's own name. A reference naming a container materializes every entry it holds. The two differ only in what is walked, never in what a destination is.

Nothing there fails with `reference.unresolved` saying nothing is there. Something there that cannot be read fails with `reference.unresolved` saying to make it readable. The two are never reported as each other.

## Selection

Selection is part of identity. Changing it changes the lock entry, not the dataset name.

Globs match the canonical `/` separated member path, on raw bytes, case sensitive, with no normalization. Four rules and nothing else. `*` matches any run of bytes within one component, including none. `**` as a whole component matches any number of components, including none. `?` matches exactly one byte within one component. Every other byte is literal, including the bracket, the brace and the backslash.

A pattern matches member paths, not subtrees. Every ancestor directory of a selected member is included whether or not a pattern matched it.

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

Either algorithm satisfies `verified`, because a publisher's SHA-256 checked against the SHA-256 of the bytes is the same evidence as a publisher's BLAKE3 checked against theirs.

`tofu` is allowed by default for a first fetch and never for a locked run. `unverified` requires an explicit flag on every invocation.

Publisher identity, manifest authenticity and content integrity are recorded as three separate facts. One never implies another.

### Witnesses

A witness is one recorded observation that an artifact hashed to a digest. It holds the digest observed, the machine that observed it, the origin that served the bytes, the run that recorded it, and when.

A witness is written only by a run that transferred the bytes in full and verified them as they arrived. A cache hit writes none, because it observed nothing. Nothing read from a source, bundle, lock, receipt or plan ever becomes a witness, so no remote party can manufacture one.

Two witnesses are independent only when they differ in all three of machine, origin and run. Two observations from one machine are one observation.

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

An unlocked run records what it resolved. A run that resolved no object writes no lock entry and emits `degrade` naming which of two reasons it was: the reference resolved to a tree, which is what a directory and a container image do, or no interop digest is recorded for the object. A run that failed writes no lock entry and emits no `degrade`, because it gave nothing up that the failure it already reports does not say.

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

A partial carries a record beside it holding the redacted location, host, stated length, stated identity, entity tag and last modified as received, whether ranges were accepted, how many bytes are known to have arrived, and the rung. It is written when the partial is opened and removed with it.

A partial is preallocated to its full length, so its size on disk says nothing about how much arrived. The recorded byte count is the only offset a resume may append at, and anything past it is discarded first. A count behind what arrived costs a refetch; one ahead would be corruption, so it is only written after the bytes are.

`If-Range` is sent only on rung three and carries only a strong entity tag. A range answered `200` rather than `206` means the server ignored it, so the partial is discarded and the rung it fell to is reported. A `416` is answered once by rereading the length and remaking the request; a second is terminal.

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

A transient failure is retried with exponential backoff and full jitter, up to the attempt limit and never past the retry ceiling.

`Retry-After` is a floor on the wait, never a replacement. The wait is the longer of the run's own backoff and what the source asked for, because politeness is never lowered by what a measurement says. A `Retry-After` longer than the ceiling is not waited out: the source is left for the next candidate, and a run with none left fails with `network.status` reporting the wait asked for.

Sources are ordered in the manifest, and order expresses preference, not a race. The same object is never transferred from more than one source at a time. Candidates are probed in parallel up to the probe limit, a probe being a bounded metadata request costing kilobytes.

Scored in fixed priority: reachable, supports ranges, exposes immutable identity, recorded throughput, time to first byte, egress cost, remaining politeness headroom. Ties break by manifest order, so selection is deterministic when measurements are equal. A losing candidate is never asked for bytes.

A transfer moves to the next candidate when it stalls past the idle timeout, exhausts retries, returns a terminal error, or sustains throughput far below what was measured. Verified bytes are kept and the resume rung is recomputed. A switch emits `source.failover` and a `degrade` naming the source left, the source taken, and the failure that ended the first.

Selection may change speed. It may never change bytes.

## Splitting

One object is fetched as several ranges at once only when four conditions hold together: the source states an immutable identity, it serves ranges, the object is longer than the split threshold, and this run has recorded a per host concurrency above one for that host. The width is that count, bounded by what politeness permits at the moment.

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
  meta/        resolution metadata, per host measurements, witnesses
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

Compression that was asked for and did not happen emits `degrade` naming the object, what was asked, and what was measured. Two cases: the probe found the object was not worth compressing, and the volume already compresses what is written to it.

A pack belongs to the process and boot that writes it and is only appended to by that writer, so two writers never contend for one pack. An entry is committed by its bytes reaching the pack; an entry whose length runs past the end was cut short by a crash and is not one the cache holds. Removing a packed object rewrites its pack without it, under a lock, because a tombstone would be a second authority on what a pack holds.

Publication is write to `partial/`, flush according to the durability tier, then atomic rename. Renames are same volume only; a cross volume rename is an error, never a copy. Under `fast` no flush is issued, so an object can be lost to power failure before it is durable, but a torn object still cannot appear.

One writer per digest, held by an advisory lock recording machine, process, boot and start time. Liveness is decided by those values, never by modification time. A second process wanting an object being written waits and reuses the result rather than starting a second transfer.

The lock exists to deduplicate transfer, so it is taken when there is a transfer to deduplicate and not otherwise. Bytes already local, whether from a `file:` source, an archive being extracted, or a bundle member, are written to a name of this process's own and published with no lock and no partial. A content addressed write is idempotent and a rename already makes a torn object impossible, so coordinating two processes writing identical bytes costs more than it saves. This is a rule about whether bytes cross a network, never about how many of them there are, and it never becomes a size threshold.

Orphaned staging and partial entries from a previous boot are removed at startup.

Prune marks, waits out a grace period, then sweeps. Pinned objects, objects leased by a running process, and objects referenced by a lock in the working directory always survive. The grace period is a race window, not a retention policy: it exists so an object claimed between mark and sweep is not removed under the process claiming it. It is not configurable, because shorter is a corruption and longer is a wait with no benefit. Retention, meaning a rule about how long an unused object is kept, does not exist.

A mismatched object moves to `quarantine/` rather than being deleted, because leaving it in `objects/` would break the invariant and deleting it would discard the bytes a localized repair needs. Quarantined objects are never served, are reported by `cache status`, and are removed only by prune or clear. Every quarantine writes a diagnosis beside the object holding what was found, the ranges that failed, and the command that fetches those bytes again.

A cache is refused outright on a volume reached over a network, with `cache.locking_unsupported`, because advisory locks there cannot be trusted across the machines sharing it. There is no conservative lock path. The refusal is the answer. A destination on such a volume is not refused.

A cache directory may be used by several users at once and Fetchloom never assumes otherwise, so there is one behavior rather than two and no way to select the unsafe one by mistake. Objects are readable by every user and writable only by their creator. Prune removes only objects the invoking user created and reports what it skipped.

If `format` does not match the running binary, every cache operation fails with instructions to run `cache clear`. There is no migration. `cache clear` is the one command the check does not apply to, because removing a directory does not depend on what wrote it.

A cache that is missing, read only, or out of space does not stop a run. It emits `degrade` and continues in `--no-cache` behavior, which is a scratch store beside the destination rather than no store at all. A format mismatch does stop it, because the user can always fix that with one command, and continuing would silently refetch everything the unusable cache already held.

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
| A zip compression method that is not store or deflate | `archive.unsupported` |
| A container or compression this build does not carry | `archive.unsupported` |
| A header the format does not permit, or a truncated archive | `archive.unsupported` |
| More members, expanded bytes, or ratio than allowed | `archive.bomb` |
| A name the target volume refuses | `destination.unrepresentable` |

A backslash is a legal byte in a member name and is never a separator, with one exception decided from the archive itself. When no member path in a zip holds a forward slash and at least one holds a backslash, that zip states its structure with backslashes and nothing else, so every backslash becomes a forward slash before any rejection is applied, and `..\..\x` is refused as `../../x` rather than accepted as a name. The run emits `degrade`. A zip holding both is ambiguous and refused. A tar is never translated.

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

Which answer `unchanged` is decided by comes from `--verify`, so one policy governs a destination entry and a cache hit rather than two. A destination with no receipt has no recorded fingerprint, so every file is hashed. So does one whose receipt describes a tree other than the one this run resolved, because a recorded fingerprint answers the question the run that wrote it asked and answers no other. That is what keeps `--adopt` from letting the next run report a tree the destination does not hold.

Every entry unchanged writes nothing: no staging, no rename, status `unchanged`, exit 0. Entries missing and nothing modified or foreign builds only the missing entries and publishes them one at a time. Anything modified or foreign stops the run before staging and exits 60.

Numbered directories are never created. A destination is never partially reconciled: it is never left holding a tree that is neither the one it held nor the one that was resolved.

## Partial success

Artifacts are independent. Objects that verified stay in the cache and are reused next run.

The destination is all or nothing. If any selected artifact fails, nothing is published and the previous destination is untouched. The result reports each artifact separately with its own status and error.

A run where one artifact of several failed records every artifact that verified in the lock and no `tree`, because a lock without a tree still pins bytes. It writes no receipt at all, because a receipt records what a run materialized and nothing was.

## Documents and their bounds

A document is bounded by what wrote it, not by which parser reads it.

A manifest, listing, lock, plan and metadata document are written by strangers, so they are bounded to refuse hostile input by the manifest size and node limits. A receipt is written by this machine about work it already did, and its length is a function of how many files the run materialized rather than of anything a stranger controls, so it is bounded by the record limits, set to hold a tree of the largest size the archive entry limit permits.

A document past either bound is refused with `resource.limit` and is never read in part, wherever it came from. Bounding a receipt by the manifest limits would let this build materialize a tree it cannot verify, which is the one thing a receipt exists to prevent.

## Credentials

A credential has one of two shapes. A bearer credential is one opaque value that is sent. A signing credential is an access key, secret key, region and optional session token, and its secret is never sent: it derives a key that signs a canonical request. Which shape a host needs is decided by the adapter serving it, never by the user.

Lookup order per host: the matching environment variable, then the platform credential store, then the provider's own helper. First match wins and the source is reported without the secret. A signing credential found without a region fails `policy.credential_invalid` naming the region, because a guessed region produces a refusal the user cannot act on.

A credential is bound to the host it was resolved for. It is dropped on any redirect to a different host, and the drop is reported.

Never written anywhere: bearer tokens, API keys, passwords, `Authorization` and `Cookie` headers, the userinfo component of a URL, and the value of every query parameter. Query values are redacted as a class rather than by a list of known sensitive names, because a list is a thing that can be incomplete. Redaction applies identically to logs, events, receipts, plans and error messages, replaced with the fixed text `[redacted]`.

Fetchloom never asks at startup and never as setup. A credential is requested only at the moment it changes the outcome, and the request states that outcome. Required means no reachable source can serve the request without it, so the run stops with `policy.credential_missing` and prints the provider's setup steps. Optional means a source needing one scored better than every reachable alternative, so both options are named with the measured difference and the run proceeds with the alternative if the user declines. An optional credential is offered only when the difference exceeds the offer threshold.

Non interactive runs never block. A required credential fails immediately with the steps on stderr. An optional one is reported as an unused opportunity and the alternative is used.

Every provider ships one fixed record and Fetchloom never improvises the wording: the name the user recognizes, what it unlocks stated concretely, whether it is required, numbered steps naming the page to open and the button to press, where to put the value, the command that confirms it works, and the narrowest permissions that suffice. Steps assume no prior knowledge of the provider, of tokens, or of the terminal.

Terms are never accepted automatically. If a manifest records `requires_acceptance`, Fetchloom refuses to transfer until the user asserts acceptance, records that the assertion was made, and makes no legal determination about what it means.

## Listing

A reference naming a container is expanded by listing it. Fetchloom lists. It does not crawl.

Supported: object store listing APIs, provider repository and record APIs, WebDAV `PROPFIND`, and standard generated HTML indexes. FTP is not spoken, so an FTP directory is not a container this build lists.

Only entries at or below the given prefix are considered. Links pointing outside the prefix are ignored and counted in the result. Nothing is discovered from the contents of files. No script is executed. Entry count is bounded. An index that is not recognized fails with `reference.unresolved` and is never guessed at.

## Output

| Stream | Content |
|---|---|
| stdout | Final result only. JSON under `--json`, otherwise nothing for machine consumption |
| stderr | Progress, logs, prompts, diagnostics |

Progress is written only when stderr is a terminal. Prompts appear only when stdin and stderr are both terminals. Otherwise a required prompt is a policy failure.

An operation with nothing to do exits 0 with status `unchanged`.

The result's `status` is one of exactly four values and no other is ever written: `materialized`, `unchanged`, `restored`, `adopted`.

The JSON result carries a `work` object holding `bytes_read`, `bytes_written`, `requests` and `file_operations`. Bytes read and written count content only, so on a run into an empty destination and empty cache that stored every object raw, bytes written is exactly what the run left on disk. A run that compressed an object wrote it twice, once to the partial as it arrived and once compressed as it was published, and both are counted, because compressing an object is moving content rather than bookkeeping about it. Neither counts the cache's own records, its fingerprint, its locks, or the receipt: those are bookkeeping about a run rather than the content it moved. Requests counts every request including retries and probes. File operations counts every file or directory created, every rename, and every flush, bookkeeping included. All four are identical on identical inputs, which is what a benchmark gates on. None is a duration.

Terminal output is dense, aligned and quiet. It is not a user interface. One accent color, and beyond it color carries meaning only. Color is never the only way a fact is conveyed. `NO_COLOR` and `--color` are honored and all styling is stripped when the stream is not a terminal. One progress renderer for the whole run, aggregated, redrawn at a fixed rate, never one indicator per file. A value the source did not supply is shown as `?` and never estimated to make a line look complete. No emoji, no box drawing, no full screen mode.

A hint is one line about the run that just happened, naming something the user could do differently. It qualifies only if it could have changed this run. At most one per run, printed after the result, never during transfer, never repeated to the same user, and suppressed rather than repeated when nothing can record that it was said. Hints go to stderr only and never appear in the JSON result or the event stream.

The live view is a consumer of the event stream and has no other input. It cannot display a fact the stream does not carry, cannot influence the run, and can be removed without changing the engine. No display mode changes bytes, digests, tree digests, exit codes or the machine readable result.

## Platform capabilities

Detected per destination and per cache volume, reported in plans and results: case sensitivity, normalization behavior, clone support, sparse file support, symlink permission, hard link support, maximum path length, whether the volume is network backed, and whether an on access scanner inspects writes.

A volume's capability answer is decided once. The first detection decides it and every later question in the same run gets that answer, so two callers asking at the same time are never given different ones. Case folding and normalization are measured per directory, so an answer carries the pair measured for the directory asked about.

A capability the platform reports is queried. One it does not report is probed inside Fetchloom's own staging directory, never by writing into the user's destination. A probe never uses a fixed name, because an already exists result is how a probe reads the filesystem's answer and another probe's file would be read as that answer.

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

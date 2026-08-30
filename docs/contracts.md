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

The outboard tree groups chunks and stores a length followed by parent nodes in pre-order. Group size and threshold are in Limits. An object at or below one group never stores an outboard, because its content digest already authenticates it whole.

## Reference grammar

| Form | Example |
|---|---|
| Bare name | `silesia` |
| Namespaced with release | `acme/imagenet@2012` |
| Local manifest | `./data.yaml` |
| Remote manifest | `https://lab.edu/eeg.yaml` |
| Direct file | `https://host/x.tar.zst` |
| Local file or directory | `file:///data/raw` |
| Object store | `s3://bucket/prefix/` |
| Provider | `hf:datasets/org/name@rev`, `zenodo:10.5281/zenodo.1234567` |
| Metadata document | `croissant:https://host/metadata.json` |
| Content address | `blake3:<hex>` |

Resolution order is deterministic: explicit scheme, then local path if it exists, then configured source priority. A bare name that matches nothing fails; it is never guessed.

A reference naming one object materializes a destination directory holding that one entry, under the object's own name. Its dataset name is that name and its tree is the one-entry tree. A reference naming a container materializes every entry the container holds. The two forms differ only in what is walked, never in what a destination is.

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

Under `--layout flatten:<n>`, a member left with no path after dropping `n` components fails with `destination.unrepresentable` naming the member and the count.

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
destination: D:\data\silesia
fetchloom: 0.1.0-dev
completed_at: 2026-08-29T04:11:02Z
```

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
trust: tofu
credentials: []
terms: []
disk:
  partial: {volume: D, bytes: 68132864}
  cache: {volume: C, bytes: 68132864}
  staging: {volume: C, bytes: 211938583}
  destination: {volume: D, bytes: 211938583}
destination: ./silesia
conflicts: []
unknown: [expanded]
```

Volumes are reported separately. Requirements on the same volume are summed.

`unknown` lists every field the source could not supply. A field is never estimated into a number.

`fetchloom apply` re-resolves the recorded digests. If a source now serves different content, it fails with an integrity error and does not fall back.

## Trust classes

Mechanical definitions. No other meaning is implied.

| Class | Condition |
|---|---|
| `verified` | Content digest matched a digest supplied by the manifest or the lock before this run. |
| `corroborated` | No prior digest, but the observed digest matches at least two independent recorded witnesses. |
| `tofu` | No prior digest and no witnesses. The observed digest is recorded for future runs. |
| `unverified` | Content could not be digested, or the user disabled verification. |

Accepting `tofu` is allowed by default for a first fetch and never for a locked run. Accepting `unverified` requires an explicit flag on every invocation.

Publisher identity, manifest authenticity, and content integrity are recorded as three separate facts.

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

A partial file carries a record beside it holding the redacted location, the host, the length the source stated, what the source said identifies the bytes, the entity tag and last modified value as received, whether the source accepted ranges, how many bytes are known to have arrived, and the rung the transfer was on. The record is written when the partial is opened and removed with it.

A partial is preallocated to its full length, so its size on disk says nothing about how much of it arrived. The recorded byte count is the only offset a resume may append at, and anything past it is discarded before appending. A count that is behind what actually arrived costs a refetch; one that is ahead would be a corruption, so it is only ever written after the bytes are.

`If-Range` is sent only on rung three and carries only a strong entity tag. Rung four sends the range with no precondition and verifies in full at completion.

A range request answered `200` rather than `206` means the server ignored the range. The partial is discarded, the transfer restarts, and the rung it fell to is reported.

A `416` is answered once: the length is reread from the response and the request is remade against it. A second `416` is terminal.

## Verification policy

| Setting | Behavior on cache hit |
|---|---|
| `--verify always` | Full reread and rehash |
| `--verify fingerprint` | Trust the object if the recorded filesystem fingerprint matches. Default. |
| `--verify never` | Trust the object unconditionally. Result trust class becomes `unverified`. |

Fingerprint is the tuple of volume identifier, file identifier, size, modification time, and change time. A fingerprint is a cache of the phrase probably unchanged. It is never evidence of content and never appears in a lock.

Verification during transfer is always on and is not configurable.

## Cache

```
<cache>/
  objects/    completed immutable objects, addressed by content digest
  outboard/   chunk trees for objects above the outboard threshold
  partial/    in-progress transfers with recorded source identity
  staging/    extraction trees not yet published
  quarantine/ objects that failed verification, kept for diagnosis and repair
  meta/       resolution metadata, per-host measurements, witnesses
  locks/      advisory single-writer locks
  pins/       pin records
  format      cache format fingerprint
```

Default locations: `%LOCALAPPDATA%\Fetchloom\Cache`, `~/Library/Caches/fetchloom`, `$XDG_CACHE_HOME/fetchloom` or `~/.cache/fetchloom`.

Invariants.

An entry in `objects/` has been fully verified. There is no other way for a file to appear there.

Publication is write to `partial/`, flush according to the durability tier, then atomic rename into `objects/`. Renames are same-volume only; a cross-volume rename is an error, never a copy. Under `fast` no flush is issued, so an object can be lost to power failure before it is durable, but a torn or partial object still cannot appear.

One writer per digest, held by an advisory lock recording machine identity, process identity, boot identity, and start time. Liveness is decided by those values, never by file modification time. Machine identity is required because a cache directory can be shared across machines that reuse process identifiers.

Publication into the cache and into a destination is refused when source and target are on different volumes, with `cache.cross_volume` or `destination.cross_volume`. It is never silently completed as a copy.

A volume that cannot express advisory locking cannot host a shared cache. It is refused with `cache.locking_unsupported` rather than used unsafely.

A second process wanting an object being written waits and reuses the result. It never starts a second transfer of the same digest.

A partial is named by the key the run knows. A run that states a content digest names the partial by that digest, and the object it publishes must hash to it. A run that states no digest names the partial by the digest of the source identity, which is the redacted location, the host, and the identity the source published, each length-prefixed, and it publishes the object under the digest the bytes hash to. The lease, the partial, its source record and its owner record all carry one key, so there is one claim per key and never two.

Orphaned staging and partial entries from a previous boot are removed at startup.

Prune marks, waits out a grace period, then sweeps. Objects that are pinned, leased by a running process, or referenced by a lock in the working directory always survive.

The grace period is a race window, not a retention policy. It exists so an object claimed between the mark and the sweep is not removed underneath the process claiming it, and it is therefore a correctness parameter rather than a preference. It is not configurable, because a shorter one is a corruption and a longer one is a wait with no benefit. Retention, meaning a rule about how long an unused object is kept, does not exist.

`cache clear` removes every object. Because refetching can cost hours and, on metered sources, money, it reports what it will remove and confirms before acting. `cache verify` rereads and rehashes every object and reports each mismatch as `cache.corrupt`. A mismatched object is moved to `quarantine/`, because leaving it in `objects/` would break the invariant that everything there has been verified, and deleting it would discard the bytes a later repair needs to find the damaged range. Quarantined objects are never served, are reported by `cache status`, and are removed only by `prune` or `clear`.

`cache pin` and `cache unpin` take a content digest. The cache is addressed by digest everywhere else, a digest needs no resolution and no network, and `cache ls` prints the digests to use.

If `format` does not match the running binary, every cache operation fails with instructions to run `cache clear`. There is no migration.

### Modes

The cache is an optimization and is never required. The destination is the real copy.

| Mode | Behavior |
|---|---|
| Cached | Default. Objects are retained and reused across projects |
| `--no-cache` | No object is retained. The partial transfer lives beside the destination and is discarded on success. Verification is unchanged. Resume works only within the run |
| Project cache | Opt-in through a relative `cache.dir` in project config |

A cache that is missing, read-only, or out of space does not stop a run. Fetchloom emits `degrade` naming the reason and continues in `--no-cache` behavior.

A format mismatch stops it. The difference is whether the user can act: no disk and no permission are conditions they often cannot fix now, while a format mismatch always has one command that fixes it. Continuing would silently refetch everything the unusable cache already held, which on a large or metered source costs far more than stopping. Every command that would touch the cache fails with `cache.format_mismatch` and exit 80, naming `cache clear` as the fix.

Sharing is not a mode. A cache directory may be used by several users at once and Fetchloom never assumes otherwise, so there is one behavior rather than two and no way to select the unsafe one by mistake. Objects are readable by every user of the directory and writable only by their creator. Advisory locks must be honored across users, so a cache on a filesystem that cannot express cross-user locking is refused with `cache.locking_unsupported` rather than used. Prune removes only objects the invoking user created, and reports what it skipped and why.

## Materialization

Canonical entry stream for the tree digest. Entries sorted by raw path bytes.

Fields are determined by type. A field that does not apply to a type is absent, not empty.

| Type | Fields |
|---|---|
| File | path, type, mode, size, content |
| Directory | path, type |
| Symlink | path, type, target |

Path is the entry path with `/` separators and no normalization applied. Paths must be valid UTF-8; an entry whose path is not is rejected, because a tree that cannot be named identically on all three platforms cannot be reproduced on them.

Mode is `0644` or `0755` only, taken from the source archive or manifest rather than from a destination stat, so a platform that cannot represent an executable bit still produces the same tree digest. On such a platform a mode change in the destination cannot be detected during reconcile, and that limit is reported.

Content is the BLAKE3 of the file bytes. Target is the symlink target bytes.

Every directory is an entry, including one that contains only other directories.

Timestamps are excluded. Ownership, ACLs, extended attributes, and alternate data streams are excluded and their presence in an archive is an error.

Rejected during extraction. Every rejection stops the run, emits `extract.reject` naming what was rejected, and publishes nothing. A rejection is called per-entry when the error names a member and fatal when it names the archive.

| Rejected | Kind | Names |
|---|---|---|
| Absolute member path | `archive.unsafe_path` | the member |
| A `..` component anywhere in the path | `archive.unsafe_path` | the member |
| A backslash or drive letter in the path | `archive.unsafe_path` | the member |
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

Clone or copy. Copy-on-write clone is attempted first and falls back to a byte copy. The chosen mechanism is reported per operation.

## Reconcile

Running a request against an existing destination produces one outcome per entry, decided against the tree the run resolved. A receipt is a cached copy of that answer, never a second authority for it.

| Outcome | Condition | Action |
|---|---|---|
| `unchanged` | Fingerprint or hash matches the resolved entry | Nothing |
| `restored` | Entry missing | Materialize from cache |
| `modified` | Entry differs from the resolved entry | Stop. Name every modified path. Require `--force` to overwrite or `--adopt` to accept as the new state |
| `foreign` | Entry present, not in the resolved tree | Stop and name it. Require `--force` to remove it or `--adopt` to accept it. Never deleted implicitly |

Every entry unchanged writes nothing: no staging directory, no rename, status `unchanged`, exit 0.

Entries missing and nothing modified or foreign builds only the missing entries in staging and publishes them into the destination one entry at a time. Anything modified or foreign stops the run before anything is staged and exits 60.

`--adopt` writes nothing and reports the tree the destination holds rather than the one that was resolved. `--force` overwrites through a full staging publication. Both are per-invocation and neither is ever implied.

Numbered directories are never created. A destination is never partially reconciled: it is never left holding a tree that is neither the one it held nor the one that was resolved.

## Partial success

Artifacts are independent. Objects that verified are kept in the cache and are reused on the next run.

The destination is all or nothing. If any selected artifact fails, nothing is published and the previous destination is left untouched.

The result reports each artifact separately with its own status and error.

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
| `--verbose` | off | Raise log level. Repeatable |
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
| `FETCHLOOM_LOG` | Log level |
| `FETCHLOOM_TOKEN_<HOST>` | Bearer credential scoped to that host |
| `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY` | Proxy policy |
| `XDG_CACHE_HOME` | Cache root on Linux |
| `NO_COLOR` | Disable color |

Precedence, highest first: command line, environment, project config, user config, built-in default. `explain` reports which level supplied each value.

## Configuration files

Configuration is TOML. Manifests accept three surface syntaxes because they are written by strangers; configuration accepts one because it is written by the user in front of us, and one format means one parser and one set of error messages.

Project configuration is `fetchloom.toml`, found by searching the working directory and then each parent until one is found or the filesystem root is reached. `explain` prints the path that was found, so the search is never a mystery.

User configuration is `config.toml` in the platform's own configuration location: `%APPDATA%\Fetchloom` on Windows, `~/Library/Application Support/fetchloom` on macOS, and `$XDG_CONFIG_HOME/fetchloom` or `~/.config/fetchloom` on Linux. This is the configuration location, not the cache location, and the two are never the same directory.

`--config` names one file and disables the search. `--no-config` disables both levels.

Unknown keys are an error and `x-` is reserved, as everywhere else.

## Credentials

Lookup order per host: matching `FETCHLOOM_TOKEN_<HOST>`, platform credential store, provider-native helper. First match wins and the source is reported without the secret.

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

Selection may change speed. It may never change bytes.

## Directory listing

A reference naming a container is expanded by listing that container. Fetchloom lists. It does not crawl.

Supported: object store listing APIs, provider repository and record APIs, WebDAV `PROPFIND`, FTP `LIST`, and standard generated HTML directory indexes.

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

The JSON result carries a `work` object holding `bytes_read`, `bytes_written`, and `requests`. Bytes read counts every byte the run read from a file, bytes written counts every byte it wrote to one, and requests counts every request it issued to a source, retries and probes included. All three are identical on identical inputs, so they are what a benchmark gates on. None of them is a duration.

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

| Limit | Default |
|---|---|
| Archive entries | 1,000,000 |
| Expanded bytes | 1 TiB |
| Expansion ratio | 200 |
| Path length | platform maximum |
| Nesting depth | 64 |
| Resident memory | 1 GiB |
| Redirects followed | 10 |
| Manifest size | 16 MiB |
| Manifest node count | 100,000 |
| Retry attempts | 5 |
| Retry ceiling | 60 s |
| Outboard threshold | 64 MiB |
| Outboard chunk group | 1 MiB |
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

## Cancellation

First interrupt: stop new work, finish flushing in-flight buffers, record resumable state, exit `130` within two seconds.

Second interrupt: abort immediately. Cache invariants still hold because nothing enters `objects/` without a completed verification and rename.

## Offline

`--offline` forbids DNS resolution, connection attempts, source probes, remote credential lookups, and update checks. Any operation requiring one of these fails with `policy.offline` before doing avoidable work. Plans produced offline mark every network-derived field as unknown.

## Determinism

These must hold under any configuration.

Adaptive tuning, concurrency, protocol choice, clone versus copy, and I/O mode may change timing and never change resulting bytes, digests, or the tree digest.

The same lock produces the same tree digest on every platform, or the run fails and names the entries that cannot be represented.

No correctness decision depends on the wall clock, on file modification times, or on the internal layout of the cache.

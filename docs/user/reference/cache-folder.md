# cache folder

the shared cache stores downloaded bytes by SHA-256 and keeps local records for reuse and recovery.

```sh
fetchloom cache
fetchloom cache verify
fetchloom cache gc
```

## location

| platform | default root |
|---|---|
| linux | `$XDG_CACHE_HOME/fetchloom` when that is an absolute path, else `$HOME/.cache/fetchloom` |
| macos | `$HOME/Library/Caches/fetchloom` |
| windows | `%LOCALAPPDATA%\fetchloom` |

nonempty `FETCHLOOM_CACHE` overrides the root and may be relative to the working directory after `-C`. on linux and macos, a cache folder fetchloom creates is readable by you only; one that already exists keeps its permissions.

```text
<cache>/
  CACHEDIR.TAG
  gc.lock
  reminded
  objects/<first 2 sha256 digits>/<remaining 62 digits>
  partial/<sha256 of URL>.part
  partial/<sha256 of URL>.json
  partial/<sha256 of URL>.lock
  urls/<sha256 of URL>.json
  stated/md5/<first 2 md5 digits>/<remaining 30 digits>-<sha256 of URL>
  unpacked/<archive sha256>
  projects/<sha256 of project root>.json
  changed/<object sha256>
  tmp/<random>.stage/lock
  tmp/<random>.stage/<random>.new
  tmp/
```

`CACHEDIR.TAG` starts with `Signature: 8a477f597d28d172789f06886806bc55` to let backup tools skip the cache. `gc.lock` is empty; commands that use the cache hold it with a shared OS lock and `cache gc` holds it alone, so gc waits for them ([runs that use the cache](../commands/cache.md#runs-that-use-the-cache)). `reminded` is empty; its modification time is when fetchloom last [checked for bytes no project needs](../commands/cache.md#reminder). opening, verifying and collecting the cache ignore unknown top-level files. on macos, first initialization also creates `.metadata_never_index` for spotlight and sets the time machine backup-exclusion attribute when the filesystem supports it. these markers belong to the cache root, not to each object.

objects hold read-only payloads. each archive stages its read-only `.new` members in a unique `.stage` folder under `tmp/`. its writer holds the empty `lock` file open with an exclusive OS lock through publication and cleanup.

the next unpack or gc removes a staging folder as soon as it can take that lock.

where locking fails, `unlocked` replaces `lock`, and cleanup waits until the folder and every file are older than 24 hours. the writer locks `lock.new` before renaming it to `lock`, so a folder without `lock` is still being set up and also waits for the age rule.

random names use 16 hexadecimal digits. unclaimed loose `.new` files use the age rule.

`tmp/` also holds atomic record writes, private downloads when file locking is unavailable, and the empty files that test whether the volume can hardlink. these records are local cache indexes, not files to commit.

## URL and partial records

`urls/` records are JSON written for revalidated, unpinned URLs. gc removes a record once neither its object nor the record of what it unpacked to is left:

| field | value |
|---|---|
| `url` | `sha256:` followed by the SHA-256 of the original URL's UTF-8 bytes; the same digest names the file |
| `sha256` | full object hash |
| `size` | total bytes |
| `name` | server-supplied file name, or null |
| `validators.etag` | `ETag` string, or null |
| `validators.last_modified` | `Last-Modified` string, or null |
| `validators.date` | response `Date` string, or null; required in every record |

partial JSON records have `url`, `received`, `sha256`, `length` and the same `validators` object. `url` uses the same fingerprint, so signed-link credentials stay out of records. `received` is the checkpoint byte count; `sha256` hashes that prefix; `length` is the total file size stated by the response, or null.

lookup requires the matching fingerprint and restores the request URL only in memory. cache bodies and other metadata are not sanitized output.

`ranges` lists pieces finished past the prefix, each with `start`, `end` and the `sha256` of its bytes; it is an empty array when there are none. every declared URL, validator and partial-record field is required. optional values are explicit nulls, not missing fields. incomplete records are discarded as hints.

progress is saved about once a second while writing. a complete known-size download of at most 1 MiB, started at offset 0 without retained ranges, needs no closing checkpoint; an interrupted prefix still gets one. resumption rehashes the prefix, keeps the ranges whose bytes still hash to their record when the download may split again, and truncates or overwrites the rest.

the `.lock` file coordinates writers; its contents are unused. a run waiting for another writer first flushes and publishes its own finished downloads, so runs sharing a cold cache do not wait for each other's unpublished files. gc removes the `.part` and `.json` of a download nobody holds once they are a day old, or at once with `cache gc --now`, and the `.lock` of any download nobody holds, finished or abandoned, without counting it among the freed leftovers. a run that opened a `.lock` just before gc removed it notices, once it holds the lock, that the name now points elsewhere or nowhere, and opens the name again, so two runs never own one download. without OS file locks, downloads use unique names under `tmp/` and cannot resume across runs.

## other indexes

- an MD5 mapping is `<sha256> <size>\n`, with a literal space and LF. it records an MD5 that a download of that URL computed and checked, and is used only for that URL, including when its object is no longer cached. a download pinned by SHA-256 records none.
- an unpack record uses the [tree encoding](data-lock.md#the-tree-hash). its hash is checked against `data.lock` when one is supplied, and a sync writes only a tree that matches the lock's `unpacks_to`; a cache hit for the whole archive also requires every member object.
- a project registration is JSON with one field, `root`, the project path. its filename hashes the path's OS-encoded bytes. registrations are written after successful project sync, including sync through `add` or `path`. `get` and standalone `lock` do not register.
- a change mark is a file named by an object's SHA-256. its first line is empty until a repair writes a time there; each later line records a copy found changed as four numbers: device and inode on unix, or volume and file index on windows, then size and modification time in nanoseconds since 1970. a project's hardlink that is still one of those copies, edited through some link, is put back by sync rather than kept as an edit; at most 16 copies are kept. it is written when fetchloom finds that object's bytes changed: in a project sync's hardlink audit, in the hash a marked or writable object gets before it is placed, in an archive that unpacked to another tree than data.lock pins, in `cache verify`, or when a new copy of an object meets an old one that no longer holds its bytes. its modification time records when. before a replacement, fetchloom refreshes the mark with an empty first line, keeping its time as when the change was found, so the old object stays in doubt. only once the new copy holds the object's name does it write that copy's modification time on the first line, in nanoseconds since 1970; a failed replacement leaves that line empty, so the changed object is hashed and refused. the cache trusts objects written well after that time, and a project's hardlink that is the very file now in the cache is the replacement, so only projects linking the old copy recheck it. gc removes a mark only after its object is gone and no registered project needs it. [changed hardlinks and aliases](../../dev/architecture/placing.md#changed-hardlinks-and-aliases) explains what other projects do with a mark.

archive bytes may be deleted after successful unpacking when no registered project or current lock needs them packed. deletion failures or unreadable project locks leave them cached. members remain ordinary objects. [cache commands](../commands/cache.md) explain retention and gc.

project-local lock and placement records live in [the project folder](project-folder.md), outside the cache.

## sharing and retention

[cache sharing](../concepts.md#cache) covers per-user caches and trusted group access.

a cache folder you may not write to fails a download with `local.io` (exit 50), and the message names the folder that refused, such as `<cache>/partial`. fix its permissions, or set `FETCHLOOM_CACHE` to a folder you may write to.

on unix, registrations and change marks flush their containing directory after replacement, as well as their file contents. a reported directory-flush failure means the new name exists but its persistence is uncertain; fix the storage error and retry. a registration turns read-only once its directory is flushed, so a sync that finds it read-only and unchanged writes nothing; one still writable has its directory flushed again first. this directory guarantee is not extended to windows here. a lost new cache object name after a power cut causes a later fetch, so it does not substitute for a project's pinned lock.

an unreadable `changed/` folder or change mark stops sync and get with `local.io` (exit 50), before they place files or record a completed sync. fix storage access and read/search permissions, run `fetchloom cache verify`, then retry. keep the marks: deleting them can hide damage from another project's hardlinks.

objects are not rehashed on every use. run `fetchloom cache verify` to check their bytes. [cache commands](../commands/cache.md) covers the 24-hour collection rule, registrations and partial failures.

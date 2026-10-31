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
| linux | `$XDG_CACHE_HOME/fetchloom`, else `$HOME/.cache/fetchloom` |
| windows | `%LOCALAPPDATA%\fetchloom` |

nonempty `FETCHLOOM_CACHE` overrides the root and may be relative to the working directory after `-C`.

```text
<cache>/
  CACHEDIR.TAG
  objects/<first 2 sha256 digits>/<remaining 62 digits>
  partial/<sha256 of URL>.part
  partial/<sha256 of URL>.json
  partial/<sha256 of URL>.lock
  urls/<sha256 of URL>.json
  stated/md5/<first 2 md5 digits>/<remaining 30 digits>
  unpacked/<archive sha256>
  projects/<sha256 of project root>.json
  changed/<object sha256>
  tmp/<random>.stage/lock
  tmp/<random>.stage/<random>.new
  tmp/
```

`CACHEDIR.TAG` starts with `Signature: 8a477f597d28d172789f06886806bc55` to let backup tools skip the cache. the cache writes no `layout` marker. existing `layout` files are left untouched. opening, verifying and collecting the cache ignore unknown top-level files.

objects hold read-only payloads. each archive stages its read-only `.new` members in a unique `.stage` folder under `tmp/`. its writer holds the empty `lock` file open with an exclusive OS lock through publication and cleanup.

the next unpack or gc removes a staging folder as soon as it can take that lock.

where locking fails, `unlocked` replaces `lock`, and cleanup waits until the folder and every file are older than 24 hours. the writer locks `lock.new` before renaming it to `lock`, so a folder without `lock` is still being set up and also waits for the age rule.

random names use 16 hexadecimal digits. older loose `.new` files still use the age rule.

`tmp/` also holds atomic record writes, private downloads when file locking is unavailable, and the empty files that test whether the volume can hardlink. these records are local cache indexes, not files to commit.

## URL and partial records

`urls/` records are JSON written for revalidated, unpinned URLs:

| field | value |
|---|---|
| `url` | original download URL, also the UTF-8 input to the filename hash |
| `sha256` | full object hash |
| `size` | total bytes |
| `name` | server-supplied file name, or null |
| `validators.etag` | `ETag` string, or null |
| `validators.last_modified` | `Last-Modified` string, or null |

partial JSON records have `url`, `received`, `sha256`, `length` and the same `validators` object. `received` is the checkpoint byte count; `sha256` hashes that prefix; `length` is the total file size stated by the response, or null.

a split download adds `ranges`, the pieces finished past the prefix, each with `start`, `end` and the `sha256` of its bytes; the field is absent when there are none.

progress is saved about once a second while writing. resumption rehashes the prefix, keeps the ranges whose bytes still hash to their record when the download may split again, and truncates or overwrites the rest.

the `.lock` file coordinates writers; its contents are unused. gc removes an abandoned download's `.part` and `.json` but keeps its `.lock`, since a run waiting for that download may already have it open. without OS file locks, downloads use unique names under `tmp/` and cannot resume across runs.

## other indexes

- an MD5 mapping is `<sha256> <size>\n`, with a literal space and LF. it records a previously checked MD5, including when its object is no longer cached.
- an unpack record uses the [tree encoding](data-lock.md#the-tree-hash). its hash is checked against `data.lock` when one is supplied, and a sync writes only a tree that matches the lock's `unpacks_to`; a cache hit for the whole archive also requires every member object.
- a project registration is JSON with one field, `root`, the project path. its filename hashes the path's OS-encoded bytes. registrations are written after successful project sync, including sync through `add` or `path`. `get` and standalone `lock` do not register.
- a change mark is an empty file named by an object's SHA-256. it is written when fetchloom finds that object's bytes changed: in a project sync's hardlink audit, in the hash a marked or writable object gets before it is placed, in an archive that unpacked to another tree than data.lock pins, in `cache verify`, or when a new copy of an object meets an old one of another size. its modification time records when. each replacement refreshes the mark and dates it just before the new bytes were written, so projects recheck old hardlinks without putting the good object in doubt. gc removes a mark only after its object is gone and no registered project needs it. [changed hardlinks and aliases](../../dev/architecture/placing.md#changed-hardlinks-and-aliases) explains what other projects do with a mark.

archive bytes may be deleted after successful unpacking when no registered project or current lock needs them packed. deletion failures or unreadable project locks leave them cached. members remain ordinary objects. [cache commands](../commands/cache.md) explain retention and gc.

project-local lock and placement records live in [the project folder](project-folder.md), outside the cache.

## sharing and retention

[cache sharing](../concepts.md#cache) covers per-user caches and trusted group access.

objects are not rehashed on every use. run `fetchloom cache verify` to check their bytes. [cache commands](../commands/cache.md) covers the 24-hour collection rule, registrations and partial failures.

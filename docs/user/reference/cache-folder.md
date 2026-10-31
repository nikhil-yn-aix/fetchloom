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

objects hold read-only payloads. each archive stages its read-only `.new` members in a unique `.stage` folder under `tmp/`. its writer holds the empty `lock` file open with an exclusive OS lock through publication and cleanup. the next unpack or gc removes a staging folder as soon as it can take that lock. where locking fails, `unlocked` replaces `lock`, and cleanup waits until the folder and every file are older than 24 hours. the writer locks `lock.new` before renaming it to `lock`, so a folder without `lock` is still being set up and also waits for the age rule. random names use 16 hexadecimal digits. older loose `.new` files still use the age rule.

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

partial JSON records have `url`, `received`, `sha256`, `length` and the same `validators` object. `received` is the checkpoint byte count; `sha256` hashes that prefix; `length` is the total file size stated by the response, or null. a split download adds `ranges`, the pieces finished past the prefix, each with `start`, `end` and the `sha256` of its bytes; the field is absent when there are none. progress is saved about once a second while writing. resumption rehashes the prefix, keeps the ranges whose bytes still hash to their record when the download may split again, and truncates or overwrites the rest.

the `.lock` file coordinates writers; its contents are unused. gc removes an abandoned download's `.part` and `.json` but keeps its `.lock`, since a run waiting for that download may already have it open. without OS file locks, downloads use unique names under `tmp/` and cannot resume across runs.

## other indexes

- an MD5 mapping is `<sha256> <size>\n`, with a literal space and LF. it records a previously checked MD5, including when its object is no longer cached.
- an unpack record uses the [tree encoding](data-lock.md#the-tree-hash). its hash is checked against `data.lock` when one is supplied, and a sync writes only a tree that matches the lock's `unpacks_to`; a cache hit for the whole archive also requires every member object.
- a project registration is JSON with one field, `root`, the project path. its filename hashes the path's OS-encoded bytes. registrations are written after successful project sync, including sync through `add` or `path`. `get` and standalone `lock` do not register.
- a change mark is an empty file named by an object's SHA-256. it is written when fetchloom finds that object's bytes changed: in a project sync's hardlink audit, in the hash a marked or writable object gets before it is placed, in an archive that unpacked to another tree than data.lock pins, in `cache verify`, or when a new copy of an object meets an old one of another size. its modification time records when. each replacement refreshes the mark and dates it just before the new bytes were written, so projects recheck old hardlinks without putting the good object in doubt. gc removes a mark only after its object is gone and no registered project needs it. [placement](../../dev/architecture/README.md) explains what other projects do with a mark.

archive bytes may be deleted after successful unpacking when no registered project or current lock needs them packed. deletion failures or unreadable project locks leave them cached. members remain ordinary objects. [cache commands](../commands/cache.md) explain retention and gc.

## the .fetchloom folder

`.fetchloom` must be a real directory. a symlink or junction is refused before fetchloom writes its lock, owner, state or ignore file.

| path | contents |
|---|---|
| `.fetchloom/.gitignore` | `*` and LF, written when the folder is created |
| `.fetchloom/lock` | OS project lock; file contents are unused |
| `.fetchloom/owner` | JSON identifying the modifying command, shared by windows and WSL |
| `.fetchloom/owner.<pid>.<16 hex digits>.tmp` | owner record being written before hardlink publication; a hard kill can leave this file behind |
| `.fetchloom/state` | compact JSON describing tracked placements |
| `.fetchloom/.<name>.<8 hex digits>.tmp` | metadata being written; a hard kill can leave this file behind |
| `.fetchloom/.fetchloom-<16 hex digits>.probe` | an empty hidden file, there only while a removal tests whether the volume can hardlink |

inside a git repository, `add` adds `/<data_dir>/` to the project's `.gitignore` if absent. `.fetchloom/` ignores itself separately.

owner fields:

| field | value |
|---|---|
| `os` | operating system name, such as `windows` or `linux` for WSL |
| `pid` | process id on that operating system |
| `started` | UTC time when the command claimed the project, in `YYYY-MM-DDTHH:MM:SSZ` form |
| `command` | subcommand name, without arguments or credentials |

fetchloom takes the OS lock, writes and flushes the complete owner JSON to `owner.<pid>.<16 hex digits>.tmp` beside `owner`, then hardlinks that file to `owner`. the hardlink fails if `owner` already exists, so a newly published owner is never empty. the temporary name is removed after the attempt. when hardlinks fail for another reason, fetchloom falls back to exclusive creation followed by a temporary-file write and rename; only this fallback can expose an empty reservation. dropping the command's guard removes `owner` before releasing the OS lock, including when the command returns an error.

an existing owner is stale when its `os` matches this process and this process holds the OS lock, or when the file is empty and its modification time is more than ten seconds old. without a working OS lock, a same-os record is not stale. before removing a stale record, fetchloom reads it again and requires the same bytes and modification time. any other record, including nonempty invalid JSON, makes the command wait. the waiting message identifies the holder when readable and gives the exact `owner` path to delete if that process is gone. a record from another os is never reclaimed by its age or pid. delete it only after that process has ended.

state fields:

| field | value |
|---|---|
| `version` | integer `1` |
| `files` | object keyed by project-relative path, using `/` |
| `files[path].dataset` | dataset name |
| `files[path].sha256` | locked member or file hash, 64 hex digits |
| `files[path].method` | `reflink`, `hardlink` or `copy`. an adopted file is `hardlink` when it is the cached object's own file, else `copy` |
| `files[path].look.size` | byte count |
| `files[path].look.modified` | modification time in nanoseconds since 1970; zero if unavailable, capped at the largest unsigned 64-bit integer |
| `files[path].writable` | `true` when the file was placed or adopted writable |
| `files[path].checked` | start of the scan used to place, adopt or vouch for the file, in nanoseconds since 1970; marks written during that run still put the file in doubt |
| `synced` | absent until a `sync` records it. `manifest` and `lock` hold the SHA-256 of `data.toml` and `data.lock` as that sync left them, in hex, and `complete` is `true` when every file they ask for ended up in place; [the project loop](../../dev/architecture/README.md) uses it to skip planning |

there is no inode or file-id field. missing, unreadable, malformed or unsupported-version state counts as empty. matching existing files can then be adopted into state and later removed as managed files. metadata comparisons do not prove content integrity.

a file whose `look.modified` is not older than the state file's own modification time is hashed before its look is trusted, since both times come from one filesystem clock and a coarse clock hides an edit made in the same tick. a hash that differs counts as an edit. a sync that finds such files unchanged writes the state again, which settles them.

## sharing and retention

one user can share a cache between processes and projects. anyone who can write to a shared cache is trusted. fetchloom has no cross-user permissions or administration layer.

objects are not rehashed on every use. run `fetchloom cache verify` to check their bytes. [cache commands](../commands/cache.md) covers the 24-hour collection rule, registrations and partial failures.

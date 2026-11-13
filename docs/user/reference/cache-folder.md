# cache folder

the cache is one folder that keeps each file by its SHA-256, with the records fetchloom needs to reuse and check them. [cache](../../dev/architecture/cache.md) in the developer guide has the mechanics.

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
  partial/<first sha256 digit>/<sha256 of URL>.part
  partial/<first sha256 digit>/<sha256 of URL>.json
  partial/<first sha256 digit>/<sha256 of URL>.lock
  urls/<first sha256 digit of URL>.log
  urls/<first sha256 digit of URL>.idx
  urls/<first sha256 digit of URL>.lock
  stated/md5/<first 2 md5 digits>/<remaining 30 digits>-<sha256 of URL>
  unpacked/<archive sha256, or the key a split archive's parts give>
  projects/<sha256 of project root>.json
  changed/<object sha256>
  imported/<sha256 of the list>
  builds/<key sha256>/<tree sha256>
  builds/<key sha256>/lock
  tmp/<random>.stage/lock
  tmp/<random>.stage/<random>.new
  tmp/<random>.build/lock
  tmp/<random>.build/in/<dataset>/...
  tmp/<random>.build/out/...
  tmp/
```

`CACHEDIR.TAG` lets backup tools skip the cache, and on macos the cache is also kept out of spotlight and time machine. `gc.lock` is empty; commands that use the cache share a lock on it and `cache gc` takes it alone, as [runs that use the cache](../commands/cache.md#runs-that-use-the-cache) says. `reminded` is empty; its modification time is when fetchloom last [checked for bytes no project needs](../commands/cache.md#reminder).

`objects/` holds the files, read-only. a file whose modification time moved since it entered the cache was written since, and is hashed before use. `tmp/` holds work in progress: an archive's files while they unpack, each recipe step's `in/` and `out/` folders, which only you may enter on linux and macos, and records being written. a run that was killed leaves its folder there, and the next run or gc removes it.

## URL and partial records

`urls/` records what a link served last, so a later run can ask the server only whether it changed, and an offline `get` can do without a site's listing. a record is one line of JSON in one of 16 `.log` files, picked by the first digit of the SHA-256 of the link, after the first 16 digits of the SHA-256 of the JSON and a space; a line that does not match its digits, as one a killed run cut short, is passed over. beside each `.log`, its `.idx` holds a 12-byte entry for each line: the first 8 bytes of the link's SHA-256, then where the line starts, both little-endian. a new record for a link is appended, and the last one counts. a run appending holds a lock on the shard's empty `.lock` file, never on the records, so other runs read them meanwhile even on windows, whose locks keep every other reader off what they cover; where the filesystem cannot lock, runs append without it, and two at once may lose a record but never mix one up, since each names its link. gc writes each file again with only the last record of each link, and drops a record once neither its file nor what it unpacked to is left:

| field | value |
|---|---|
| `url` | `sha256:` followed by the SHA-256 of the original URL's UTF-8 bytes |
| `sha256` | full object hash |
| `size` | total bytes |
| `name` | server-supplied file name, or null; for a link a `get` kept one file from, that file's path in the listing |
| `validators.etag` | `ETag` string, or null |
| `validators.last_modified` | `Last-Modified` string, or null |
| `validators.date` | response `Date` string, or null; required in every record |
| `validators.version` | the version an S3 bucket that keeps versions named for the bytes, its `x-amz-version-id`, or null |

a link is named by a fingerprint, the SHA-256 of its text, so a secret in a signed link never reaches a record. a download a run stopped keeps its bytes as `partial/<digit>/<fingerprint>.part`, with a `.json` record of how many bytes arrived, their SHA-256, the file's stated length, the server's validators and any pieces finished past them, so the next run can continue it; its `.lock` keeps two runs from writing one download. gc removes an unfinished download nobody came back for after a day, or at once with `cache gc --now`. without file locks, downloads cannot continue across runs.

## other indexes

- `stated/md5/` maps an MD5 a source stated for a link to the SHA-256 and size of the bytes that matched it, as `<sha256> <size>`, with the S3 version after them when the bucket keeps versions, so a later download of that link can be skipped.
- `unpacked/` holds what each archive unpacked to, in the [tree encoding](data-lock.md#the-tree-hash), checked against the `unpacks_to` data.lock pins.
- `builds/<key>/<tree>` records what a recipe's step made, in the same encoding, under its key, so a later build can reuse it. on linux and macos a build reuses only records you wrote and no one else may change.
- `projects/` registers each project a sync placed files for, as JSON naming its folder, so `cache gc` knows what it needs.
- `imported/` lists the cached files an import or the removal of an import put or found there, one SHA-256 a line, one list a run. no source can give them back, so `cache gc` counts them apart and asks before `--now` frees them, as [gc](../commands/cache.md#gc) says. gc drops a list once none of its files is left.
- `changed/` holds a mark for each cached file found changed, as by an edit through a hardlink, naming the copies found changed, so sync can put each project's link back. [changed hardlinks and aliases](../../dev/architecture/placing.md#changed-hardlinks-and-aliases) explains how they are used.

an archive's own bytes leave the cache once its files are in, unless a registered project needs it packed. [cache commands](../commands/cache.md) explain retention and gc.

project-local lock and placement records live in [the project folder](project-folder.md), outside the cache.

## sharing and retention

[cache sharing](../concepts.md#cache) covers per-user caches and trusted group access.

a cache folder you may not write to fails a download with `local.io` (exit 50), and the message names the folder that refused, such as `<cache>/partial`. fix its permissions, or set `FETCHLOOM_CACHE` to a folder you may write to.

an unreadable `changed/` folder or change mark stops sync and get with `local.io`, exit 50, before they place files. fix the storage or its permissions, run `fetchloom cache verify`, then retry; keep the marks, since deleting them can hide damage from another project's hardlinks. a file the cache got just before a power cut can be lost and is then downloaded again, so the cache never stands in for a project's pins.

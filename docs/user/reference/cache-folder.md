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

nonempty `FETCHLOOM_CACHE` overrides the root and may be relative to the working directory after `-C`. on linux and macos, a cache folder fetchloom creates is readable by you only; one that already exists keeps its permissions. a run that opens a cache folder another user owns, or one every user of the machine may write to, warns with `cache.foreign` once, unless a group shares it on purpose, as a [lab's cache](../guides/clusters.md#one-cache-for-a-whole-lab) is: a setgid folder its group may write and no one else, in a group you are in, and so does one every user may enter when a folder fetchloom writes in inside it, such as `objects/` or `tmp/`, is another user's or every user may write to it, or when every user may write one of the folders in `objects/`, as a run with `umask 000` makes them. `status` warns as the commands that write do, and `fetchloom cache` also warns of each cached file every user may write.

fetchloom makes its cache only in an empty folder or a new one, and knows a cache from then on by its `CACHEDIR.TAG`. on windows, the folder where fetchloom remembers how fast each host is, `%LOCALAPPDATA%\fetchloom\state`, sits in the default cache folder, and a run with the cache elsewhere can make it first: a folder holding only that still counts as empty. a folder holding other files and no such tag, such as a home folder or the top of a usb drive, is refused and left as it is, even one whose folders have the names below, so a run stops with `local.io` (exit 50) and `settings set cache` saves nothing; a new folder inside it, such as `<folder>/fetchloom-cache`, works. a folder that holds only what a cache holds but no tag looks like a cache whose tag is gone, as a cleaner can leave one: it is refused the same way, and the help gives a command that puts the tag back and refuses to write over a file or a link at its name, on linux and macos `(set -C; printf 'Signature: 8a477f597d28d172789f06886806bc55\n# a cache made by fetchloom\n' > <cache>/CACHEDIR.TAG)`, never a new cache inside it, which would download everything again. fetchloom moves no cache: after `settings set cache` or `settings unset cache` moves it, the old one stays, and the command says what it holds and what deleting it frees, since a file placed in data/ as a link to a cached file keeps those bytes on the disk.

on windows a folder takes its permissions from the folders above it, and fetchloom changes none. inside your user profile, as the default in `%LOCALAPPDATA%` is, they keep the cache yours. a cache folder outside the profile, such as `C:\fetchloom-cache` or `D:\cache`, may let other users of the machine change the files your projects place from it, so each run that opens it warns with `cache.foreign` once. a cache on a FAT or exFAT drive, which keeps no permissions, is never warned about. a team that shares a cache on purpose can leave the warning.

```text
<cache>/
  CACHEDIR.TAG
  gc.lock
  reminded
  objects/<first 2 sha256 digits>/<remaining 62 digits>
  partial/<first sha256 digit>/<fingerprint>.part
  partial/<first sha256 digit>/<fingerprint>.json
  partial/<first sha256 digit>/<fingerprint>.lock
  urls/<first sha256 digit of URL>.log
  urls/<first sha256 digit of URL>.idx
  urls/<first sha256 digit of URL>.lock
  stated/md5/<first 2 md5 digits>/<remaining 30 digits>-<sha256 of URL>
  unpacked/<archive sha256, or the key a split archive's parts give>
  projects/<sha256 of project root>.json
  changed/<object sha256>
  imported/<sha256 of the list>
  tmp/<random>.stage/lock
  tmp/<random>.stage/<random>.new
  tmp/
```

`CACHEDIR.TAG` lets backup tools skip the cache, and on macos the cache is also kept out of spotlight and time machine. `gc.lock` is empty; commands that use the cache share a lock on it and `cache gc` takes it alone, as [runs that use the cache](../commands/cache.md#runs-that-use-the-cache) says. `reminded` is empty; its modification time is when fetchloom last [checked for bytes no project needs](../commands/cache.md#reminder).

`objects/` holds the files, read-only. a file whose modification time moved since it entered the cache was written since, and is hashed before use. `tmp/` holds work in progress: an archive's files while they unpack, and records being written. a run that was killed leaves its folder there, and the next run or gc removes it.

## URL and partial records

`urls/` records what a link served last, so a later run can ask the server only whether it changed, and an offline `get` can do without a site's listing. a record is one line of JSON in one of 16 `.log` files, picked by the first digit of the SHA-256 of the link, after the first 16 digits of the SHA-256 of the JSON and a space; a line that does not match its digits, as one a killed run cut short, is passed over. beside each `.log`, its `.idx` holds a 12-byte entry for each line: the first 8 bytes of the link's SHA-256, then where the line starts, both little-endian. a new record for a link is appended, and the last one counts. a run appending holds a lock on the shard's empty `.lock` file, never on the records, so other runs read them meanwhile even on windows, whose locks keep every other reader off what they cover; where the filesystem cannot lock, runs append without it, and two at once may lose a record but never mix one up, since each names its link. gc writes each file again with only the last record of each link, and drops a record once neither its file nor what it unpacked to is left, unless a project's data.lock pins what it served as an archive or a part of one, which an upgrade asks about again:

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
| `page` | `true` when the bytes came as a web page; absent otherwise. a later run that takes the bytes from the cache, offline or after the server answered that they are unchanged, refuses them unless it asks for the page, as a fresh answer would be refused |

a link is named by a fingerprint, the SHA-256 of its text, so a secret in a signed link never reaches a record. a download a run stopped keeps its bytes as `partial/<digit>/<fingerprint>.part`, the fingerprint of the pinned SHA-256 for a file data.lock pins, so a mirror's, an archive's or a new link's download of that file goes on from bytes another left, else of the link, with a `.json` record of how many bytes arrived, their SHA-256, the file's stated length, the server's validators, any pieces finished past them and the file name the server gave, so the next run can continue it; a `.part` with no `.json`, as a run killed in a download's first second leaves, is continued only for a file data.lock pins, whose SHA-256 then checks it. its `.lock` keeps two runs from writing one download. gc removes an unfinished download nobody came back for after a day, or at once with `cache gc --now`. without file locks, downloads cannot continue across runs, so a stop deletes what an unfinished download brought and says so.

## other indexes

- `stated/md5/` maps an MD5 a source stated for a link to the SHA-256 and size of the bytes that matched it, as `<sha256> <size>`, with the S3 version after them when the bucket keeps versions, so a later download of that link can be skipped.
- `unpacked/` holds what each archive unpacked to, in the [tree encoding](data-lock.md#the-tree-hash), checked against the `unpacks_to` data.lock pins.
- `projects/` registers each project a sync placed files for, as JSON naming its folder, so `cache gc` knows what it needs. a record that is a link, a fifo or anything but a small plain file is never read: `cache`, `cache gc` and `drop` stop with `local.io`, exit 50, and name it, and removing it is safe, since a sync in a project writes that project's record again.
- `imported/` lists the cached files an import or the removal of an import put or found there, one SHA-256 a line, one list a run. no source can give them back, so `cache gc` counts them apart and asks before `--now` frees them, as [gc](../commands/cache.md#gc) says. gc drops a list once none of its files is left.
- `changed/` holds a mark for each cached file found changed, as by an edit through a hardlink, naming the copies found changed, so sync can put each project's link back. [changed hardlinks and aliases](../../dev/architecture/placing.md#changed-hardlinks-and-aliases) explains how they are used.

an archive's own bytes leave the cache once its files are in, unless a registered project needs it packed. [cache commands](../commands/cache.md) explain retention and gc.

project-local lock and placement records live in [the project folder](project-folder.md), outside the cache.

## sharing and retention

[cache sharing](../concepts/cache-and-placement.md#cache) covers per-user caches and trusted group access.

a cache folder you may not write to fails a download with `local.io` (exit 50), and the message names the folder that refused, such as `<cache>/partial`. fix its permissions, or set `FETCHLOOM_CACHE` to a folder you may write to.

an unreadable `changed/` folder or change mark stops sync and get with `local.io`, exit 50, before they place files. fix the storage or its permissions, run `fetchloom cache verify`, then retry; keep the marks, since deleting them can hide damage from another project's hardlinks. a file the cache got just before a power cut can be lost and is then downloaded again, so the cache never stands in for a project's pins.

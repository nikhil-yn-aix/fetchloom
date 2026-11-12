# push

`push` copies the files `data.lock` pins from this machine's cache into a folder [mirror](../mirrors.md), downloading first what the cache lacks, so a teammate, a machine without a network or a reviewer can sync the project from there.

```sh
fetchloom push --to /mnt/lab/fetchloom
fetchloom push iris --dry-run
fetchloom push --inputs clean
fetchloom push --to /media/usb/fetchloom --json
```

## usage

```sh
fetchloom push [options] [name]...
```

`name` is a dataset in `data.toml`; with none, every dataset `sync` places is pushed, so one with `place = false` stays off the mirror unless it is named or `--inputs` adds it. an unknown name is a usage error, exit 2, that suggests the closest name. `--inputs` adds what the named [recipes](../recipes.md) are made from.

| option | effect |
|---|---|
| `--to <folder>` | the mirror's folder. without it, the `push_to` setting names it, from the [settings file](../reference/environment.md#settings-file) or `FETCHLOOM_PUSH_TO`; with neither, push stops with `usage`, exit 2 |
| `--inputs` | also push the datasets the named recipes are made from, and what those are made from in turn, so a rebuild on another machine finds them on the mirror. with no name, of every recipe |
| `-n`, `--dry-run` | say what would be downloaded, what would be copied and what is there already; change nothing |

[global options](README.md#global-options) also apply. `--yes` answers the [question](#data-you-may-not-share) about data whose source wanted an account or accepted terms.

## what it writes

push writes the layout every mirror is read in: the `fetchloom-mirror` file, each file a dataset places under `objects/` by its SHA-256, and for each archive that unpacks, the list of the files it holds under `trees/`, by the `unpacks_to` data.lock pins. with that list, a reader takes the archive's files one by one and never needs the archive, so push leaves the archive out, every part of one split into parts too; an archive with `extract = false` is a file of its own and is pushed. a member a [members table](../reference/data-toml.md#files-inside-archives) leaves out, as `fetchloom drop` writes one, is not placed, so it is not pushed: a reader who needs that member finds neither it nor the archive on the mirror, and in mirror-only mode gets `<archive>: not there on <mirror>`, while the same push run again skips every file. to give it to them, widen the members table, run `fetchloom sync`, which places those members from the same pinned archive and downloads it again when gc freed it, check them with `fetchloom status --verify`, and push again. a file several datasets need goes once. push writes nothing else: not data.toml, data.lock or code. it never changes the project. what the cache lacks it takes from the data folder or downloads first, as [what it fetches](#what-it-fetches) says.

from a real run, a project with a csv file and a zip that unpacks to five files, with the mirror's path shortened, on stderr:

```text
    Started a mirror in /mnt/lab/fetchloom
     Pushed 6 files · 106 B to /mnt/lab/fetchloom
         Ok every file iris and pets need is on the mirror
```

each file is written under a temporary name in the mirror's `tmp/` folder, read-only, and hashed as it is written. once all are written, they go to the disk as the cache's new files do: up to 64 flushed one by one, more with one flush of the whole disk where the system can. only then is each read back from the disk, hashed again and given its name, with a rename that never replaces a file already there. a file that reads back other bytes than were written never gets a name, and push stops with `integrity.mismatch`, exit 30. so does a copy in the cache that no longer holds its pinned bytes. on linux push drops the file from memory before reading it back, and on macos asks to read past memory; a network disk, or windows, may still answer from memory, and every reader checks each file against its pin again.

push knows the file it wrote by its number on the disk, so another file put in its place shows at the read-back. a network disk may forget a file's number and give it a new one, as smb mounted with `noserverino` and sshfs without `use_ino` do. on linux and macos push still takes such a file when the account that wrote it owns it and its size is right: it makes the file read-only, flushes it again and reads it back like any other. a disk that refuses the change of mode leaves the file as it was, and it still takes its name, since its bytes are checked. another account's file, one of another size, and on windows any file with a new number were put there by something else, so they get no name, and push stops with `local.io`, exit 50.

a push stopped with ctrl-c keeps the files that already have their names, gives no name to a file it has not read back and removes its unfinished files from `tmp/` before it ends; a second ctrl-c ends it at once and may leave them, as a kill does. once every file has its name, a ctrl-c while the disk flushes the mirror's folders lets the push finish: it says `Finishing what it started (ctrl-c again to quit at once)` and ends with its usual lines, exit 0. a push that is killed leaves its unfinished files under their temporary names, never under their own, and a later push removes temporary files in `tmp/` that are more than a day old. a new mirror's `fetchloom-mirror` file is on the disk, its name too, before the first file is pushed, so a power cut never leaves pushed files in a folder that does not say it is a mirror.

a file already there is found with one look at its name and size, and left as it is. pushing the same project again:

```text
    Skipped 6 files already there · 106 B
         Ok every file iris and pets need is on the mirror
```

a copy of another size, or a link or anything else that is no file in a file's place, is moved aside, and once the checked copy has its name, removed:

```text
     Pushed 1 file · 12 B to /mnt/lab/fetchloom
   Replaced 1 damaged copy on the mirror
    Skipped 5 files already there · 94 B
         Ok every file iris and pets need is on the mirror
```

one look cannot see a copy of the right size with other bytes. a reader never uses it, since it checks every file, and `sync` warns that the mirror holds a damaged copy; delete that file, and the next push writes it again.

two pushes into one mirror at once need no lock: each writes its own temporary files, and a name one of them gives a file first keeps that file. every folder in the mirror is opened from the mirror's top folder without following a link, so a link someone planted inside the mirror is refused, never written through. the top folder itself may be a link.

new files are read-only. new folders take the permissions mask of the account that makes them, so a lab folder with the setgid bit and `umask 002` lets the whole group push, as a [shared cache](../hpc.md#the-cache-on-a-shared-disk) does.

## where it writes

| the folder | what push does |
|---|---|
| a mirror, with its `fetchloom-mirror` file | adds what it lacks |
| new, in a folder that is there, or empty | starts a mirror there, then fills it |
| holding files but no `fetchloom-mirror` file | stops with `local.not_empty`, exit 50, and writes nothing: it may be anything, such as the empty folder a network disk is mounted on |
| not there, nor the folder above it | stops with `local.io`, exit 50, as when a disk is not mounted |
| a web address | stops with `usage`, exit 2 |

push writes folders only: a lab nas, a network share or a usb drive. to fill a web mirror or a bucket, push to a folder, then copy that folder with a tool such as rclone or rsync, which copy only what changed:

```sh
fetchloom push --to /srv/fetchloom
rclone copy /srv/fetchloom lab-bucket:fetchloom
```

a FAT32 drive refuses a file larger than 4 GiB, so push stops at the first one with `local.io`; exFAT has no such limit.

## what it fetches

push copies what this machine's cache holds, and fills it first with what it lacks. a file the cache lacks that the data folder holds, at a path data.lock places it at, is copied into the cache, as in a project [moved to another machine](cache.md#moving-a-project-to-another-machine) with its data folder. the copy is a clone where the disk makes one, never a link, and the cache keeps it only when its bytes hash to the pin, so the file in the data folder stays as it is. stderr from a real run in such a copy, whose cache was empty, the mirror's path shortened:

```text
     Copied 1 file · 21 B from data into the cache
    Started a mirror in /mnt/lab/fetchloom
     Pushed 1 file · 21 B to /mnt/lab/fetchloom
         Ok every file squares and table need is on the mirror
```

what neither holds, push downloads as a sync would: from a [mirror](../mirrors.md) when one holds it, else from its source, each file checked against its pin. an archive whose files this machine never unpacked, as in a copy whose cache is empty, is downloaded and unpacked to learn them, so its files and their list reach the mirror. so is the raw data of a dataset with `place = false`, which no sync downloads, when `--inputs` or its name asks for it. stderr from a real run in a copy of a project with a csv file, a zip and a recipe, whose cache was empty, the mirror's path shortened:

```text
 Downloaded 1 file · 824 B in 6 ms
   Unpacked pets/pets.zip · 6 files · 639 B in 56 ms
     Copied 2 files · 3.9 KiB from data into the cache
    Started a mirror in /mnt/lab/fetchloom
     Pushed 8 files · 4.6 KiB to /mnt/lab/fetchloom
         Ok every file clean, iris and pets need is on the mirror
```

offline, push downloads nothing, and in [mirror-only mode](../mirrors.md) it asks no source. a file it would have to download then stops it before anything is written, with `offline.missing` or `network.refused`, exit 20, naming the files, with help for the next push. offline, from the same copy:

```text
error: offline, and 1 file data.lock names is not in the cache
  pets/pets.zip
  help: run it again without --offline to download them, or set FETCHLOOM_CACHE to a cache folder that already holds them, or name a folder mirror that holds them with `--mirror <folder>`
```

a file in the data folder with other bytes than its pin counts as missing, even with the pinned size, so push downloads the pinned bytes instead and never changes it. push reads such a file once, as it copies it, after any [question](#data-you-may-not-share), so a file the state does not vouch for that turns out to hold other bytes is downloaded then.

push sends what data.lock pins, so a dataset `data.lock` does not pin as `data.toml` asks stops it with `lock.outdated`, exit 1, before anything is written: run `fetchloom lock`. push never builds: a [recipe](../recipes.md) whose result data.lock does not pin stops it with `build.needed`, exit 60. a result data.lock pins is pushed by the files data.lock lists for it, which the cache holds once a build made them. no download brings them, so a file of one that the cache lacks stops push before anything downloads, with `offline.missing`, exit 20, named with the build that makes it. the JSON `fix` is null, since a build runs the project's code. from a clone that never built:

```text
error: this machine's cache lacks files the push needs, which only a build makes
  clean/clean.csv
  help: `fetchloom build clean` makes clean's files
```

`fetchloom push --inputs clean` sends `clean`'s result with every dataset it is made from, so a reviewer whose sources are gone can rebuild it from the mirror and check it against data.lock. with that raw data on the mirror, [`cache gc --inputs`](cache.md#raw-input-a-mirror-holds) can free this machine's copy of it.

## data you may not share

`access` in [data.lock](../reference/data-lock.md) says what a dataset's source told about who may read it. a dataset whose source wanted an account (`login`) or accepted terms (`terms`), or did not say (`unknown`), is pushed only once you say yes, since anyone who can read the mirror can take its files without either. most sources say nothing, so most datasets ask. a recipe's result asks when a dataset it is made from does. a dataset whose files are all on the mirror already asks nothing. push asks before it downloads anything, so a no costs no request and adds nothing to the cache.

```text
iris needs accepted terms. anyone who can read /mnt/lab/fetchloom can take its files from there. push it anyway? [y/N]
```

no keeps everything as it was. `--yes` answers yes. without a terminal, and with `--json`, push stops before writing anything with `unconfirmed`, exit 1, naming the datasets; its JSON `fix` is null, since only a person may agree:

```text
error: the sources of iris and pets did not say who may read them, so pushing them needs a yes
  help: answer yes at a terminal, or add --yes once everyone who can read /mnt/lab/fetchloom may have them
```

`access` is a hint from the source, never a licence check. a yes does not give the right to share, and public data may still come with a licence that limits copies: a [mirror is not an archive](../mirrors.md#a-mirror-is-not-an-archive).

## dry run

`--dry-run` looks at each file's name in the mirror, as a push does, then says what it would write and what it would ask about. it writes nothing and asks nothing. a file the cache lacks counts as one it would copy from the data folder, as `Would copy 1 file · 21 B from data into the cache`, only when the push would take it. a file the project's state records with its pinned bytes, and that still looks as recorded, counts without being read, as sync trusts it; any other file there is read and hashed first, with nothing written. so a dry run in a copy whose `.fetchloom` came along reads no file of the data folder, one without it reads each such file once, and an edit of the pinned size, as one made through a hardlink `cp -a` kept, counts as missing in the dry run as in the push. a file it would download is named, as `Would fetch pets/pets.zip · 824 B`, and an archive it would unpack as `Would open pets/pets.zip`. the files of an archive this machine never unpacked are not counted, since only unpacking it tells them, and a dry run asks no mirror or server, so a file a mirror holds is named too. in the copy above:

```text
      Would copy 2 files · 3.9 KiB from data into the cache
      Would fetch pets/pets.zip · 824 B
      Would open pets/pets.zip
  hint: an archive's files are known once it is unpacked, so they are not counted
      Would start a mirror in /mnt/lab/fetchloom
      Would push 2 files · 3.9 KiB to /mnt/lab/fetchloom
         Ok dry run, nothing was changed
```

a dry run of a project whose cache holds every file:

```text
      Would start a mirror in /mnt/lab/fetchloom
      Would push 6 files · 106 B to /mnt/lab/fetchloom
      Would ask first: the sources of iris and pets did not say who may read them
         Ok dry run, nothing was changed
```

## JSON

`result` has `target`, the mirror's absolute folder, with any `..` in `--to` resolved as the system resolves it; `started`, whether the push started a mirror there; `selected`, the names given, or null; `datasets`, every dataset pushed; `objects` and `bytes`, the files those datasets need on the mirror, each once, and their bytes; `planned` and `planned_bytes`, the files of those the push set out to write, as the mirror lacked them or held a damaged copy; `copied` and `copied_bytes`, the files written; `present` and `present_bytes`, the files found there and left as they were; `replaced`, how many of `copied` took the place of a damaged copy; `trees`, the lists of archives' files written; and `asks`, a `{dataset, access}` record for each dataset that asks first. an error before anything is written, such as `offline.missing`, `network.refused`, `unconfirmed` or a stop, keeps `result`, with `copied`, `replaced` and `trees` at 0, since nothing was written, and what the push would have written in `planned`. a dry run counts what it would write in `copied` too and adds `dry_run: true` and `changes`, with `copy`, a `{path, sha256, size}` record for each file, `path` being the first `<dataset>/<file>` that needs it; `trees`, the same for each list of files, `path` naming its archive; `download`, a `{path, size}` record for each file it would download first, as `<dataset>/<path>` in data.lock; and `unpack`, the archives among them, which it would unpack.

`result` of `fetchloom --json push iris --dry-run` from a real run, the mirror's path shortened:

```json
{"target":"/mnt/lab/new","started":true,"selected":["iris"],"datasets":["iris"],"objects":1,"bytes":63,"planned":1,"planned_bytes":63,"copied":1,"copied_bytes":63,"present":0,"present_bytes":0,"replaced":0,"trees":0,"asks":[{"dataset":"iris","access":"unknown"}],"dry_run":true,"changes":{"copy":[{"path":"iris/iris.csv","sha256":"985aeca868a70414ab0d2404b7b9f9d7cc8ca5185a671a3cb401ccdf3bfff017","size":63}],"trees":[],"download":[],"unpack":[]}}
```

a push that stops at a file's read-back has `result` null and one error: `integrity.mismatch` when the file read back holds other bytes than were written, and `local.io` when something else replaced it. `errors[0]` of a real run where another program kept replacing the files push wrote, the mirror's path shortened:

```json
{"kind":"local.io","message":"something replaced many/d00/f00 in the mirror /mnt/lab/fetchloom while push wrote it, so it was not given its name","details":[],"help":"stop whatever else writes into that folder, such as a sync client, a backup tool or antivirus, then push again","fix":null}
```

the work counters count what push downloaded, as `requests`, `bytes_down` and `mirror_objects` do for a sync, and `data_folder_objects` and `data_folder_bytes` count the files it took from the data folder into the cache. [JSON output](../reference/json-output.md) has the envelope.

## exit codes

0 means every file the datasets need is on the mirror. 1 means nobody said yes to data that asks first, or data.lock is out of date; 2 a usage error, such as no folder or a web address; 20 that a file it needs could not be downloaded, as offline or in mirror-only mode, or that the cache lacks a file of a recipe's result, which only a build makes; 30 that a download or a pushed file read back holds other bytes than expected, or that the cache's copy holds other bytes than its pin; 50 that the folder is not a mirror, that something else replaced a file push wrote, that data.lock pins a file at a size its cached bytes do not have, or that the disk refused a write or is full; 60 that a recipe needs a build; 130 a stop. other failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| `holds files, but no fetchloom-mirror file says it is a mirror` | `--to` names a folder that is not a mirror, or a network disk that is not mounted | name an empty folder or a mirror's top folder, and mount the disk |
| `this machine's cache lacks files the push needs, which only a build makes` | a recipe's result was never built here, or `cache gc` freed it, and the data folder does not hold it | run the build the help names, then push again |
| `offline, and 1 file data.lock names is not in the cache` | push needs to download a file the cache and the data folder lack, and the run is offline | run the push again online, or with `FETCHLOOM_CACHE` naming a cache that holds the file |
| `data.lock pins small/small.csv at 13 bytes, but its SHA-256 is that of 12 bytes on this machine` | someone edited a file's size in data.lock by hand, or data.lock was damaged, and the cache holds the bytes of that SHA-256 at their real size. no download could bring other bytes | restore data.lock from git, or set that size back by hand, then push again |
| `... so pushing them needs a yes` | a source wanted an account or accepted terms, or did not say who may read the data | check that everyone who can read the mirror may have it, then answer yes or add `--yes` |
| `..., read back from ..., holds other bytes than were written, so it was not given its name` | the disk lost or changed a write, so the file read back is not what push wrote | check the disk that holds the mirror, then push again |
| `something replaced ... in the mirror ... while push wrote it, so it was not given its name` | another program writes into the mirror's folder, such as a sync client, a backup tool or antivirus, and put another file in place of one push wrote | stop that program, or keep it out of the folder, then push again |
| `push needs a folder to copy the files into` | neither `--to` nor `push_to` names one, or an empty `FETCHLOOM_PUSH_TO` cleared `push_to` | add `--to <folder>`, or set `push_to` in the settings file; for an empty `FETCHLOOM_PUSH_TO`, which the help names, add `--to <folder>` or unset it |
| `<archive>: not there on <mirror>` | a members table on the pushing machine left those files out, and push never writes an archive whose files it lists | widen the members table, run `fetchloom sync` and `fetchloom status --verify`, then push again |
| `holds a damaged copy of <file>, so it was not used` | a mirror file has the right size but other bytes, which push skips as already there | delete that file under the mirror's `objects/`, then push again |

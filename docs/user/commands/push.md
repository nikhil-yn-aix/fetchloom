# push

`push` copies the files `data.lock` pins from this machine's cache into a folder or S3 bucket [mirror](../guides/mirrors.md), downloading first what the cache lacks, so a teammate, a machine without a network or a reviewer can sync the project from there.

```sh
fetchloom push --to /mnt/lab/fetchloom
fetchloom push iris --dry-run
fetchloom push --to /media/usb/fetchloom --json
```

## usage

```sh
fetchloom push [options] [name]...
```

`name` is a dataset in `data.toml`; with none, every dataset is pushed. an unknown name is a usage error, exit 2, that suggests the closest name.

| option | effect |
|---|---|
| `--to <folder>` | the mirror's folder, or an S3 bucket below a prefix, as `s3://lab-data/fetchloom`. without it, the `push_to` setting names it, from the [settings file](../reference/environment.md#settings-file) or `FETCHLOOM_PUSH_TO`; with neither, push stops with `usage`, exit 2 |
| `--verify` | read back each copy already on a folder mirror and replace one whose bytes are not the pinned ones; without it a copy of the right size counts as there. a bucket refuses it, since S3 checked each file when push sent it |
| `-n`, `--dry-run` | say what would be downloaded, what would be copied and what is there already; change nothing |

[global options](global-options.md) also apply. `--yes` answers the [question](#data-you-may-not-share) about data whose source wanted an account or accepted terms.

## what it writes

push writes the [layout every mirror has](../guides/mirrors.md#a-folder-mirror): the `fetchloom-mirror` file, each file a dataset places under `objects/` by its SHA-256, and for each archive that unpacks, the list of its files under `trees/`. a reader takes an archive's files one by one, so push leaves the archive itself out, and every part of a split one, unless its list of files is longer than a reader takes, past about 600,000 files: then the archive, or each part, goes whole in place of its files, and push says so; an archive with `extract = false` is a file of its own and is pushed. a member a [members table](../reference/data-toml.md#files-inside-archives) leaves out is not placed, so it is not pushed either. a file several datasets need goes once. push writes nothing else, not data.toml, data.lock or code, and never changes the project.

from a real run at a terminal, a project with a csv file and a zip that unpacks to five files, with the mirror's path shortened:

```text
the sources of iris and pets did not say who may read them. anyone who can read /mnt/lab/fetchloom can take their files from there. push them anyway? [y/N] y
    Started a mirror in /mnt/lab/fetchloom
     Pushed 6 files · 106 B to /mnt/lab/fetchloom
         Ok every file iris and pets need is on the mirror
  next: teammates add mirrors = ["/mnt/lab/fetchloom"] to their settings file, here ~/.config/fetchloom/settings.toml; `fetchloom settings` shows where
```

each file is written read-only under a temporary name in the mirror's `tmp/` folder, in a push of 500 files or more in one of 16 folders below it by the first digit of the file's SHA-256, flushed to the disk, read back and hashed again, and only then given its name, never replacing a file already there. a file that reads back other bytes than were written stops push with `integrity.mismatch`, exit 30, and so does a copy in the cache that no longer holds its pinned bytes. a file another program put in place of one push wrote stops it with `local.io`, exit 50. a network disk may still answer the read-back from memory, but every reader checks each file against its pin again.

a push stopped with ctrl-c keeps the files that already have their names and removes its unfinished ones; a killed push leaves them under temporary names, which a later push removes once they are a day old. once every file has its name, a ctrl-c lets the push finish, as [interruptions](../concepts/questions-and-interruptions.md#interruptions) explains.

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

two pushes into one mirror at once need no lock. each holds a lock of its own in the mirror's `tmp/` while it writes, so a later push removes at once what a stopped one left, and never what one still writing wrote, however long it writes; on a disk that gives no locks, as lustre mounted with noflock, a push writes without one, and what a stopped push left goes once it is a day old. a link someone planted inside the mirror is refused, never written through, though the top folder itself may be a link. new folders take the permissions mask of the account that makes them, so a lab folder with the setgid bit and `umask 002` lets the whole group push, as a [shared cache](../guides/clusters.md#the-cache-on-a-shared-disk) does.

## where it writes

| the folder | what push does |
|---|---|
| a mirror, with its `fetchloom-mirror` file | adds what it lacks |
| new, in a folder that is there, or empty | starts a mirror there, then fills it |
| holding files but no `fetchloom-mirror` file | stops with `local.not_empty`, exit 50, and writes nothing: it may be anything, such as the empty folder a network disk is mounted on |
| not there, nor the folder above it | stops with `local.io`, exit 50, as when a disk is not mounted |
| a web address | stops with `usage`, exit 2 |

push writes to a folder, such as a lab nas, a network share or a usb drive, or to an [S3 bucket mirror](../guides/mirrors.md#an-s3-bucket-mirror), as `--to s3://lab-data/fetchloom`, signed with the [AWS keys](../reference/credentials.md#aws-keys) fetchloom finds. a web address is refused, so to fill a web mirror, push to a folder, then copy that folder to the web server with a tool such as rclone or rsync, which copy only what changed:

```sh
fetchloom push --to /srv/fetchloom
rsync -a /srv/fetchloom/ www.lab.example:/srv/www/fetchloom/
```

a FAT32 drive refuses a file larger than 4 GiB, so push stops at the first one with `local.io`; exFAT has no such limit.

## what it fetches

push copies what this machine's cache holds, and fills it first with what it lacks. a file the cache lacks that the data folder holds with its pinned bytes is copied into the cache, as in a project [moved to another machine](cache.md#moving-a-project-to-another-machine) with its data folder; the file in the data folder stays as it is. what neither holds, push downloads as a sync would: from a [mirror](../guides/mirrors.md) when one holds it, else from its source, each file checked against its pin. an archive this machine never unpacked is downloaded and unpacked to learn its files. stderr from a real run in a copy of a project with a csv file twice, as datasets iris and twin, and a zip, whose cache was empty, the mirror's path shortened:

```text
 Downloaded 1 file · 515 B in 6 ms
   Unpacked pets/pets.zip · 4 files · 33 B in 2 ms
     Copied 1 file · 104 B from data into the cache
    Started a mirror in /mnt/lab/fetchloom
     Pushed 5 files · 137 B to /mnt/lab/fetchloom
         Ok every file iris, pets and twin need is on the mirror
```

offline, push downloads nothing, and in [mirror-only mode](../guides/mirrors.md#mirror-only-mode) it asks no source, so a file it would have to download stops it before anything is written, with `offline.missing`, exit 20, or `network.refused`, exit 10, naming the files. offline, from the same copy:

```text
error: offline, and 1 file data.lock names is not in the cache
  pets/pets.zip
  help: run it again without --offline to download them, or set FETCHLOOM_CACHE to a cache folder that already holds them, or name a folder mirror that holds them with `--mirror <folder>`
```

a file in the data folder with other bytes than its pin counts as missing, so push downloads the pinned bytes instead and never changes the file.

push sends what data.lock pins, so a dataset `data.lock` does not pin as `data.toml` asks stops it with `lock.outdated`, exit 1, before anything is written: run `fetchloom lock`.

## data you may not share

`access` in [data.lock](../reference/data-lock.md) says what a dataset's source told about who may read it. a dataset whose source wanted an account (`login`) or accepted terms (`terms`), or did not say (`unknown`), is pushed only once you say yes, since anyone who can read the mirror can take its files without either. most sources say nothing, so most datasets ask. one data.lock calls `public` asks too when this machine keeps a key for the host of one of its files, or when one of your keys went with the download of one of its files, for this project or another, as when a link redirected to a host you sign in to. a dataset already on the mirror asks nothing. push asks before it downloads anything, so a no changes nothing.

```text
iris needs accepted terms. anyone who can read /mnt/lab/fetchloom can take its files from there. push it anyway? [y/N]
```

no keeps everything as it was. `--yes` answers yes. without a terminal, and with `--json`, push stops before writing anything with `unconfirmed`, exit 1, naming the datasets; its JSON `fix` is null, since only a person may agree:

```text
error: the sources of iris and pets did not say who may read them, so pushing them needs a yes
  help: answer yes at a terminal, or add --yes once everyone who can read /mnt/lab/fetchloom may have them
```

`access` is a hint from the source, never a license check. a yes does not give the right to share, and public data may still come with a license that limits copies: a [mirror is not an archive](../guides/mirrors.md#a-mirror-is-not-an-archive).

## dry run

`--dry-run` looks at each file's name in the mirror, as a push does, then says what it would write and what it would ask about. it writes nothing and asks nothing. a file it would take from the data folder is counted, as `Would copy 1 file · 21 B from data into the cache`, after hashing it when the project's state does not vouch for it. a file it would download is named, as `Would fetch pets/pets.zip · 824 B`, and an archive it would unpack as `Would open pets/pets.zip`, without its files, since only unpacking tells them. a dry run asks no mirror or server. in the copy above:

```text
      Would copy 2 files · 3.9 KiB from data into the cache
      Would fetch pets/pets.zip · 824 B
      Would open pets/pets.zip
  hint: an archive's files are known once it is unpacked, so they are not counted
      Would start a mirror in /mnt/lab/fetchloom
      Would push 2 files · 3.9 KiB to /mnt/lab/fetchloom
         Ok dry run, nothing was changed
```

a dataset that would ask first adds a line such as `Would ask first: the sources of iris and pets did not say who may read them`.

## JSON

`result` has `target`, the mirror's folder; `started`, whether the push started a mirror there; `datasets`, every dataset pushed; `planned`, the files it set out to write; `copied`, the files written; `present`, the files already there; `replaced`, the damaged copies it replaced; and `asks`, a `{dataset, access}` record for each dataset that asks first. an error before anything is written, such as `offline.missing`, `unconfirmed` or a stop, keeps `result`, with nothing copied and the plan in `planned`. a dry run adds `dry_run` and `changes`. [JSON output](../reference/json-output.md#command-results) gives every field.

`result` of `fetchloom --json push iris --dry-run` from a real run, the mirror's path shortened:

```json
{"target":"/mnt/lab/new","started":true,"selected":["iris"],"datasets":["iris"],"objects":1,"bytes":63,"planned":1,"planned_bytes":63,"copied":1,"copied_bytes":63,"present":0,"present_bytes":0,"replaced":0,"trees":0,"asks":[{"dataset":"iris","access":"unknown"}],"dry_run":true,"changes":{"copy":[{"path":"iris/iris.csv","sha256":"985aeca868a70414ab0d2404b7b9f9d7cc8ca5185a671a3cb401ccdf3bfff017","size":63}],"trees":[],"download":[],"unpack":[]}}
```

a push that stops at a file's read-back has `result` null and one error: `integrity.mismatch` when the file read back holds other bytes than were written, and `local.io` when something else replaced it. `errors[0]` of a real run where another program kept replacing the files push wrote, the mirror's path shortened:

```json
{"kind":"local.io","message":"something replaced many/d00/f00 in the mirror /mnt/lab/fetchloom while push wrote it, so it was not given its name","details":[],"help":"stop whatever else writes into that folder, such as a sync client, a backup tool or antivirus, then push again","fix":null}
```

the work counters count what push downloaded, as for a sync, and `data_folder_objects` and `data_folder_bytes` the files it took from the data folder into the cache.

## exit codes

| exit | when |
|---|---|
| 0 | every file the datasets need is on the mirror |
| 1 | nobody said yes to data that asks first, or data.lock is out of date |
| 2 | a usage error, such as no folder or a web address |
| 10 | in mirror-only mode, a file it needs is on no mirror |
| 20 | offline, a file it needs is not in the cache |
| 30 | a download or a pushed file read back holds other bytes than expected, or the cache's copy holds other bytes than its pin |
| 50 | the folder is not a mirror, something else replaced a file push wrote, data.lock pins a file at a size its cached bytes do not have, or the disk refused a write or is full |
| 130 | a stop |

other failures use the [shared exit codes](../reference/exit-codes.md).

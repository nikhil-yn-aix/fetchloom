# concepts

fetchloom turns the list of datasets in `data.toml` into pinned files that projects can reuse from one cache. to pin a dataset is to record its exact files and their SHA-256 in `data.lock`.

```sh
fetchloom add zenodo:3608990
fetchloom sync
```

| part | purpose |
|---|---|
| `data.toml` | name the datasets and selection rules you want |
| `data.lock` | record the exact files, their sizes and SHA-256 |
| the cache folder | keep each file once, for reuse across projects |
| `sync` | fill `data/` from `data.lock` |

## manifest and lock

`data.toml` describes intent. each dataset has a reference and can select files, exclude paths, keep archives packed, request writable copies, choose a destination or stay out of the data folder. see the [manifest reference](reference/data-toml.md).

`data.lock` records selected source files by SHA-256 and size, plus stated MD5 when available. source metadata and resolved versions make the origin inspectable. archive extraction adds a hash of the unpacked file tree.

files on your own machine can be pinned too: `fetchloom add ./flowers` copies them into the cache and pins them by their bytes, with `ref = "local"` and no path or link, and leaves the folder as it was. a teammate gets the same files from a copy, or by running the same `add` with `--as` where the files are; no source can download them again. see [files on this machine](commands/add.md#files-on-this-machine).

downloads must match their locked size and hashes. cached objects are not rehashed on every use: sealing dates each object by its birth, and one whose time has moved since, as an edit through a placed hardlink moves it, is hashed before it is used. file metadata and saved state avoid unnecessary reads; [cache verify](commands/cache.md#verify) checks cached bytes explicitly.

source checksum support covers MD5 and SHA-256. a known SHA-256 and size can allow a file to be locked without downloading it. hashes pin bytes after resolution; a source reference is not always immutable. see [lock rules](reference/data-lock.md#the-spec-hash).

`sync` updates missing or stale locks first unless `--locked` requires an up-to-date lock. unchanged entries retain their pins. `lock --check` checks manifest membership and specs without changing files, while `lock --upgrade` deliberately asks sources again.

## recipes

a dataset can also be made from other datasets by a command of yours. a [recipe](recipes.md) in `data.toml` names the datasets it reads with `from`, the command with `run`, the files it depends on with `code` and its variables with `env`. fetchloom keys the result by the content of all of these, so `status`, `lock --check` and `sync --locked` say when data.lock's pin of it no longer matches. no command but `fetchloom build` runs a recipe: it runs the command on private copies of what it reads, pins what it wrote in `data.lock` and places it like any dataset. `fetchloom build --dry-run` shows the commands first.

a dataset only recipes read can stay out of the data folder with `place = false`. it stays pinned in `data.lock`, and its files stay in the cache for the builds that read them, but no command places it, and the next sync removes what it placed of it before. once a folder mirror holds them, `fetchloom cache gc --inputs` frees the cache's copy too. [keep only the result](recipes.md#keep-only-the-result) shows it end to end.

## cache

the cache is local and content-addressed: objects are named by SHA-256. projects reuse the same bytes rather than downloading a separate copy for each project.

one user's processes can share it. on shared machines, give each user their own cache. a cache folder fetchloom creates itself on linux and macos is readable by you only. a group-writable cache is suitable only among trusted users: create its folder first with the group permissions you want, or set them with ACLs, and fetchloom keeps them; [one cache for a whole lab](hpc.md#one-cache-for-a-whole-lab) gives the commands, on a cluster or a shared desktop alike. fetchloom has no cross-user administration or group-permission layer. use `copy = true` for students or jobs that edit inputs.

successful project sync registers its root for garbage collection. `get` and standalone `lock` do not. [cache commands](commands/cache.md) covers retention, verification and cleanup; [cache folder](reference/cache-folder.md) covers locations and records.

## placement

| method | how files land | writable input? |
|---|---|---|
| reflink | clone file data using filesystem copy-on-write support | yes when `copy = true` |
| hardlink | share the cache object's file | no |
| copy | write independent bytes | yes when `copy = true` |

placement tries reflink, hardlink, then copy where supported. read-only placement on windows tries hardlinks before reflinks to reuse an already scanned file. it does not create symlinks. recognized synced folders skip hardlinks but may use reflinks.

files are read-only by default. for an analysis that writes into its inputs:

```toml
[datasets.iris]
ref = "zenodo:3608990"
copy = true
```

then run `fetchloom sync`. it replaces unchanged read-only files with writable reflinks or copies, never hardlinks. edited files stay. turning `copy` off leaves existing writable files as they are.

keep results of your own outside the data folder, or declare the step that makes them as a [recipe](recipes.md). a file you delete from the data folder comes back at the next sync, since data.lock still pins it. to leave a bad sample out for good, run [`fetchloom drop <path>`](commands/drop.md): it writes a pattern into data.toml that leaves the file out, so every clone leaves it out too. do not make a shared hardlink writable to edit it: every project using that link shares the change. sync treats such an edit as damage to the cache's copy: it downloads the file again and puts the pinned file back in each project still linked to the changed copy, keeping the changed copy beside it as `<path>.changed`. set `copy = true` on a dataset whose files you edit. [shared edits](commands/sync.md#local-changes) explains detection and repair, including how windows, whose folder listings can hide such a change, is checked. [working with raw data](raw-data.md) shows each of these cases from a real run.

sync preserves local conflicts unless forced. status differences and kept conflicts still exit 0. [scripting](scripting.md) shows how to inspect them.

## archives

extraction is enabled for `.zip`, `.7z`, `.tar`, `.tar.gz`, `.tar.xz`, `.tar.zst`, `.tar.bz2`, `.tgz`, `.txz`, `.tzst`, `.tbz` and `.tbz2`, ignoring case. the bytes choose the decoder.

other names stay intact, including `.fastq.gz`, `.xlsx`, `.npz`, `.pt` and extensionless downloads. `.rar` and recognized names containing unsupported bytes stay packed with a warning. a `.rar`'s warning names a command for your system that unpacks it into `<name>-unpacked`, outside the data folder, so `status` lists none of its files: unrar on linux, unar on macos, 7-Zip on windows. [compatibility](reference/compatibility.md#archives) shows each one. unsafe, damaged or encrypted supported archives fail extraction, as does a 7z compressed with a method fetchloom does not read, such as ppmd. nested archives are not unpacked recursively. zip directories are checked before their entries are collected: at most 1,000,000 entries, 4,096-byte paths and 256 MiB of directory metadata work. a refused archive can still be kept packed.

use `--no-extract` with `add` or `get`, or `extract = false` in an existing manifest entry, to keep packed bytes. select and exclude patterns match the listed archive, not the files inside it; a [members table](reference/data-toml.md#files-inside-archives) keeps or leaves out those, matching each by its path inside the archive.

paths below are relative to the dataset or `get` folder:

| selected files | members in `train.zip` | placed paths |
|---|---|---|
| only `train.zip` | `train/a.csv`, `train/b.csv` | `a.csv`, `b.csv` |
| only `raw/train.zip` | `train/a.csv` | `raw/a.csv` |
| `train.zip` and another file | `train/a.csv`, `train/b.csv` | `train/a.csv`, `train/b.csv` |
| `train.zip` and another file | only `a.csv` | `a.csv` |
| `train.zip` and another file | `a.csv`, `b.csv` | `train/a.csv`, `train/b.csv` |

a sole archive loses one shared top folder. beside other selected files, a shared top folder or lone top-level file keeps its name; otherwise members go under the archive's stem. the archive's parent path is retained. colliding destinations fail, a member landing on another dataset's file too; sync finds that once the archive is unpacked, before it places anything.

an archive with no regular files creates no dataset folder in a project. `get` still creates an empty output folder. members enter the cache before placement. packed bytes can then be removed unless a registered project needs them packed.

[tree hashes](reference/data-lock.md#the-tree-hash) records archive member paths before placement strips a folder. empty directories are not placed.

### split archives

a big dataset may come as one archive cut into parts, such as `scans.zip.001` to `scans.zip.003` on zenodo, or `train.tar.gz.aa` and `.ab` in a hugging face repository. `split` and 7-Zip cut an archive's bytes, so its parts read one after another are the archive. when a dataset lists every part, fetchloom unpacks them as that one archive, reading part after part where the cache keeps them, and places the files as it would place the whole archive's; the parts themselves are not placed. a dataset that is one split archive gets the files straight in its folder, and one beside other files puts them under the name before the counter, `scans/` for `scans.zip.001`. data.lock pins each part as a file and gives the first part `unpacks_to`, as [split archives in data.lock](reference/data-lock.md#split-archives) describes, and the cache drops the parts once the files are in. a [members table](reference/data-toml.md#files-inside-archives) and `fetchloom drop` name the archive by its first part, as in `[datasets.scans.members."scans.zip.001"]`.

parts belong together only inside one dataset and one folder, after the name of an archive fetchloom unpacks, so `weights.bin.001` stays a file. their counters run as `split` and 7-Zip write them, letters from `aa` or numbers from `00`, `000` or `001`, and `.part`, `.part-` or `.part_` may come before a counter. a counter after a bare `.` has at least two characters, so a copy named `data.tar.gz.1` stays a file. stderr from `fetchloom add ../zipped`, a folder holding `headers.zip.00` to `.02` that `split -b 256K -d` made of a zip of 300 files:

```text
   Imported zipped · 3 files · 748.6 KiB, copied into the cache; ../zipped is unchanged
   Unpacked zipped/headers.zip.00 · 300 files · 2.2 MiB in 522 ms
```

what stays packed is told once, with what to do. stderr from `fetchloom add` of a folder missing `headers.zip.01`, of a folder of rar volumes and a zip spanned over `.z01` files, and of `add -i` with one link for each part:

```text
warning: gap/headers.zip.01 is missing, so the parts of headers.zip stay as they are
  help: fetchloom joins a split archive only from every one of its parts: check that select and exclude keep them all, or ask the dataset's authors for the missing one
warning: volumes/clips.part1.rar stays packed with 7 other volumes of its rar, since fetchloom does not unpack rar
  help: unpack it with unrar from your package manager: `unrar x data/volumes/clips.part1.rar clips.part1-unpacked/`. extract = false stops this warning
warning: volumes/maps.zip stays packed with 1 other file it spans, since fetchloom does not unpack a zip spanned over several files
  help: join its parts with zip from your package manager, then unpack them: `zip -s 0 data/volumes/maps.zip --out maps-joined.zip`, then `unzip maps-joined.zip -d maps-unpacked`. extract = false stops this warning
warning: headers-zip/headers.zip.00 stays packed, since it does not unpack on its own
  help: it may be the first part of an archive split into several files, which fetchloom joins only inside one dataset, such as a folder or a record that lists every part
```

the parts of an archive missing one are placed as plain files. rar volumes and spanned zips are no cut of the bytes, so only their own tools join them: a rar's help is the one for a single rar, given its first volume, from which unrar and 7-Zip read the others, and a spanned zip's joins its parts with zip and unpacks them with unzip, or on windows runs 7-Zip, which reads them itself. a first part alone, as `add -i` makes each link a dataset of its own, is unpacked when its bytes are a whole archive, and otherwise stays packed with the last warning above. `extract = false` and `--no-extract` keep the parts packed, and switching `extract` back on unpacks them from the cache with no request.

### webdataset shards

webdataset splits a large dataset into numbered tar files, shards such as `train-000000.tar` to `train-001023.tar`, and its loaders read them packed. unpacked, each shard becomes its many small files: while a sync runs, the cache holds both the shard and its files, and afterwards every file takes an inode, which a cluster's quota counts. so before anything downloads, `add`, `get` and a `sync` that adds or changes a dataset hint when the files that would unpack include at least 8 tars in one folder named `<prefix><digits>.tar`, `.tar.gz` or `.tgz`, with one prefix and one digit width. stderr from `fetchloom add s3://wds/shards/` with a local stand-in for the bucket:

```text
   Resolved s3://wds/shards/ · 1,024 files · 10.0 MiB
  hint: shards holds 1,024 numbered .tar shards, which webdataset loaders read packed. keep them packed with --no-extract, or extract = false in data.toml
 Downloaded 1,024 files · 10.0 MiB in 1.6 s · 6.2 MiB/s
```

`sync` names only `extract = false in data.toml`, and `get` only `--no-extract`, the way each keeps archives packed. the hint shows when the dataset is new or a change to its entry lists the source again; a later sync, an upgrade and a switch of `extract` show none, and neither do `-q`, `--json` or `FETCHLOOM_NO_HINTS=1`. turning `extract` off later needs no new listing, but the shards come down again, since sync drops an archive's own bytes once its files are in; [data.lock](reference/data-lock.md#the-spec-hash) explains the switch.

## offline

```sh
fetchloom sync --offline
fetchloom --offline path iris
```

offline mode prevents fetchloom from making HTTP or DNS requests. a locked project can use unchanged placed files, cached objects and unpack records.

search and entries needing resolution fail offline, even when you already know a source id or plain URL. repository listings are not cached for offline resolution. offline `get` of a direct URL needs its URL record and cached bytes, or with `-o`, a file at the path that still holds what the link served last time. an unpinned new link reports "offline, and a new link must be downloaded once to learn its SHA-256". the help names the flag or environment setting to change, followed by "to pin it. `fetchloom get <link> --offline` reuses a file fetched before".

prepare the project online before disconnecting and keep its manifest and lock unchanged. `FETCHLOOM_OFFLINE=1` also enables offline mode. to reconnect, remove `--offline` and unset that variable if it is `1`. offline, a folder [mirror](mirrors.md), such as a usb drive, still gives the pinned files the cache lacks, and so does the data folder, where it holds them at the paths data.lock places them at.

## recovery

a [mirror](mirrors.md) you name is asked for a pinned file before its source, so a file whose source is gone can still come from a lab's copy. if a locked download returns 404 or 410, fetchloom asks its recorded source for a new URL and checks the new download against the original size and hashes. it does not rewrite the lock. an unchanged URL or another kind of failure cannot recover this way.

successful downloads stay cached when another file fails. retries can reuse saved partial bytes when hashes or server validators allow it. unsupported ranges, changed validators or damaged saved bytes can require a restart.

transfers retry transient failures, 4 more times by default or as `FETCHLOOM_RETRIES` sets, and respect announced rate limits within bounded waits. several files download at once. eligible files of at least 64 MiB can split into parallel range requests after one stream stays below 1 MB/s over 2 seconds, with at least 16 MiB left. a configured rate cap turns off that adaptive split; `FETCHLOOM_SPLIT=1` and a resumed download whose later ranges are kept still split. [environment settings](reference/environment.md) controls network work and rate limits.

## approval

```sh
fetchloom sync --yes
```

approval is required above 1 GiB, not at exactly 1 GiB. `FETCHLOOM_ASK_ABOVE`, or `ask_above` in the [settings file](reference/environment.md#settings-file), moves that line, as `FETCHLOOM_ASK_ABOVE=50G` on a machine meant for big data.

| download | bytes checked |
|---|---|
| `get` | known bytes still needing download; cached checksummed files and unpacked archives do not count |
| project commands | known bytes still missing from the cache |
| unknown source size | the server's length against the remaining download, or received bytes after a file passes that size |
| known source size | counted up front, as in the rows above; an answer stating another length is refused before download, and a body without a stated length stops once it passes the known size, so a larger download never slips past the approval |

unknown sizes are checked per file, with at most one such question per batch. offline runs never ask for download approval.

the disk holding the cache is checked too. when what a run is about to download needs more than that disk has free, counting each archive whose files are not known yet once more for unpacking, fetchloom asks before anything starts, or stops with `unconfirmed` and both numbers when no one can answer. a sync asks once for all its downloads, so parts that fit alone but not together are caught. `--yes` goes on anyway, as a disk that compresses what it stores can hold more than it tells. a file of unknown size is checked once its server states the length.

`--yes` or `-y` answers yes to every question a person could be asked, never a [dataset search choice](commands/search.md#choosing-a-dataset). a question shows its default in capitals, as `[y/N]`: downloads and a project in your home folder default to no, saving a key you typed to yes. questions require interactive stdin and stderr. answer with y or yes, or n or no, then enter; enter alone takes the default, and any other answer, such as q, asks the question again. pipes, CI, [coding agents](ai-agents.md) and JSON mode cannot answer; an unapproved download fails with exit 1 and `unconfirmed`. a run asks before it places any file, so a no at the question says that nothing changed. the exception is a `sync` that has already written `data.lock`, which it does before it downloads what placing needs: that change stays, and the `Cancelled` line does not say nothing changed.

## interruptions

at a download question, a sign-in prompt or the dataset picker, ctrl-c cancels the question and stops with exit 130, and the terminal shows what you type again. a cancel you choose is not an error: fetchloom prints one `Cancelled` line saying what was cancelled, such as `Cancelled the download of huge.bin (2.0 GiB), so nothing changed`, instead of `error:`. during other work, the first ctrl-c requests a stop, prints `Stopping (press ctrl-c again to quit at once)`, and preserves resumable bytes before exiting 130. a stop you ask for is not an error either: the run ends with one `Stopped` line saying where it stopped, such as `Stopped while talking to 127.0.0.1`, and a dim line under it says how much the unfinished downloads keep, together, which is what the next run goes on from. cache scans check between entries, and verification checks between hash reads. a second ctrl-c clears the progress bars and exits immediately with code 130. once a command's changes are in place, as when `add`, `sync`, `lock` or `remove` prints its last lines, a ctrl-c no longer stops it, and it exits 0.

## deleting a dataset from a machine

a data-use agreement can require deleting a dataset's bytes from a machine. fetchloom keeps them in more than one place, and frees only the ones it manages:

1. in each project that uses the dataset, run `fetchloom remove <name>`, after `fetchloom remove <name> --dry-run` to see what goes. it keeps files you edited and other files in the dataset's folder, such as the `<path>.changed` copies sync kept, and names them in warnings: delete those yourself. a data folder that `data_dir` no longer names keeps all its files, which sync named once when `data_dir` changed: delete that folder yourself too. deleting a whole project folder works too.
2. delete the folders `fetchloom get` wrote; no project records them.
3. run `fetchloom cache gc --now`, after `fetchloom cache gc --now --dry-run` to see what goes. it deletes the cached files no project needs, including what a deleted project folder alone needed, and every unfinished download no running fetchloom holds. temporary files under the cache's `tmp/` folder keep their one-day grace period even then.
4. repeat this on each machine and cache that fetched the dataset, such as other computers, CI caches, container volumes and a lab's shared cache.

a cached file stays while another project using the same cache pins the same bytes, under any dataset name, since projects share files by SHA-256. `fetchloom cache` lists the projects that use the cache.

fetchloom cannot reach copies it did not make: backups, a mirror or shared drive you copied files to, a synced folder's trash or version history, and copies other programs made. it deletes a file the way `rm` does, without overwriting it, so snapshots, copy-on-write clones or the disk itself can keep the bytes, and fetchloom does not verify erasure. when an agreement requires that, follow your institution's procedure for the disk. the git history of `data.toml` and `data.lock` keeps the dataset's reference, file names, sizes and SHA-256, not its bytes.

## what leaves your machine

fetchloom sends nothing on its own: it has no telemetry, no account and no update check. a request goes out only for work a command does:

| what | goes to |
|---|---|
| search words, given to `search`, or to `add` or `get` in place of a link | the search of hugging face, kaggle and zenodo, or of the sites `--source` names |
| a reference | the site it names: a DOI to doi.org, which says where it points, an id such as `zenodo:3608990` to that site, a link to its host |
| a download | the file's host, or a [mirror](mirrors.md#privacy) you named, which is asked by SHA-256 alone |
| a key | its own host only, over https, as [auth](commands/auth.md) describes |

each request names fetchloom and its version, as `fetchloom/0.1.0`, in its `User-Agent` header. a path on this machine never leaves it: `add` copies the files a path names into the cache with no request, `get` and `search` refuse a path before any request, and data.lock records no path for an import. what your repository shares is `data.toml` and `data.lock`: each dataset's reference, links, file names, sizes and SHA-256. a `-vv` log names local paths and every link fetchloom asked for, so read it before you share it.

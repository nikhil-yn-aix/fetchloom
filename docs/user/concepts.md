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

`data.toml` says what you want. each dataset has a reference and can select files, exclude paths, keep archives packed, ask for writable copies, choose a folder or stay out of the data folder. see the [manifest reference](reference/data-toml.md).

`data.lock` records what you got: each selected file's SHA-256 and size, plus the MD5 a source states, the version the reference resolved to, and for an archive, a hash of its unpacked file tree. see the [lock reference](reference/data-lock.md).

files on your own machine can be pinned too: `fetchloom add ./flowers` copies them into the cache and pins them by their bytes, and leaves the folder as it was. see [files on this machine](commands/add.md#files-on-this-machine).

every download must match its locked size and hashes. a source that states a SHA-256 and size lets a file be locked without downloading it. what a reference points at can change, as a plain link's file can, but the lock pins bytes: a source that serves other bytes fails the check. see [the spec hash](reference/data-lock.md#the-spec-hash).

cached files are not hashed again on every use. a cached file written after it entered the cache, as an edit through a placed hardlink writes it, is hashed before it is used, and [cache verify](commands/cache.md#verify) hashes everything when you want to be sure.

`sync` first updates a missing or stale lock, unless `--locked` asks for an up-to-date one. unchanged entries keep their pins. `lock --check` checks the lock against data.toml without changing files, and `lock --upgrade` asks the sources for newer versions.

## recipes

a dataset can also be made from other datasets by a command of yours. a [recipe](recipes.md) in `data.toml` names the datasets it reads with `from`, the command with `run`, the files it depends on with `code` and its variables with `env`. fetchloom keys the result by the content of all of these, so `status`, `lock --check` and `sync --locked` say when the pinned result no longer matches. only `fetchloom build` runs a recipe: it runs the command on private copies of what it reads, pins what it wrote in `data.lock` and places it like any dataset. `fetchloom build --dry-run` shows the commands first.

a dataset only recipes read can stay out of the data folder with `place = false`: it stays pinned and cached for the builds that read it, and the next sync removes what it placed of it before. once a folder mirror holds its files, `fetchloom cache gc --inputs` frees the cache's copy too. [keep only the result](recipes.md#keep-only-the-result) shows it end to end.

## cache

the cache is one folder on this machine that names each file by its SHA-256, so projects share the same bytes instead of downloading a copy each. [cache folder](reference/cache-folder.md) says where it is and what it holds.

one user's processes can share a cache. on a shared machine, give each user their own: a cache folder fetchloom creates on linux and macos is readable by you only. a cache shared by a group suits only trusted users; create its folder first with the group permissions you want and fetchloom keeps them, as [one cache for a whole lab](hpc.md#one-cache-for-a-whole-lab) shows. use `copy = true` for students or jobs that edit inputs.

a sync registers its project with the cache, so `cache gc` keeps what the project needs. `get` and `lock` register nothing. [cache commands](commands/cache.md) cover retention, verification and cleanup.

## placement

| method | how files land | writable? |
|---|---|---|
| reflink | a clone that shares the cache file's blocks until either is written | yes, with `copy = true` |
| hardlink | another name for the cache file itself | no |
| copy | independent bytes | yes, with `copy = true` |

sync tries a reflink, then a hardlink, then a copy, as the disk allows. it never makes symlinks. a folder fetchloom recognizes as synced, such as onedrive, dropbox or icloud drive, gets no hardlinks, but may get reflinks; [windows](windows.md#onedrive-and-synced-folders) and [macos](macos.md#icloud-drive-and-cloud-storage) list the folders.

files are read-only by default. for an analysis that writes into its inputs:

```toml
[datasets.iris]
ref = "zenodo:3608990"
copy = true
```

then run `fetchloom sync`. it replaces unchanged read-only files with writable reflinks or copies, never hardlinks, and keeps files you edited. turning `copy` off leaves writable files as they are.

keep results of your own outside the data folder, or make them with a [recipe](recipes.md). a file you delete from the data folder comes back at the next sync, since data.lock still pins it; to leave a bad sample out for good, [`fetchloom drop <path>`](commands/drop.md) writes a pattern into data.toml that every clone follows. do not make a hardlinked file writable to edit it: the edit changes the cache's copy, which every project linked to it shares. sync treats that as damage, as [local changes](commands/sync.md#local-changes) explains. [working with raw data](raw-data.md) shows each of these cases from a real run.

sync keeps local edits unless forced, and status differences and kept conflicts still exit 0. [scripting](scripting.md) shows how to check for them.

## archives

fetchloom unpacks `.zip`, `.7z`, `.tar`, `.tar.gz`, `.tar.xz`, `.tar.zst`, `.tar.bz2`, `.tgz`, `.txz`, `.tzst`, `.tbz` and `.tbz2`, ignoring case; the bytes choose the decoder. other files stay as they are, including `.fastq.gz`, `.xlsx`, `.npz` and `.pt`.

a `.rar` stays packed with a warning, from the add that pins it and once a run from each sync that places it, as a teammate's first sync does. the warning names a command for your system that unpacks it beside itself in the data folder, where sync keeps the files and `status` lists them as extra; [compatibility](reference/compatibility.md#archives) shows each. an archive inside another also stays packed, and the run that places it says so once, with a command that unpacks it.

an unsafe, damaged or encrypted archive fails to unpack, as does a 7z compressed with a method fetchloom does not read, such as ppmd. unsafe means a member that would land outside its folder, a symlink, a device or a fifo, or a tar hardlink to anything but a file before it. an archive past the [limits](limits.md) on files, size or compression ratio is refused too. a refused archive can still be kept packed.

use `--no-extract` with `add` or `get`, or `extract = false` in data.toml, to keep archives packed. select and exclude patterns match the files a source lists, so an archive, not the files inside it; a [members table](reference/data-toml.md#files-inside-archives) chooses those by their paths inside the archive.

where the files land, relative to the dataset or `get` folder:

| selected files | members in `train.zip` | placed paths |
|---|---|---|
| only `train.zip` | `train/a.csv`, `train/b.csv` | `a.csv`, `b.csv` |
| only `raw/train.zip` | `train/a.csv` | `raw/a.csv` |
| `train.zip` and another file | `train/a.csv`, `train/b.csv` | `train/a.csv`, `train/b.csv` |
| `train.zip` and another file | only `a.csv` | `a.csv` |
| `train.zip` and another file | `a.csv`, `b.csv` | `train/a.csv`, `train/b.csv` |

a sole archive loses one top folder its members share. beside other selected files, a shared top folder or a lone top-level file keeps its name; otherwise members go under the archive's name without its extension. an archive in a folder unpacks into that folder. files a system writes by itself, such as finder's `.DS_Store` and `__MACOSX` or windows' `Thumbs.db`, are left out, as a folder import leaves them out, and one `Skipped` line counts them, as in `Skipped recordings/.DS_Store and 2 more the system makes in finder/finder.zip`. two members, or a member and another dataset's file, that land on one path fail before anything is placed. empty folders are not placed, and an archive with no files makes no dataset folder.

members enter the cache before they are placed, and the archive's own bytes then leave it, unless a project needs them packed. [the tree hash](reference/data-lock.md#the-tree-hash) records member paths before placement drops a top folder.

### split archives

a big dataset may come as one archive cut into parts, such as `scans.zip.001` to `scans.zip.003` on zenodo, or `train.tar.gz.aa` and `.ab` in a hugging face repository. `split` and 7-Zip cut an archive's bytes, so the parts read one after another are the archive. when a dataset lists every part, fetchloom unpacks them as that one archive and places its files; the parts themselves are not placed. a dataset that is one split archive gets the files straight in its folder, and one beside other files puts them under the name before the counter, `scans/` for `scans.zip.001`. data.lock pins each part as a file, as [split archives in data.lock](reference/data-lock.md#split-archives) describes, and a [members table](reference/data-toml.md#files-inside-archives) or `fetchloom drop` names the archive by its first part, as in `[datasets.scans.members."scans.zip.001"]`.

parts belong together only inside one dataset and one folder, after the name of an archive fetchloom unpacks, so `weights.bin.001` stays a file. their counters run as `split` and 7-Zip write them: letters from `aa`, or numbers from `00`, `000` or `001`, with `.part`, `.part-` or `.part_` allowed before the counter. a counter after a bare `.` has at least two characters, so a copy named `data.tar.gz.1` stays a file. stderr from `fetchloom add ../zipped`, a folder holding `headers.zip.00` to `.02` that `split -b 256K -d` made of a zip of 300 files:

```text
   Imported zipped · 3 files · 748.6 KiB, copied into the cache; ../zipped is unchanged
   Unpacked zipped/headers.zip.00 · 300 files · 2.2 MiB in 522 ms
```

what stays packed is told once, with what to do. stderr from `fetchloom add` of a folder missing `headers.zip.01`, of a folder of rar volumes and a zip spanned over `.z01` files, and of `add -i` with one link for each part:

```text
warning: gap/headers.zip.01 is missing, so the parts of headers.zip stay as they are
  help: fetchloom joins a split archive only from every one of its parts: check that select and exclude keep them all, or ask the dataset's authors for the missing one
warning: volumes/clips.part1.rar stays packed with 7 other volumes of its rar, since fetchloom does not unpack rar
  help: unpack it with unrar from your package manager: `unrar x data/volumes/clips.part1.rar data/volumes/clips.part1-unpacked/`. extract = false stops this warning
warning: volumes/maps.zip stays packed with 1 other file it spans, since fetchloom does not unpack a zip spanned over several files
  help: join its parts with zip from your package manager, then unpack them: `zip -s 0 data/volumes/maps.zip --out data/volumes/maps-joined.zip`, then `unzip data/volumes/maps-joined.zip -d data/volumes/maps-unpacked`. extract = false stops this warning
warning: headers-zip/headers.zip.00 stays packed, since it does not unpack on its own
  help: it may be the first part of an archive split into several files, which fetchloom joins only inside one dataset, such as a folder or a record that lists every part
```

the parts of an archive missing one are placed as plain files. rar volumes and spanned zips are not cuts of the bytes, so only their own tools join them. a first part alone, as `add -i` makes each link a dataset of its own, is unpacked when its bytes are a whole archive, and otherwise stays packed with the last warning above. `extract = false` and `--no-extract` keep the parts packed, and turning `extract` back on unpacks them from the cache with no request.

### webdataset shards

webdataset splits a large dataset into numbered tar files, shards such as `train-000000.tar` to `train-001023.tar`, and its loaders read them packed. unpacked, each shard becomes its many small files, and each file takes an inode, which a cluster's quota counts. so before anything downloads, `add`, `get` and a `sync` that adds or changes a dataset print a hint when the files to unpack include at least 8 tars in one folder named `<prefix><digits>.tar`, `.tar.gz` or `.tgz`, with one prefix and one digit width. in a project, the hint names the one [members table](reference/data-toml.md#one-table-for-many-shards) that keeps only some files of every shard. stderr from `fetchloom add s3://wds/shards/` with a local stand-in for the bucket, on a debug build:

```text
   Resolved s3://wds/shards/ · 1,024 files · 10.0 MiB
  hint: shards holds 1,024 numbered .tar shards, which webdataset loaders read packed. keep them packed with --no-extract, or extract = false in data.toml. to unpack them and keep only some files of each, write one members table, [datasets.shards.members."train-*.tar"]
 Downloaded 1,024 files · 10.0 MiB in 15.5 s · 659.5 KiB/s
```

`sync` names only `extract = false in data.toml`, and `get` only `--no-extract`, which keeps no project and so names no members table. the hint shows when a dataset is new, or a change to its entry lists the source again, and not for a dataset a members table already covers, nor with `-q`, `--json` or `FETCHLOOM_NO_HINTS=1`. turning `extract` off later needs no new listing, but the shards download again, since sync drops an archive's own bytes once its files are in.

## offline

```sh
fetchloom sync --offline
fetchloom --offline path iris
```

offline, fetchloom makes no HTTP or DNS requests. a locked project whose files are cached or already placed works as usual.

whatever needs a source fails offline: search, and an entry the lock does not pin yet, even when you know its id or link, since listings are not cached. a link not pinned yet fails with `offline, and a new link must be downloaded once to learn its SHA-256`, unless a `get` or an `add` on this machine kept its file, which `add` then pins from the cache, with `--as` or without. `get` of a link works offline once a `get` on this machine kept its file, and with `-o`, once the file at that path still holds what the link served.

prepare the project online and keep `data.toml` and `data.lock` unchanged before you disconnect. `FETCHLOOM_OFFLINE=1` or `true` turns offline mode on too, and a value fetchloom cannot read, such as `yes`, stops a command before any request. offline, a folder [mirror](mirrors.md), such as a usb drive, still gives the pinned files the cache lacks, and so do the files already at their places in the data folder.

## recovery

a [mirror](mirrors.md) you name is asked for a pinned file before its source, so a file whose source is gone can still come from a lab's copy. if a locked download returns 404 or 410, fetchloom asks the recorded source for a new link and checks the new download against the pinned size and hashes, without rewriting the lock. when no source can give the file again, as for a plain link that died, the error's help says how to get the pinned bytes from someone who has them: they run `fetchloom push --to <folder>`, then you run `fetchloom sync --mirror <folder>`.

when one file fails, the files that downloaded stay cached. a retry continues from the bytes it kept when hashes or server validators show they belong to the same file; otherwise it starts the file over.

a failed transfer is tried 4 more times, or as many as `FETCHLOOM_RETRIES` sets, and a server's announced rate limit is respected within bounded waits. while a download waits, its bar says which try comes next and when, as in `try 3 of 5 · next in 2 s`. several files download at once, and a file of at least 64 MiB whose one stream stays below 1 MB/s for 2 seconds, with at least 16 MiB left, splits into parallel range requests. [environment](reference/environment.md) lists the settings for network work and rate limits.

## approval

```sh
fetchloom sync --yes
```

a download of more than 1 GiB asks first. `FETCHLOOM_ASK_ABOVE`, or `ask_above` in the [settings file](reference/environment.md#settings-file), moves that line, as `FETCHLOOM_ASK_ABOVE=50G` on a machine meant for big data.

| download | bytes counted |
|---|---|
| `get` | known bytes still to download; cached files with checksums and unpacked archives do not count |
| project commands | known bytes still missing from the cache |
| a file of unknown size | the length its server states, or the bytes received once they pass the line |
| a file of known size | counted up front; a server stating another length is refused before the download, and a body without a stated length stops once it passes the known size |

a batch asks at most one question about files of unknown size. offline runs never ask.

fetchloom also checks the free space on the cache's disk. when a run needs more than is free, counting each archive whose files are not known yet once more for unpacking, it asks before anything starts, naming the cache folder and both numbers. a sync asks once for all its downloads. `--yes` goes on anyway, since a disk that compresses what it stores can hold more than it says. a file whose server never states its length is checked as it arrives, and stops before it fills the disk.

`--yes` or `-y` answers yes to every question, but never picks a dataset from a [search](commands/search.md#choosing-a-dataset). a question shows its default in capitals, as `[y/N]`: downloads, a project in your home folder and the offer to sign in after a refusal default to no, and saving a key you typed defaults to yes. answer y, yes, n or no and press enter; enter alone takes the default, and any other answer asks again. questions need a terminal on stdin and stderr, so pipes, CI, [coding agents](ai-agents.md) and `--json` cannot answer them: an unapproved download fails with exit 1 and `unconfirmed`. a run asks before it places any file, so a no changes nothing, except in a `sync` that had already written `data.lock`, which stays.

## interruptions

at a question, a sign-in prompt or the dataset picker, ctrl-c cancels and exits 130. a cancel you chose is not an error, so one `Cancelled` line says what was cancelled, such as `Cancelled the download of huge.bin (2.0 GiB), so nothing changed`.

during other work, the first ctrl-c asks for a stop. fetchloom prints `Stopping (press ctrl-c again to quit at once)`, keeps what it downloaded, and ends with one `Stopped` line, such as `Stopped while talking to 127.0.0.1`, and exit 130. a dim line under it says how much the unfinished downloads keep and whether the next run continues from there, which needs a server that sends parts of a file. a later run that cannot continue says so once, as `Restarting big.bin · the 2.0 MiB an earlier run kept cannot be continued`, and starts that file over.

two moments finish instead, since stopping there would throw work away. once every byte is downloaded and only the disk is still writing, a ctrl-c prints `Finishing (the download is complete; ctrl-c again to quit at once)`. once a command is writing its changes or its last lines, a ctrl-c prints `Finishing what it started (ctrl-c again to quit at once)`, and the command ends with its usual closing line, such as `Added iris`, and exit 0.

a second ctrl-c prints `Quit at once` and exits 130 without waiting for the work to wind down. a first `add` quit this way before it wrote anything leaves its folder as it was. on a slow disk, the process can outlast the `Quit` line until the operating system finishes flushing a file.

on linux and macos, SIGTERM, which `kill`, a container's stop and a scheduler such as slurm send, does what a first ctrl-c does in every command, worded for it: `Stopping (a second SIGTERM quits at once)`, then a `Stopped` line that names it, such as `Stopped by a SIGTERM while talking to zenodo.org, so nothing changed`, and exit 130. with `--json`, the run still prints its one document, with an `interrupted` error. a second SIGTERM, or a ctrl-c after it, quits at once.

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
| search words, given to `search`, or to `add` or `get` in place of a link | the search of hugging face, kaggle, zenodo, ENA, GEO, dandi and openneuro, or of the sites `--source` names |
| a reference | the site it names: a DOI to doi.org, which says where it points, an id such as `zenodo:3608990` to that site, a link to its host |
| a download | the file's host, or a [mirror](mirrors.md#privacy) you named, which is asked by SHA-256 alone |
| a key | its own host only, over https, as [where a key goes](signing-in.md#where-a-key-goes) says |

each request names fetchloom and its version, as `fetchloom/0.1.0`, in its `User-Agent` header. a path on this machine never leaves it: `add` copies the files a path names into the cache with no request, `get` and `search` refuse a path before any request, and data.lock records no path for an import. what your repository shares is `data.toml` and `data.lock`: each dataset's reference, links, file names, sizes and SHA-256. a `-vv` log names local paths and every link fetchloom asked for, so read it before you share it.

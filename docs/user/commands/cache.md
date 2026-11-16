# cache

`cache` says where the shared cache is and what fills it, frees what no project needs, and checks the cached bytes.

```sh
fetchloom cache
fetchloom cache gc
fetchloom cache verify
```

## usage

```sh
fetchloom cache [options] [command]
fetchloom cache gc [options]
fetchloom cache verify [options]
```

`cache gc --now` also frees unneeded objects and unpack records less than a day old. `cache gc --dry-run`, or `-n`, shows what gc would free and deletes nothing. [global options](global-options.md) apply to all three forms.

`fetchloom cache` makes no cache where there is none, and neither do gc and verify, in any form. before the first command that needs a cache makes one in the default folder, they say so, as on linux `Ok there is no cache at ~/.cache/fetchloom yet, so there is nothing to free`, exit 0. an empty folder `FETCHLOOM_CACHE` or the `cache` setting names, as a lab makes for a [shared cache](../guides/clusters.md#one-cache-for-a-whole-lab) before anyone syncs, gets the same answer. one that is missing, as a mistyped one is, stops them with `local.io`, exit 50, and the help says where the folder was set. stderr of `fetchloom cache gc` with `FETCHLOOM_CACHE` mistyped, the folder above it shortened to `/home/me`:

```text
error: there is no cache at /home/me/cahce, so there is nothing to free
  help: FETCHLOOM_CACHE names that folder: check it for a typo
```

## inspect

`fetchloom cache` reports the cache path, the bytes it holds and the projects using it, each on a `Used by` line while there are at most three, or counted on one line, which `-v` expands. the bare path goes to stdout only when stdout is not a terminal or `-q` hides the story, so `fetchloom -q cache` prints it for a script. JSON returns it in `result.path`, and every project in `result.projects`.

it also says where the space goes. stderr from a test on a drive that makes hardlinks, with the sandbox folder shortened to `/home/me`: a project needs `iris.csv` and `penguins.csv`, the second with `copy = true`, a dataset removed from it left `wine.csv`, and `get` linked `notes.txt` into a folder of its own:

```text
      Cache /home/me/cache · 4 files · 214 B
    Used by /home/me/work
     Needed 133 B by 1 project · 81 B of it is linked outside the cache too, so deleting it would free nothing
   Unneeded 81 B · gc frees 49 B once it is a day old; 32 B of it is linked outside the cache too, so deleting it would free nothing
     Copies 52 B in 1 project are separate copies of cached files
  hint: `fetchloom cache gc` frees what no project needs once it is a day old; `fetchloom cache gc --now` frees it now
```

- `Needed` is what the registered projects' data.lock files need. a file placed as a hardlink is a second name for the cached file, so its bytes are on the disk once, and deleting the cache's copy would free nothing of them.
- `Unneeded` is what no registered project needs, and when [gc](#gc) frees it: now, once it is a day old, or never while a folder outside the cache links it too, as one `get` made does. when nothing is unneeded, the report ends with `Ok every cached file is needed by a project`. the files of a fetched dataset that [waits for its file list](../reference/data-lock.md#big-datasets) in a registered project count here too, since nothing names them until the list is back, and a hint names that dataset.
- `Copies` is what the projects hold as files of their own beside the cached files: copies, clones, and hardlinks that lost their link, as [below](#copies-and-backups). a clone shares its blocks with the cache's copy until either changes, so it takes little room, and the line says how much of it is clones. an edited file is your own, so it does not count.

the sizes count what the files hold, while a disk stores each file in whole blocks, often 4 KiB, so `du` counts more, many times more for many small files: 4,000 files of about 33 bytes on ext4 were `4,000 files · 128.9 KiB` to `fetchloom cache` and 17388 KiB to `du -sk`. plan free space by `du`. JSON adds `needed`, `linked`, `unneeded`, `freeable`, `copies`, `unlinked_copies` and `cloned`, as [JSON output](../reference/json-output.md#command-results) defines; a script that wants what `cache gc` frees now reads `freeable`, not `bytes`. the report looks once at each placed file of each project, so a project on a slow network drive slows it.

downloads a run stopped before they finished keep their bytes under `partial/`, so a later run can continue them where the server allows, and `fetchloom cache` counts them on an `Unfinished` line, as JSON's `unfinished_downloads` and `unfinished_bytes`. bytes kept with no record beside them, as a run killed in a download's first second leaves, start again from their first byte unless the run knows the file's hash, as data.lock pins it or a source such as zenodo states it, which checks them once the rest is in; the line says how many have no record. gc removes a download's record only with its bytes, so bytes another program holds open, as windows refuses to remove them, keep their record for the next try. [cache folder](../reference/cache-folder.md) says where the cache is; `FETCHLOOM_CACHE` moves it.

### copies and backups

a project copied without keeping hardlinks, as `rsync -a` without `-H` copies a project with its cache, or `cp -a` copies a project alone, holds a file of its own for every file that was linked to the cache, so its bytes take room twice. `fetchloom cache` counts them, and a sync in the project links them to the cache again, as [copies of cached files](sync.md#copies-of-cached-files) describes. the end of the stderr of `fetchloom cache`, then of `fetchloom sync`, from a real run after `rsync -a` put back a project's data folder holding one 3.0 MiB file:

```text
     Needed 3.0 MiB by 1 project
     Copies 3.0 MiB in 1 project are separate copies of cached files; all of it was linked when placed, as `rsync -a` without -H or `cp -a` leaves it
         Ok every cached file is needed by a project
  hint: `fetchloom sync` in a project links its copies to the cache again
```

```text
     Linked data/iris · 1 copy of a cached file is now linked to the cache, freeing 3.0 MiB
     Synced 1 dataset into data
```

that sync brought the project and its cache from `6.2M` back to `3.2M` in `du -sh --total`. `rsync -aH` keeps the links; [backups and copies](../guides/clusters.md#backups-and-copies) has more.

### moving a project to another machine

a project copied with its data folder to a machine whose cache never held its files, as `rsync -a` copies it to another machine or cluster, needs no download for what that folder holds: a command that needs a pinned file the cache lacks takes it from the data folder before it asks a mirror or the source, when its bytes hash to the pin, and leaves the file there as it is. so `push` and `sync` work there `--offline` too. an edited file is never taken, and the files inside an archive come back this way only once the cache or a mirror lists what the archive holds. stderr of `fetchloom --offline push --to /mnt/lab/fetchloom --yes` in such a copy of a project whose datasets iris and twin hold one file, with an empty cache, from a real run with the mirror's path shortened:

```text
     Copied 1 file · 104 B from data into the cache
    Started a mirror in /mnt/lab/fetchloom
     Pushed 1 file · 104 B to /mnt/lab/fetchloom
         Ok every file iris and twin need is on the mirror
```

with `--json`, `work.data_folder_objects` and `work.data_folder_bytes` count those files. the data folder and the cache then each hold a copy, so the next sync links the data folder's to the cache's, as [copies of cached files](sync.md#copies-of-cached-files) describes.

## gc

1. read the registered projects' `data.toml` and `data.lock` to find the files they need: each pinned file, each archive's unpacked files but those a [members table](../reference/data-toml.md#files-inside-archives) leaves out, and what a sync cut short already downloaded of a dataset [`add --no-sync`](add.md) pinned.
2. remove unneeded files and records once they are at least 24 hours old by modification time, not last use; `--now` removes this grace period.
3. clean unfinished downloads nobody came back for in a day, or with `--now` every one no running fetchloom holds, and old temporary files, then report what it deleted.
4. report the unneeded files kept because they are less than a day old.

```sh
fetchloom cache gc --now
```

`--now` frees unneeded files whatever their age, and every unfinished download no running fetchloom holds, so the next run of those downloads starts over. temporary files keep their one-day grace period even then. without `--now`, a hint names what waits for its day:

```text
  hint: 4.1 KiB no project needs is kept for a day in case you want it back; `fetchloom cache gc --now` frees it now
```

JSON reports those as `result.kept_objects` and `result.kept_bytes`.

files [imported](add.md#files-on-this-machine) from a folder come back from no source, so gc counts them apart, as `result.imported_objects` and `result.imported_bytes` of what it removed, and its `Removed` line says how much of it they are. `--now` asks before it frees them, with what it would free, and a no changes nothing; with nobody to ask it stops with `unconfirmed`, exit 1, unless `--yes` answers. the question comes before gc holds the cache, and gc then frees only the imported files the answer covered. stderr from a dry run after `fetchloom remove precious --yes`:

```text
      Would remove 1 file · 32 B from the cache, 32 B of it imported from files no source can give back
```

every sync registers its project, also the sync inside `add` or `path`; `get` and `lock` register nothing. a registration warning means gc may not keep that project's bytes; check the cache's permissions and sync again.

`cache gc` forgets a project whose `data.toml` is gone, as when its folder moved, and frees what only it needed, naming it on a `Forgot` line and in `result.forgotten`. run `fetchloom sync` in the project's new folder to register it again; that does not put back what gc freed, as [below](#old-pins-and-moved-projects). a project whose folder is gone along with the disk it registered from, as a drive that is not connected leaves it on linux, macos and windows, is not forgotten: gc stops before it frees anything, naming the project, until the drive is back or you delete the record its help names. a network or memory disk keeps no number of its own for this, so a project there is forgotten once its `data.toml` is gone. `fetchloom cache` skips such a project but keeps its registration. a path it may not read does not count as gone, and an unreadable registration, `data.toml` or `data.lock` stops gc before it frees anything, with an error naming the path; when the project belongs to another user, as in a [lab's shared cache](../guides/clusters.md#one-cache-for-a-whole-lab), the help names them and what to ask. `fetchloom cache` warns of such a project and goes on, and its `Unneeded` line then says gc frees none of it while a project cannot be read. a `data.toml` that is a fifo, a link to a device, or larger than 256 MiB counts as unreadable, so a commit pulled into a clone cannot make every gc wait or fill memory.

removing a cached file leaves placed files intact. a cached file another name outside the cache still links keeps its bytes on the disk, so what gc removes can be more than the space it frees; `fetchloom cache` tells the two apart beforehand, as `unneeded` and `freeable`.

gc warns about paths it could not remove, and keeps what it removed: JSON keeps the counts and the failures in `result.not_removed`, each with `path`, `kind`, `os_code` and `message`, beside a `local.io` error, exit 50. fix the cause, then run it again. a link or a file where the cache keeps a folder of its own, as a junction someone made in `objects`, is named as one, with help to remove the link itself, not what it leads to; gc and verify never list, remove or hash what is behind it.

### old pins and moved projects

gc reads each registered project's data.lock as it is now, not its git history. after `fetchloom lock --upgrade`, no project needs the bytes older commits pin, so gc frees them, and their source may serve only the new bytes by then. to keep a way back, push the old pins to a folder mirror before the upgrade or gc: `fetchloom push --to <folder>`. a checkout of an older commit then gets the exact bytes with `--mirror`. from a real run after `push --to ../mirror`, `lock --upgrade`, `cache gc --now` and `git checkout` of the older commit, with the source serving the new bytes:

```sh
$ fetchloom sync
error: corpus/corpus.csv is not the file data.lock records
  expected 24 bytes, got 26
  help: to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run `fetchloom sync --mirror <folder>`. if the new bytes are intended, run `fetchloom lock --upgrade corpus`
$ fetchloom sync --mirror ../mirror
     Copied 1 file · 24 B from ../mirror
     Placed data/corpus · 1 file linked to the cache
     Synced 1 dataset into data
```

a project gc forgot, as one whose folder was moved away during gc, loses from the cache what only it needed. the sync that registers it again finds every file in place and puts nothing back, so its data folder then holds the only copy on this machine of those files. before you delete or move that data folder, push it to a folder mirror, which takes the files from the data folder, as [push](push.md#what-it-fetches) shows.

### runs that use the cache

gc never removes a file a running command is about to place: while `add`, `sync`, `get`, `cache` or `cache verify` work with the cache, gc waits, saying `Waiting for other fetchloom runs to finish with the cache`, a command started during gc waits for it the same way, and ctrl-c stops either wait. a dry run waits for nothing, so what a gc dry run lists can differ from what gc later frees. gc deletes nothing where it cannot lock the cache, as on a filesystem without locks or a read-only cache folder; on NFS it needs write access to the cache's `gc.lock`. [cache](../../dev/architecture/cache.md#layout) has the lock.

### reminder

fetchloom frees nothing by itself, apart from an archive's own bytes once its files are in the cache; freeing is `cache gc`'s, which you run on purpose. so the cache cannot grow unnoticed, after an `add`, `sync`, `remove` or `get` at a terminal, at most once a week, fetchloom checks what gc would free now and, when that is 1 GiB or more, says so once:

```text
  hint: the cache holds 3.2 GiB no project needs; `fetchloom cache gc` frees it
```

scripts, CI and coding agents never see it, and `-q` or `FETCHLOOM_NO_HINTS=1` turn it off.

### dry run

```sh
fetchloom cache gc --now --dry-run
```

`--dry-run` runs the same sweep with every deletion recorded instead of made, so it applies the same age rules, and changes nothing. stderr when one unneeded 32-byte file is in the cache, matching a test:

```text
      Would remove 1 file · 32 B from the cache
         Ok dry run, nothing was changed
```

`-v` names each path gc would delete. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## verify

1. hash every cached file again.
2. mark each changed file, so the next run that needs it downloads it again. a file [imported](add.md#files-on-this-machine) from a folder comes back from no source, so its warning says so, and the help names what brings it back: a folder mirror that holds it, or `fetchloom add <folder> --as <name>` again where the files are.
3. report how many files it checked, the changed ones and the ones it could not read.

a file it cannot read is named in a warning and not marked. JSON keeps the counts and findings in `result` beside the error. sync and get refuse a change mark they cannot read with `local.io`, exit 50, since they cannot tell then whether the cached bytes are safe: fix the named path's storage or permissions, verify the cache and run it again, and never delete the marks to get past the error.

verification does not check copies in `data/`. after it finds changed bytes, run `fetchloom sync` in each project that needs them: a file placed as a hardlink to the changed copy is put back without `--force`, keeping the changed copy beside it as `<path>.changed`, as [status](status.md#categories) describes.

## exit codes

| result | exit |
|---|---|
| successful inspection, gc or verification, also before the default cache folder holds a cache | 0 |
| verification found changed bytes | 30, `integrity.mismatch` |
| verification found unreadable paths but no changed bytes | 50, `local.io` |
| the folder `FETCHLOOM_CACHE` or the `cache` setting names is missing, so gc or verify has no cache to work on | 50, `local.io`; nothing is made |
| gc could not acquire its exclusive cache lock | 50, `local.io`; nothing is removed |
| gc could not remove or inspect a cleanup path | 50, `local.io`, even after other removals succeeded |
| interrupted work | 130 |

[exit codes](../reference/exit-codes.md) lists usage and other local failures.

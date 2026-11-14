# cache

`cache` says where the shared cache is and what fills it, frees what no project needs, and checks the cached bytes.

```sh
fetchloom cache
fetchloom cache gc
fetchloom cache gc --inputs --dry-run
fetchloom cache verify
```

## usage

```sh
fetchloom cache [options] [command]
fetchloom cache gc [options]
fetchloom cache verify [options]
```

`cache gc --now` also frees unneeded objects and unpack records less than a day old. `cache gc --inputs` also frees raw data that only built recipes read, where a folder mirror holds a copy that hashes right, once you say yes, as [raw input a mirror holds](#raw-input-a-mirror-holds) describes. `cache gc --dry-run`, or `-n`, shows what gc would free and deletes nothing. [global options](global-options.md) apply to all three forms.

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

- `Needed` is what the registered projects' data.lock files need. a file placed as a hardlink is a second name for the cached file, so its bytes are on the disk once, and deleting the cache's copy would free nothing of them. the files of a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read) that only built [recipes](../guides/recipes.md) read are needed too, and the line says how much they are, as `Needed 465 B by 1 project · 265 B of it is read only by builds`; `cache gc --inputs` frees them where a folder mirror holds a copy.
- `Unneeded` is what no registered project needs, and when [gc](#gc) frees it: now, once it is a day old, or never while a folder outside the cache links it too, as one `get` made does. when nothing is unneeded, the report ends with `Ok every cached file is needed by a project`.
- `Copies` is what the projects hold as files of their own beside the cached files: copies, clones, and hardlinks that lost their link, as [below](#copies-and-backups). a clone shares its blocks with the cache's copy until either changes, so it takes little room, and the line says how much of it is clones. an edited file is your own, so it does not count.

the sizes count what the files hold, while a disk stores each file in whole blocks, often 4 KiB, so `du` counts more, many times more for many small files: 4,000 files of about 33 bytes on ext4 were `4,000 files · 128.9 KiB` to `fetchloom cache` and 17388 KiB to `du -sk`. plan free space by `du`. JSON adds `needed`, `linked`, `unneeded`, `freeable`, `copies`, `unlinked_copies`, `cloned` and `inputs_only`, as [JSON output](../reference/json-output.md#command-results) defines; a script that wants what `cache gc` frees now reads `freeable`, not `bytes`. the report looks once at each placed file of each project, so a project on a slow network drive slows it.

downloads a run stopped before they finished keep their bytes under `partial/`, so a later run can continue them, and `fetchloom cache` counts them on an `Unfinished` line, as JSON's `unfinished_downloads` and `unfinished_bytes`. [cache folder](../reference/cache-folder.md) says where the cache is; `FETCHLOOM_CACHE` moves it.

### copies and backups

a project copied without keeping hardlinks, as `rsync -a` without `-H` copies a project with its cache, or `cp -a` copies a project alone, holds a file of its own for every file that was linked to the cache, so its bytes take room twice. `status` and `sync` there find nothing wrong, but `fetchloom cache` counts them. the end of its stderr after `rsync -a` copied a project and its cache, holding one 3.0 MiB file, and `fetchloom sync` ran in the copy with `FETCHLOOM_CACHE` naming the copied cache:

```text
     Needed 3.0 MiB by 2 projects
     Copies 3.0 MiB in 1 project are separate copies of cached files; all of it was linked when placed, as `rsync -a` without -H or `cp -a` leaves it
         Ok every cached file is needed by a project
  hint: to link those files to the cache again, delete their project's data folder, if it holds nothing else you need, and run `fetchloom sync` there
```

deleting the copy's `data/` and running `fetchloom sync` there brought it from `6.3M` back to `3.3M` in `du -sh`. `rsync -aH` keeps the links; [backups and copies](../guides/clusters.md#backups-and-copies) has more.

### moving a project to another machine

a project copied with its data folder to a machine whose cache never held its files, as `rsync -a` copies it to another machine or cluster, needs no download for what that folder holds: a command that needs a pinned file the cache lacks takes it from the data folder before it asks a mirror or the source, when its bytes hash to the pin, and leaves the file there as it is. so `push`, `build`, `build --check` and `sync` work there `--offline` too. an edited file is never taken, and the files inside an archive come back this way only once the cache or a mirror lists what the archive holds. stderr of `fetchloom --offline build --check` in such a copy, with an empty cache:

```text
      Needs up to 63 B on the disk holding the cache, which has 326.2 GiB free
     Copied 1 file · 21 B from data into the cache
    Running squares · cp -R <build>/in/table/. <build>/out
    Checked squares · 1 of 1 file matches data.lock
         Ok every step made the files data.lock pins
```

with `--json`, `work.data_folder_objects` and `work.data_folder_bytes` count those files.

## gc

1. read the registered projects' `data.toml` and `data.lock` to find the files they need: each pinned file, each archive's unpacked files but those a [members table](../reference/data-toml.md#files-inside-archives) leaves out, each [recipe](../guides/recipes.md)'s pinned result, every file of a dataset with `place = false`, which builds read, and the files of a [kept rebuild](build.md#a-rebuild-that-differs).
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

`cache gc` forgets a project whose `data.toml` is gone, as when its folder moved, and frees what only it needed, naming it on a `Forgot` line and in `result.forgotten`. run `fetchloom sync` in the project's new folder to register it again; that does not put back what gc freed, as [below](#old-pins-and-moved-projects). `fetchloom cache` skips such a project but keeps its registration, in case its drive is only disconnected. a path it may not read does not count as gone, and an unreadable registration, `data.toml` or `data.lock` stops gc with an error naming the path.

removing a cached file leaves placed files intact. a cached file another name outside the cache still links keeps its bytes on the disk, so what gc removes can be more than the space it frees; `fetchloom cache` tells the two apart beforehand, as `unneeded` and `freeable`.

gc warns about paths it could not remove, and keeps what it removed: JSON keeps the counts and the failures in `result.not_removed`, each with `path`, `kind`, `os_code` and `message`, beside a `local.io` error, exit 50. fix the cause, then run it again.

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

### raw input a mirror holds

gc keeps every file a project needs, including the raw data of a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read) that only built [recipes](../guides/recipes.md) read. `--inputs` frees that raw data as well, so a laptop short on disk keeps every result while the raw data waits on a lab disk or a usb drive. a file goes only when all of these hold:

- every registered project that needs it reads it only as raw input: fetched or imported data with `place = false`, read by recipes each built as data.lock pins it. a recipe still to build, a dataset that places the same file, a recipe's result or a [kept rebuild](build.md#a-rebuild-that-differs) keeps it. a recipe's result that only builds read stays too, since the next build would put it back, from a mirror or by running its recipe.
- no name outside the cache shares it, such as a hardlink in a project, since freeing it would free nothing.
- a folder [mirror](../guides/mirrors.md), from `--mirror`, `FETCHLOOM_MIRRORS` or the `mirrors` setting, holds a copy that gc reads whole and that hashes to its name in this run. a web mirror does not count, nor does a link or a file of another size in a copy's place, and a folder without its `fetchloom-mirror` file is warned about and skipped, as sync skips it. the folder `push_to` names counts only when it is one of those mirrors, since a build reads only mirrors.

[`fetchloom push --inputs <recipe>`](push.md) puts exactly that raw data on a folder mirror. gc reads each copy it relies on in full, so 200 GiB on the mirror means 200 GiB read, and a `Checked` line says how much and how long; then it says what it would free and keep and what freeing costs, and asks. stderr of a real run at a terminal on linux, paths shortened, for a project whose python recipe `clean` reads a 93.8 MiB table and the 5 files of a zip, both with `place = false`, after `fetchloom push --inputs clean --to /home/me/lab-mirror`:

```text
    Checked 6 files · 99.5 MiB on /home/me/lab-mirror in 392 ms
      Would free adult · 5 files, 5.7 MiB of raw input that only builds read
            /home/me/lab-mirror holds a copy of each, hashed in this run
      Costs rerunning clean fetches these 5.7 MiB back from /home/me/lab-mirror or adult's source
      Would free covid · 1 file, 93.8 MiB of raw input that only builds read
            /home/me/lab-mirror holds a copy of it, hashed in this run
      Costs rerunning clean fetches these 93.8 MiB back from /home/me/lab-mirror or covid's source
free adult and covid, 99.5 MiB, from the cache? [y/N] y
      Freed adult · 5 files, 5.7 MiB of raw input that only builds read
            /home/me/lab-mirror holds a copy of each, hashed in this run
      Freed covid · 1 file, 93.8 MiB of raw input that only builds read
            /home/me/lab-mirror holds a copy of it, hashed in this run
    Cleaned the cache, keeping the rest of what 1 project needs
```

`du -sh` of the cache went from `100M` to `136K`. the project still syncs, and `fetchloom build` finds clean up to date, since its result stays in the cache. a build that runs clean again, as after a change to clean.py or with `build --check`, copies the raw data back from a folder mirror, `--offline` too, as `Copied 6 files · 99.5 MiB from /home/me/lab-mirror`, or downloads it from its source. so keep the mirror where builds can read it: a [mirror is not an archive](../guides/mirrors.md#a-mirror-is-not-an-archive), and once it and the source have lost a file, no build can make the result again. use `--inputs` for raw data you rarely rebuild from.

no keeps everything as it was. `--yes` answers yes. without a terminal, and with `--json`, gc stops before it changes anything, the usual gc included, with `unconfirmed`, exit 1, and says what a yes would free; the JSON `fix` is null, since only a person may agree:

```text
error: freeing adult and covid, 99.5 MiB, from the cache needs a yes
  help: answer yes at a terminal, or add --yes, once you are sure the mirror named above keeps these files
```

the long read and the question never hold up other runs. after a yes, gc looks again and frees a file only while it is still raw input nothing else keeps and its copy on the mirror is as it was when hashed; then it frees what plain gc frees. `--dry-run` reads the copies too, says what gc would free and keep, and frees and asks nothing.

each file kept says why:

| reason | the file stays because |
|---|---|
| `pending` | a recipe that reads it is not built as data.lock pins it, so the next build reads it |
| `derived` | a recipe makes it, and the next build would put it back |
| `shared` | the same bytes are needed another way too, as by a dataset that places them |
| `linked` | a name outside the cache links it too, so freeing it would free nothing |
| `no_mirror` | no folder mirror is set; web mirrors do not count |
| `no_copy` | no folder mirror holds a copy |
| `damaged` | the copy on a mirror is of another size or does not hash to its name |
| `unreadable` | the copy on a mirror could not be read |
| `changed` | the copy on a mirror changed or went after gc read it |

a dry run names each kept file's reason in words, as `Would keep adult · 1 file, 3.8 MiB: its copy on /home/me/lab-mirror does not hash to its name` for a copy damaged with its size kept. [push](push.md) replaces a copy of another size by itself; delete one of the right size that does not hash to its name, and the next `push --inputs` writes it again.

JSON adds `result.inputs`: `freed_objects` and `freed_bytes`; `datasets`, a record for each dataset freed, with `route.mirrors`, the folder mirrors whose copies hashed right, and `costs.recipes`, the recipes that fetch it back to run again; and `kept`, a record for each dataset and reason, with `mirror`, the folder mirror a `damaged`, `unreadable` or `changed` copy is on, else null. `removed_objects` and `removed_bytes` keep counting only what plain gc removes. `result.inputs` of that damaged run with `--yes`, the paths shortened:

```json
{"freed_objects":5,"freed_bytes":100404272,"datasets":[{"project":"/home/me/study","name":"adult","objects":4,"bytes":2012789,"route":{"mirrors":["/home/me/lab-mirror"]},"costs":{"recipes":["clean"]}},{"project":"/home/me/study","name":"covid","objects":1,"bytes":98391483,"route":{"mirrors":["/home/me/lab-mirror"]},"costs":{"recipes":["clean"]}}],"kept":[{"project":"/home/me/study","name":"adult","objects":1,"bytes":3974305,"reason":"damaged","mirror":"/home/me/lab-mirror"}]}
```

## verify

1. hash every cached file again.
2. mark each changed file, so the next run that needs it downloads it again.
3. report how many files it checked, the changed ones and the ones it could not read.

a file it cannot read is named in a warning and not marked. JSON keeps the counts and findings in `result` beside the error. sync and get refuse a change mark they cannot read with `local.io`, exit 50, since they cannot tell then whether the cached bytes are safe: fix the named path's storage or permissions, verify the cache and run it again, and never delete the marks to get past the error.

verification does not check copies in `data/`. after it finds changed bytes, run `fetchloom sync` in each project that needs them: a file placed as a hardlink to the changed copy is put back without `--force`, keeping the changed copy beside it as `<path>.changed`, as [status](status.md#categories) describes.

## exit codes

| result | exit |
|---|---|
| successful inspection, gc or verification | 0 |
| verification found changed bytes | 30, `integrity.mismatch` |
| verification found unreadable paths but no changed bytes | 50, `local.io` |
| gc could not acquire its exclusive cache lock | 50, `local.io`; nothing is removed |
| gc could not remove or inspect a cleanup path | 50, `local.io`, even after other removals succeeded |
| `gc --inputs` had something to free and nobody said yes, or the answer was no | 1, `unconfirmed`; nothing is removed |
| interrupted work | 130 |

[exit codes](../reference/exit-codes.md) lists usage and other local failures.

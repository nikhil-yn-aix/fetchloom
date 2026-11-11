# cache

`cache` reports shared storage, collects unneeded bytes and checks stored objects.

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

`cache gc --now` also frees unneeded objects and unpack records less than a day old. `cache gc --inputs` also frees raw data that only built recipes read, where a folder mirror holds a copy that hashes right, once you say yes, as [raw input a mirror holds](#raw-input-a-mirror-holds) describes. `cache gc --dry-run`, or `-n`, shows what gc would free and deletes nothing. [global options](README.md#global-options) apply to all three forms.

## inspect

`fetchloom cache` reports the cache path, the bytes it holds and the projects using it. it prints the bare path on stdout only when stdout is not a terminal or quiet mode hides the story; at a terminal the story already shows it. JSON returns it in `result.path`.

it also says where the space goes. stderr from a test on a drive that makes hardlinks, with the sandbox folder shortened to `/home/me`: a project needs `iris.csv` and `penguins.csv`, the second with `copy = true`, a dataset removed from it left `wine.csv`, and `get` linked `notes.txt` into a folder of its own:

```text
      Cache /home/me/cache · 4 files · 214 B
    Used by /home/me/work
     Needed 133 B by 1 project · 81 B of it is linked outside the cache too, so deleting it would free nothing
   Unneeded 81 B · gc frees 49 B once it is a day old; 32 B of it is linked outside the cache too, so deleting it would free nothing
     Copies 52 B in 1 project are separate copies of cached files
  hint: `fetchloom cache gc` frees what no project needs once it is a day old; `fetchloom cache gc --now` frees it now
```

- `Needed` is what the registered projects' data.lock files need. a file placed as a hardlink is a second name for the cached file, so those bytes are on the disk once, and deleting the cache's copy would free nothing of them. files placed as clones or copies are files of their own, so they add nothing to `linked`; [placement](../concepts.md#placement) says which a drive gets. the files of a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read) that only built [recipes](../recipes.md) read are needed too, and the line says how much of it they are, as `Needed 465 B by 1 project · 265 B of it is read only by builds`: they are in no data folder, and the recipes reading them are built, so only a build that runs again reads them. gc keeps them; `cache gc --inputs` frees them where a folder mirror holds a copy.
- `Unneeded` is what no registered project needs, and when [gc](#gc) frees it: now, once it is a day old, or never while a folder outside the cache links it too, as one `get` made does. gc removes such a file from the cache, but its bytes stay with the other name. when nothing is unneeded, the report ends with `Ok every cached file is needed by a project`.
- `Copies` is what the registered projects hold as files of their own beside the cached files: files placed as copies or clones, as `copy = true` places them, and files placed as hardlinks that lost their link, as [below](#copies-and-backups). a clone shares its blocks with the cache's copy until either changes, so it can take far less room than it counts for, and the line says how much of it is clones. a file counts only while its size and modification time are as placed, since an edited file is your own, and only while the cache still holds its bytes, since otherwise the file is their only copy.

the sizes are estimates in payload bytes, what the files hold, as gc counts them. `du` counts the whole blocks a file takes, often 4 KiB each, and the cache's folders, so it can show a little more. JSON adds `result.needed`, `linked`, `unneeded` and `freeable`, what `cache gc` frees now, so a script promises `freeable` rather than `bytes`, and `copies`, with `unlinked_copies`, the part of it that lost its link, and `cloned`, the part placed as clones: `copies - cloned` is what takes room twice. `cloned` goes by how each file was placed, so in a project copied by a tool that writes every file out whole, files placed as clones are counted as clones though they no longer share blocks. `result.inputs_only` is the part of `needed` only builds read: files of datasets with `place = false` that recipes read and that every project needing them only reads that way, each of those recipes built as data.lock pins it. the report reads each registered project's recipes and code to tell, so it counts a recipe whose code changed since its build as one still to build, whose inputs it keeps out of `inputs_only`. the report reads each registered project's record of what it placed and looks once at each placed file, so a project on a slow network drive slows it.

byte totals exclude indexes and partial downloads. downloads a run stopped before they finished keep their bytes under `partial/`, so a later run can continue them; `fetchloom cache` names how many there are and what they hold in an `Unfinished` line, and JSON reports them as `result.unfinished_downloads` and `result.unfinished_bytes`. what they hold is the space they take on the disk: a download split into parts writes each part where it belongs, so until it finishes its file can be far longer than what the disk holds for it. a full disk under the cache names what they hold too, in the error. [cache folder](../reference/cache-folder.md) gives default locations and record formats; `FETCHLOOM_CACHE` overrides the root.

### copies and backups

a project copied without keeping hardlinks, as `rsync -a` without `-H` copies a project together with its cache, or `cp -a` copies a project alone, holds a file of its own for every file that was linked to the cache, so its bytes take room twice. `status` and `sync` there find nothing wrong, since every file still holds its pinned bytes. `fetchloom cache` counts them. the end of its stderr in a run on linux, after `rsync -a` copied a project and its cache, holding one 3.0 MiB file, and `fetchloom sync` ran in the copy with `FETCHLOOM_CACHE` naming the copied cache:

```text
     Needed 3.0 MiB by 2 projects
     Copies 3.0 MiB in 1 project are separate copies of cached files; all of it was linked when placed, as `rsync -a` without -H or `cp -a` leaves it
         Ok every cached file is needed by a project
  hint: to link those files to the cache again, delete their project's data folder, if it holds nothing else you need, and run `fetchloom sync` there
```

in that run `du -sh` showed `3.3M` for the original project and cache and `6.3M` for the copy; deleting the copy's `data/` and running `fetchloom sync` there brought the copy back to `3.3M`. `rsync -aH` keeps the links; [backups and copies](../hpc.md#backups-and-copies) has more.

### moving a project to another machine

a project copied with its data folder to a cache that never held its files, as `rsync -a` copies it to another machine or cluster, needs no download for what that folder holds. a command that needs a pinned file the cache lacks takes it from the data folder before it asks a mirror or the source: from a file at a path data.lock places it at, whether or not `.fetchloom` came along. the cache gets a clone where the disk makes one, else a copy, never a link, and keeps it only when its bytes hash to the pin, so the file stays as it is. `build`, `build --check` and a `sync` that must place a file at another path then work there with `--offline` too. a file edited since it was placed holds other bytes, so it is never taken, and the command goes on as it would without it: to a mirror, the source, or with `--offline` an `offline.missing` error. the files inside an archive come back this way only once the cache or a mirror lists what the archive holds. stderr from a real run of `fetchloom --offline build --check` in such a copy, with an empty cache:

```text
      Needs up to 63 B on the disk holding the cache, which has 326.2 GiB free
     Copied 1 file · 21 B from data into the cache
    Running squares · cp -R <build>/in/table/. <build>/out
    Checked squares · 1 of 1 files match data.lock
         Ok every step made the files data.lock pins
```

with `--json`, `work.data_folder_objects` and `work.data_folder_bytes` count those files. the copy's files stay files of their own beside the cache's, as above.

## gc

1. read registered project locks to find needed objects and unpacked members. a file inside an archive that the project's [members table](../reference/data-toml.md#files-inside-archives) leaves out is not needed, and its absence no longer keeps the packed archive; a data.toml that does not read keeps every file. a [recipe](../recipes.md)'s pinned result is needed with its files and the record of its files under `builds/`, and so is every file of a dataset with `place = false`, which builds read. the files of a [kept rebuild](build.md#a-rebuild-that-differs) a project's state records are needed too.
2. remove unneeded objects, unpack records and the records of results no project pins once they are at least 24 hours old by modification time, not last use; `--now` removes this grace period. a key under `builds/` left with no record goes with its lock.
3. clean unfinished downloads nobody came back for in a day, or with `--now` every one no running fetchloom holds, old temporary files and the lock files of downloads nobody holds, and forget links whose bytes are gone, then report deleted counts and bytes.
4. report unneeded objects kept because they are less than a day old.

```sh
fetchloom cache gc --now
```

`--now` ignores modification times for unneeded objects, unpack records and result records, including future times that FAT timestamp rounding can produce. `--now` also removes unfinished downloads that no running fetchloom holds, however new, so the next run of those downloads starts over; a download in progress keeps its bytes, since its run holds its lock. temporary files keep their one-day grace period even with `--now`.

the kept-file hint names the retained bytes:

```text
  hint: 4.1 KiB no project needs is kept for a day in case you want it back; `fetchloom cache gc --now` frees it now
```

a gc dry run shows the same hint. `cache gc` JSON reports these young unneeded objects as `result.kept_objects` and `result.kept_bytes`, excluding objects a registered project needs. plain `fetchloom cache` says beforehand what gc frees now and what waits for its day, in its `Unneeded` line, and JSON reports what gc frees now as `result.freeable`; [inspect](#inspect) has the details.

successful project sync registers the project, including sync through `add` or `path`. `get` and standalone `lock` do not register. a registration warning means gc may not keep that project's bytes; check permissions and sync again.

`cache gc` removes a registration when its manifest is missing, as when a project folder moved, and then frees what only that project needed. it names each project it forgets this way, in a `Forgot` line and in `result.forgotten`; a dry run names them with `-v` and in `result.forgotten`. run `fetchloom sync` in a project's new folder to register it again. a failed registration removal warns, appears in `result.not_removed` and returns `local.io` (exit 50), even if object cleanup succeeded. inspection skips that project but keeps its registration, in case its drive is disconnected. an unreadable registration, manifest or lock stops inspection or gc with an error naming the path. inaccessible paths do not count as missing.

removing a cache object leaves placed files intact. an object that a file outside the cache still links keeps its bytes on the disk, so the bytes gc removes can be more than the space it frees; `fetchloom cache` tells the two apart beforehand, as `unneeded` and `freeable`.

gc warns about paths it could not remove or inspect for cleanup. JSON retains successful counts and failure records in `result.not_removed`, alongside the error. each record has `path`, `kind`, `os_code` and `message`. follow the cause-specific advice: fix access for permission errors, or check storage for I/O errors, then retry. already absent paths do not count as failed removals.

### runs that use the cache

every command that opens the cache to work with it, such as `add`, `sync`, `get`, `cache` and `cache verify`, holds the empty `gc.lock` file in the cache folder with a shared OS lock while it works, and `cache gc` takes that lock alone, as cargo and uv lock their caches. so gc never removes an object that a running command has found and is about to place. while another command uses the cache, gc prints `Waiting for other fetchloom runs to finish with the cache` and starts once they have; a command started during gc waits for it in the same way. ctrl-c stops either wait. dry runs, `cache gc --dry-run` among them, take no lock and never wait, so what a gc dry run lists can differ from what a gc run later frees. when the filesystem explicitly reports that locks are unsupported, shared-cache commands can continue, but gc refuses to delete without an exclusive lock. other lock errors stop the command. gc also refuses when a read-only cache root prevents creating its lock. a read-only `gc.lock` can still protect shared readers; on NFS, gc needs write access to that file to take its exclusive lock.

### reminder

fetchloom does not automatically collect unneeded payload objects, apart from a packed archive no project needs once its files are unpacked. abandoned staging files are cleaned during unpacking or gc. freeing bytes is `cache gc`'s, which you run on purpose. so the cache cannot grow unnoticed, after an `add`, `sync`, `remove` or `get` at a terminal, at most once a week, fetchloom checks what no project needs and, when what gc would free now totals 1 GiB or more, says so once. it counts that as `fetchloom cache` counts `freeable`:

```text
  hint: the cache holds 3.2 GiB no project needs; `fetchloom cache gc` frees it
```

scripts, CI and coding agents never see it, since it only runs when a person can read it. with `-q` or `FETCHLOOM_NO_HINTS=1`, which hide hints, it does not read the cache at all. the first run only starts the weekly clock.

### dry run

```sh
fetchloom cache gc --now --dry-run
```

`--dry-run` runs the same sweep with every deletion recorded instead of made, so it applies the same age rules and keeps the same files. the cache, partial downloads and project registrations stay as they are, and a cache that does not exist yet is not created. explicit `FETCHLOOM_TRACE` output is still created or truncated. stderr when one unneeded 32-byte object is in the cache, matching a test:

```text
Would remove 1 file · 32 B from the cache
         Ok dry run, nothing was changed
```

`-v` names each path gc would delete, including the registrations of projects whose `data.toml` is gone. a path it cannot remove is named in a warning and does not fail the dry run; an object folder or project registration it cannot read stops it, as it stops gc. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

### raw input a mirror holds

gc keeps every file a project needs, including the raw data of a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read) that only built [recipes](../recipes.md) read. `--inputs` frees that raw data as well, so a laptop short on disk keeps every result while the raw data waits on a lab disk or a usb drive. a file goes only when all of these hold:

- every registered project that needs it reads it only as raw input: fetched or imported data with `place = false`, read by recipes each built as data.lock pins it. a recipe still to build, a dataset that places the same file, a recipe's result or a [kept rebuild](build.md#a-rebuild-that-differs) keeps it. a recipe's result that only builds read stays too, since the next build would put it back, from a mirror or by running its recipe.
- no name outside the cache shares it, such as a hardlink in a project, since freeing it would free nothing.
- a folder [mirror](../mirrors.md), from `--mirror`, `FETCHLOOM_MIRRORS` or the `mirrors` setting, holds a copy that gc reads whole and that hashes to its name in this run. a web mirror does not count, nor does a link or a file of another size in a copy's place, and a folder without its `fetchloom-mirror` file is warned about and skipped, as sync skips it. the folder `push_to` names counts only when it is one of those mirrors, since a build reads only mirrors.

[`fetchloom push --inputs <recipe>`](push.md) puts exactly that raw data on a folder mirror. gc reads each copy it relies on in full, so 200 GiB on the mirror means 200 GiB read; a `Checked` line says how much it read and how long it took. then it says what it would free and keep, and what freeing costs, and asks. stderr from a real run at a terminal on linux, the paths shortened, for a project whose python recipe `clean` reads a 93.8 MiB table and the 5 files of a zip, both with `place = false`, after `fetchloom push --inputs clean --to /home/me/lab-mirror`:

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

`du -sh` of the cache said `100M` before and `136K` after. the project still syncs, and `fetchloom build` finds clean up to date, since its result stays in the cache. a build that runs clean again, as after a change to clean.py or with `build --check`, copies the raw data back from a folder mirror, as `Copied 6 files · 99.5 MiB from /home/me/lab-mirror`, or downloads it from its source; folder mirrors are read with `--offline` too. so keep the mirror where builds can read it: a [mirror is not an archive](../mirrors.md#a-mirror-is-not-an-archive), and once it and the source have lost a file, no build can make the result again.

no keeps everything as it was. `--yes` answers yes. without a terminal, and with `--json`, gc stops before it changes anything, the usual gc included, with `unconfirmed`, exit 1, and says what a yes would free; the JSON `fix` is null, since only a person may agree:

```text
error: freeing adult and covid, 99.5 MiB, from the cache needs a yes
  help: answer yes at a terminal, or add --yes, once you are sure the mirror named above keeps these files
```

gc reads the copies and asks before it takes its lock, so the long read and the question never hold up other runs. once you say yes it takes the lock as plain gc does, reads the registered projects again, and frees a file only while that look still finds it raw input nothing else keeps, and while its copy on the mirror has the size and the time it had when it hashed. then it frees what plain gc frees. a registered project whose data.toml or data.lock cannot be read stops it before anything is freed, as it stops gc. a second run finds nothing left to free and asks nothing. `--dry-run` reads the copies too, says what gc would free and keep, and frees and asks nothing.

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

the lines about adult in a dry run of the same project, after one copy on the mirror was damaged, its size kept and a byte changed:

```text
 Would free adult · 4 files, 1.9 MiB of raw input that only builds read
            /home/me/lab-mirror holds a copy of each, hashed in this run
      Costs rerunning clean fetches these 1.9 MiB back from /home/me/lab-mirror or adult's source
 Would keep adult · 1 file, 3.8 MiB: its copy on /home/me/lab-mirror does not hash to its name
```

[push](push.md) replaces a copy of another size by itself; delete one of the right size that does not hash to its name, and the next `push --inputs` writes it again.

JSON adds `result.inputs`: `freed_objects` and `freed_bytes`, each file once; `datasets`, a record for each dataset freed, with `project`, `name`, `objects`, `bytes`, `route.mirrors`, the folder mirrors whose copies hashed to their names, and `costs.recipes`, the recipes that read it, which fetch it back from those mirrors or its source to run again; and `kept`, a record for each dataset and reason, with `project`, `name`, `objects`, `bytes`, `reason` as above, and `mirror`, the folder mirror a `damaged`, `unreadable` or `changed` copy is on, else null. `removed_objects` and `removed_bytes` keep counting only what plain gc removes. a dry run counts what gc would free, and its `changes.remove` names each file too. `result.inputs` of that damaged run with `--yes`, the paths shortened:

```json
{"freed_objects":5,"freed_bytes":100404272,"datasets":[{"project":"/home/me/study","name":"adult","objects":4,"bytes":2012789,"route":{"mirrors":["/home/me/lab-mirror"]},"costs":{"recipes":["clean"]}},{"project":"/home/me/study","name":"covid","objects":1,"bytes":98391483,"route":{"mirrors":["/home/me/lab-mirror"]},"costs":{"recipes":["clean"]}}],"kept":[{"project":"/home/me/study","name":"adult","objects":1,"bytes":3974305,"reason":"damaged","mirror":"/home/me/lab-mirror"}]}
```

## verify

1. read each cached object's metadata and rehash its bytes.
2. note each changed file, so the next download that needs it replaces it.
3. report examined counts, changed hashes and unreadable paths.

unreadable objects stay unmarked with warnings. failures to write change marks are also reported. JSON retains counts and findings in `result` beside the error. unreadable directories can hide objects from the totals; counts cover only objects whose metadata could be read.

sync and get refuse unreadable change marks with `local.io` (exit 50), because a read failure cannot show that cached bytes are safe. fix the named path's storage or permissions, verify the cache and retry; do not delete the marks to bypass the error.

verification does not check independent copies in `data/`. after it finds changed bytes, run `fetchloom sync` in each project that needs the affected files: a file placed as a hardlink to the changed copy is put back without `--force`, even in the project whose link was edited, since that edit changed the copy every project shares; the changed copy stays beside it as `<path>.changed`. see [status](status.md#categories). cache scans check cancellation between entries; verification also checks between hash reads.

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

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a recent unused object remains | gc uses a 24-hour age rule | collect later or use `cache gc --now` |
| `get` data is collected | one-off downloads do not register a project | use a project for inputs whose cache retention matters |
| a damaged independent copy passes cache verification | only cache objects are checked | inspect that project's files separately |
| gc reports errors after deleting files | cleanup can partially succeed | inspect the causes in `not_removed` and retry after fixing them |
| `gc --inputs` keeps raw data with `no_copy` or `no_mirror` | the mirror lacks it, or gc was not told of the mirror | run `fetchloom push --inputs <recipe> --to <folder>`, then name that folder with `--mirror` or the `mirrors` setting |
| a build after `gc --inputs` copies or downloads the raw data again | gc freed what the build reads | expected; use `--inputs` for raw data you rarely rebuild from |

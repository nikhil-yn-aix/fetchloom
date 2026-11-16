# sync

`sync` puts the files `data.lock` pins into the data folder, and keeps your local edits unless you force it.

```sh
fetchloom sync
fetchloom sync iris
fetchloom sync --offline
fetchloom sync --locked
fetchloom sync --strict
fetchloom sync --verify
fetchloom sync --force --dry-run
```

## usage

```sh
fetchloom sync [options] [name]...
```

names, as data.toml gives them, limit the sync to those datasets, as [named datasets](#named-datasets) describes; with none, sync works on every dataset.

| option | effect |
|---|---|
| `--force` | replace edited files, and files fetchloom did not place, at the paths `data.lock` lists, naming each replaced file, and remove what stands in a locked file's way: a link, never what it points to, a file where a folder belongs, or an empty folder. it finds changes by size and time; add `--verify` for changes that kept both |
| `--locked` | fail instead of updating data.lock when it is out of date with data.toml, for CI. given names, only their entries are checked |
| `--verify` | check the SHA-256 of every placed file and of each cached file it places, and put back or download again those that changed though their size and time did not |
| `--strict` | fail with `data.not_ready`, exit 1, when the data asked for is not ready to use once the sync is done, naming each file or dataset with what makes it ready. the sync does the same work and keeps edits as without it, as [local changes](#local-changes) shows |
| `-n`, `--dry-run` | plan locking, downloads, placement and removal without applying project or cache changes |

[global options](global-options.md) also apply.

## what happens

1. find the project and read its manifest and lock.
2. fail if `--locked` finds a missing or stale lock; otherwise update it, keeping unchanged entries. a fetched dataset [renamed](../reference/data-toml.md#dataset-names) in data.toml keeps its pin under the new name, with no request, and says `Renamed iris to flowers in data.lock · its pin stays`. a dataset pinned for the first time gets a `Pinned` line with what it resolved to, its title, file count and size, so an id with a typo that pins an unrelated record stands out.
3. reuse unchanged placed files and cached files, then take what the cache lacks from files [already in place](#already-in-place) in the data folder, then from [other caches](../guides/other-tools.md#reusing-other-caches) on this machine, such as hugging face's, then from the [mirrors](../guides/mirrors.md) you named, and download the rest. in [mirror-only mode](../guides/mirrors.md#mirror-only-mode), a file no mirror holds fails the run before any download.
4. check downloads against their pinned sizes and hashes, then place files and remove the unchanged files fetchloom placed that no dataset needs now.
5. save the project's state and register the project with the cache.
6. add the data folder to `.gitignore` if needed: in a git repository, and outside one on the project's first sync, when there is no data.lock yet.

with `--json`, `result.datasets` names the datasets whose pins changed and `result.selected` the names the sync was given, or null; [JSON output](../reference/json-output.md) has the rest. [placement](../concepts/cache-and-placement.md#placement) says how files are placed and when they are writable.

a locked 32-byte file that upstream replaced with a 15-byte file fails (stderr):

```text
error: iris/iris.csv is not the file data.lock records
  expected 32 bytes, got 15
  help: to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run `fetchloom sync --mirror <folder>`. if the new bytes are intended, run `fetchloom lock --upgrade iris`
```

a [folder mirror](../guides/mirrors.md) that holds the pinned bytes gives them back. upgrade only when you mean to change the input, since it pins what the source serves now. with `--json` the error's `fix` is null. [recovery](../concepts/offline-and-recovery.md#recovery) covers expired download links and interrupted transfers.

a sync that cannot get every file still gets and places the rest. an archive that cannot be fetched, an import this machine's cache lacks or a file that fails to download leaves its own dataset waiting, and the run downloads and places every other dataset before it fails. the error names each file that failed, as `errors[0].failures` does in JSON, then the datasets it placed and those that wait, which `result.datasets` and `result.waiting` give. when placing the datasets it has whole fails too, as in a folder it may not write, a line of the error says why. a server that stops answering ends the downloads still running, so their datasets wait too, and the file that failed first in the run's order gives the [exit code](../reference/exit-codes.md).

## named datasets

`fetchloom sync small` works on small alone: it resolves small when its entry in data.toml changed, downloads and places its files, and removes the files it placed for small that small no longer pins. every other dataset keeps its entry in data.lock and its files as they are, even one data.toml changed, dropped or added. stderr in a fresh clone of a project with datasets small and large, from a real run:

```text
 Downloaded 1 file · 1 B in 26 ms
     Placed data/small · 1 file linked to the cache
     Synced small into data · 1 other dataset left as it was
```

the options follow the names: `--force small` replaces edits in small's files only, `--verify small` hashes small's files only, and `--locked small` checks only small's entry in data.lock, so a job needing small passes while another entry is stale. a warning about an edit a named sync keeps names the `--force` for that file's dataset alone. a name data.toml does not have fails before any request, with exit 2 and the closest name:

```text
error: data.toml has no dataset called "smal"
  help: did you mean `fetchloom sync small`?
```

a settled `sync small` looks at small alone, so it costs what small holds, and JSON's `result.placement` counts small's files alone. `status` still lists what it left as it was, and since a named sync never records the project as fully synced, the next plain `sync` looks at every dataset again, placing what the named sync left out and removing the files of a dataset gone from data.toml. a file of another dataset where a named dataset needs a folder stays, even with `--force`, and its warning says to sync that dataset first.

## already in place

a file someone put at a pinned path, such as one downloaded by hand, is adopted when it holds the pinned bytes: sync takes it as placed and downloads nothing for it when the SHA-256 is known. when the cache holds the bytes and the drive links, sync then replaces the file with a link to the cache's copy in one rename, a clone or a hardlink as it places a new file, so one copy stays on disk. stderr on a drive that makes hardlinks, matching a test; on one that clones, the line ends `so it is now cloned from the cache`:

```text
    Adopted data/iris · 1 file already held the pinned bytes, so it is now linked to the cache
```

a file in a dataset with `copy = true`, or whose bytes the cache lacks, is adopted but not linked, and the line ends `so it stays as it is`. before the sync, `status` counts such files on one `In place` line, as no difference, and a dry run lists them under `Would adopt`. `remove` later deletes an adopted file as it deletes the files sync placed, and warns that it was already there.

a file whose bytes the cache lacks can also come from another file in the data folder that holds them at a pinned path, as when two datasets hold one file, in a project [moved to another machine](cache.md#moving-a-project-to-another-machine). the cache gets a clone or a copy of it, never a link, and keeps it only when it hashes to the pin, so nothing is downloaded. stderr from a real run in a copy of a project whose datasets iris and twin hold one file, with an empty cache, after `data/twin/iris.csv` was deleted:

```text
     Copied 1 file · 104 B from data into the cache
     Placed data/twin · 1 file linked to the cache
    Adopted data/iris · 1 file already held the pinned bytes, so it is now linked to the cache
     Synced 2 datasets into data
```

## copies of cached files

a file fetchloom placed by link can become a file of its own: `rsync -a` without `-H` or `cp -a` copied the data folder, a push took the files of a project [moved to another machine](cache.md#moving-a-project-to-another-machine) into its cache, or the files were copied while the cache was on another drive and the cache moved to the project's. its bytes then take room twice, once in the data folder and once in the cache. sync links each such copy to the cache's copy, as it links a new file, and says how much that frees. stderr on a drive that makes hardlinks, matching a test:

```text
     Linked data/iris · 1 copy of a cached file is now linked to the cache, freeing 32 B
```

sync reads each copy first, unless the same run just read it, and links only one that holds the pinned bytes and still looks as placed: the link takes the copy's name in one rename, so a program reading the copy keeps reading the bytes it opened. a file with other bytes, or one the drive refuses to link, stays as it is, and no later sync reads it again. sync looks for copies only where the data folder and the cache share a drive that links, so a project on FAT or exFAT, or one whose cache is on another drive, pays nothing for them. a dataset with `copy = true` keeps its copies. a clone, as fetchloom places on APFS, Btrfs or XFS, shares the cache's blocks, and its size and time do not tell it from a copy, so a copy that took a clone's place stays. on windows, a copy another program holds open without sharing stays until a sync after it is closed.

## folders left behind

sync deletes only unchanged files it placed, and only inside the data folder. when a dataset goes from `data.toml`, or its `path` changes, its old folder goes with the last such file, along with files a system wrote there by itself, such as finder's `.DS_Store`, though not what a folder such as a disk's trash holds, which may be yours. a folder that still holds anything else, such as work of yours or a `.changed` copy, stays, and sync names it (stderr):

```text
warning: data/pets still holds data/pets/own.txt, a file fetchloom did not place, so the folder stays
  help: `fetchloom status` lists it as extra; delete it when you no longer need it
```

when `data_dir` changes, sync places the files in the new data folder and deletes nothing in the old one. it names the old folder once, since it then stops recording those files, as `warning: data still holds 2 files fetchloom placed before data_dir changed to inputs/raw, such as data/iris/iris.csv. fetchloom leaves them there`, with the help `delete data when you no longer need what it holds`. when the old folder also holds files fetchloom did not place, the warning names up to three, and its help says to move them out first. stderr from a real run after `data_dir` changed to `inputs`, with four such files in data:

```text
warning: data still holds 2 files fetchloom placed before data_dir changed to inputs, such as data/iris/iris.csv, and 4 files it did not place: data/iris/my-notes.txt, data/readme.txt, data/scratch/plot.py and 1 more. fetchloom leaves them all there
  help: move the 4 files fetchloom did not place out of data before you delete data
```

a dry run warns the same way, and `-v` names each file. when the old folder holds the new one, as when `data_dir` goes from `inputs` to `inputs/raw`, the warning names the files rather than a folder to delete.

## local changes

a file fetchloom placed that someone deleted or moved comes back, since data.lock still pins it, and sync names the first five and counts the rest; JSON lists them in `result.placement.put_back`. stderr after a sample was deleted by hand, from a test run:

```text
warning: put back data/pets/images/dog_010_corrupt.png, which was deleted or moved, since data.lock pins it
  help: to leave a file out for good, run `fetchloom exclude data/pets/images/dog_010_corrupt.png`; keep files of your own in a folder outside data/
```

[`fetchloom exclude`](exclude.md) writes a pattern into data.toml, so every clone leaves the file out too. the help names exclude only where it works: the only file of a dataset goes with the dataset, through `fetchloom remove`. a folder deleted whole inside a dataset is named once, as `put back 60 files that were deleted or moved, since data.lock pins them: the 30 in data/many/part03/, the 30 in data/many/part04/`, with the `fetchloom exclude` of that folder.

a dataset's own folder, or the whole data folder, deleted to start afresh is no mistake, so sync places it again with no warning and says why, as `Placed data/many · 120 files linked to the cache · the folder was missing`. a copy you moved elsewhere in the data folder stays, and status lists it as extra. a dry run warns the same way, as `a sync would put back`.

without `--force`, edited files and untracked files at pinned paths stay in place, and JSON lists them in `result.placement.kept`; other extra files are not listed there, and [status](status.md) finds them. the warning about each names the `fetchloom sync --force` that replaces it. a sync that placed files ends with where a script finds the dataset, as `` next: `fetchloom path cows` tells your scripts where cows is``.

a script that must not read an edited input asks for `--strict`. the sync does the same work and keeps the edit, then fails with `data.not_ready`, exit 1, while the data is not ready to use: a file kept with other bytes than data.lock pins. `status --check` answers the same question without changing anything. stderr after `iris.csv` was edited, from a real run:

```text
warning: data/iris/iris.csv has changed, so it stays
  help: `fetchloom sync --force` replaces it
     Synced 1 dataset into data, 1 file left unchanged; see the warnings above
error: the data is not ready to use
  data/iris/iris.csv changed since fetchloom placed it: save what you need, then `fetchloom sync --force` replaces it
  help: each line says what makes it ready
```

once the file holds the pinned bytes again, `sync --strict` exits 0. a strict sync that fails prints no closing `Ok` line before its error, since the error says what is not ready. with `--json`, `errors[0].kind` is `data.not_ready` and `result` holds the sync's report as without `--strict`.

save your files before `--force`: it overwrites edited and untracked files at pinned paths, naming each, as `Replaced data/iris/iris.csv · discarding your edit`, and JSON lists them in `result.placement.overwritten`; `--dry-run` warns about each first. it deletes no extra file, nor an edited file no dataset needs now. until the pinned files are in place, a replaced file waits as `.fetchloom-<hex>.old` in its folder, deleted once they have their names and renamed back if placing fails or is stopped. a run cut off at once, by a second ctrl-c, a kill or a crash, can leave one, but its name stays on record: status names the file it holds, and the next sync moves it back unless something took its name since, saying so on a `Moved back` line. one that cannot go back, as when a folder now has its name, stays under its hidden name, and the run, every later sync and status say where it is until you move it.

a hardlinked file is the cache's own copy, shared by every project that placed it. an edit through any of its links, after making it writable, changes that shared copy, so sync treats it as damage, not as a project's edit: it downloads the file again and, without `--force`, puts the pinned file back in each project whose file is still that copy. each of those keeps the changed bytes beside it as `<path>.changed`, or `<path>.changed.2` when that name is taken, its own copy that status lists as extra until you delete it, so the edit is not lost. a run that stops or fails after keeping one says where it is in its error. a file saved anew, as most editors save, is the project's own and stays an edit. to edit a dataset's files, set `copy = true` for it. the warning reads:

```text
warning: <path> was put back as pinned: it is linked to the cache's copy, which was changed. the changed copy is kept as <path>.changed
  help: to edit a dataset's files, set copy = true for it in data.toml
```

when that download fails, as offline, nothing is put back and the edited file stays. the error names each such file under the download's own message, as `data/iris/iris.csv is linked to the cache's copy, which was changed, so its pinned bytes must be fetched again`, and the next sync that can download puts it back.

a changed hardlink shared by other projects, whose cached copy the cache no longer records, as after the cache folder was deleted, stays with kept reason `shared` until `fetchloom sync --force`, since nothing tells which project made the change. on windows, where a folder's listing can keep a hardlink's old size and time, `status` and `sync` open each hardlinked file to read it as it is, so such an edit shows at once there too.

an entry in a pinned file's way, such as a link at its path or a file where its folder belongs, stays without `--force`, with kept reason `blocked` and its path as `blocked_by`. with `--force`, sync removes it, a link itself and never what it points to; a folder that holds anything stays even then. a file fetchloom placed that no dataset needs now is removed first, so when a pinned file becomes a folder of files, or the other way round, one sync places the new files.

sync compares sizes and times, and hashes only files those cannot vouch for, so with nothing to do it says `Ok all pinned files are in place by size and time, nothing to do`. `--verify` checks the hash of every placed file too, and puts back one whose bytes changed though its size and time did not, without `--force`, keeping the changed bytes beside it as `<path>.changed`, since they may be an edit. it hashes each file it takes from the cache as well, so a cached file that changed under its size and time, as a cache restored from elsewhere can hold, is downloaded again with a `cache.changed` warning rather than placed. a dataset that [waits for its file list](../reference/data-lock.md#big-datasets) is left as it is, files placed before included, so the closing line counts only the datasets placed and names it, as `Synced 1 dataset into data · clean waits for its file list` or `Ok all pinned files but clean's are in place by size and time, nothing to do`.

if registering with the cache fails, sync still succeeds, with a warning that `cache gc` may not keep the project's files; check the cache's permissions and run sync again.

## dry run

`sync --dry-run`, or `-n`, prints what sync would change without downloading files for placement, writing `data.lock` or state, changing `data/` or writing the cache. to tell whether `data.lock` would change, it resolves changed entries as `lock` does, so a source may still be asked what a link holds, as [kaggle](../sources/kaggle.md) is for a competition. stderr after a dataset `cows` was added to `data.toml`, matching a test, where `[port]` stands for the test server's port:

```text
      Would pin cows, new in data.lock: http://127.0.0.1:[port]/files/cows.csv · 1 file · size unknown
      Would fetch cows/cows.csv · size unknown
      Would place data/cows · 1 file
         Ok dry run, nothing was changed
```

each download is listed with its size, or `size unknown` when only the download tells, and a file the data folder already holds is none. a file whose hash only a download can learn counts as one to place, or as `Would check` when a file already stands where it goes. an archive not unpacked here is `Would open`, without its members. `--force` lists the files it would overwrite as `Would renew`; without it, warnings name them, as a real sync does. `Would remove` counts the files no dataset needs now, and `Would retire` those a drop cut short left; `-v` names each file.

`--locked` still checks the lock first. offline, a dry run that would need a download fails as the sync would, with exit 20. a dry run takes no hold on the project, so a sync running at the same time can change what it reports. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

a checkout you may not write, such as a read-only mount in a job or a container, still syncs when nothing needs writing: `data.toml` and `data.lock` are as the last complete sync left them, and every placed file still looks as placed. a sync that would write fails, naming the folder it could not write.

## exit codes

| exit | when |
|---|---|
| 0 | the sync succeeded, also with kept conflicts or a registration warning |
| 1 | `--strict` found the data not ready to use, `data.not_ready`; `--locked` found a missing or stale lock, `lock.outdated`, before anything changed, as `there is no data.lock, so nothing pins the files' bytes yet`; or a question went unanswered |
| 2 | a name data.toml does not have |
| 20 | data missing offline |
| 30 | downloaded bytes differ from the pins |

other failures use the [shared exit codes](../reference/exit-codes.md).

## stopping a sync

ctrl-c while a sync resolves or downloads stops it before it places any file, with `Stopped while talking to zenodo.org, so nothing changed`, as [interruptions](../concepts/questions-and-interruptions.md#interruptions) explains. a sync that already wrote `data.lock`, which it does before it downloads, keeps the new `data.lock` and says `so the data folder is as it was` instead.

during placement, the first ctrl-c stops new placements and removals; those under way may finish, and unfinished copies never take their final names. run sync again to finish.

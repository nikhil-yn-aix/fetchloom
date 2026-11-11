# sync

`sync` restores the files in `data.lock` while preserving local edits by default.

```sh
fetchloom sync
fetchloom sync iris
fetchloom sync --offline
fetchloom sync --locked
fetchloom sync --verify
fetchloom sync --force --dry-run
```

## usage

```sh
fetchloom sync [options] [name]...
```

name datasets, by their names in data.toml, to sync only those, as [named datasets](#named-datasets) describes; with no name, sync syncs every dataset.

| option | effect |
|---|---|
| `--force` | replace edited files, and files fetchloom did not place, at the paths `data.lock` lists, naming each replaced file, and remove what stands in a locked file's way: a link, never what it points to, a file where a folder belongs, or an empty folder. it finds changes by size and time; add `--verify` for changes that kept both |
| `--locked` | fail instead of updating data.lock when it is out of date with data.toml, for CI. given names, only their entries are checked |
| `--verify` | check the SHA-256 of every placed file, and put back those that changed though their size and time did not |
| `-n`, `--dry-run` | plan locking, downloads, placement and removal without applying project or cache changes |

[global options](README.md#global-options) also apply.

## what happens

1. find the project and read its manifest and lock.
2. fail if `--locked` finds a missing or stale lock; otherwise update it, retaining unchanged entries. a dataset pinned for the first time gets a `Pinned` line of its own, with what it resolved to, its title, file count and size, so an entry whose id has a typo and pins an unrelated record stands out.
3. reuse unchanged local files and cached objects, copy what the cache lacks from the [mirrors](../mirrors.md) you named, and download the rest when online. in [mirror-only mode](../mirrors.md#mirror-only-mode) a file no mirror holds fails the run before any download. a file fetchloom did not place that already holds the pinned bytes is adopted, as [already in place](#already-in-place) describes.
4. check downloads against locked sizes and hashes, then place files and remove unchanged managed files no longer needed.
5. save project state and register the project with the cache.
6. in a git repository, add the data folder to `.gitignore` if needed, as `add` does, so a repository started after the first `add` stops listing `data/`.

with `--json`, `result.datasets` names the datasets whose pins changed: pinned for the first time, pinned to other data, or no longer in `data.lock`. `result.selected` lists the names the sync was given, or is null when it was given none. [JSON output](../reference/json-output.md) describes the rest.

placement tries a reflink, then a hardlink, then a copy; read-only placement on windows tries hardlinks first. files are read-only by default. recognized synced folders skip hardlinks. `copy = true` requests writable reflinks or copies. see [placement](../concepts.md#placement).

if a locked 32-byte file is replaced upstream by a 15-byte file, stderr reports:

```text
error: iris/iris.csv is not the file data.lock records
  expected 32 bytes, got 15
  help: if the new version is intended, run `fetchloom lock --upgrade iris`
```

follow the upgrade hint only when you intend to change the input. [recovery](../concepts.md#recovery) covers expired download URLs and interrupted transfers.

## named datasets

`fetchloom sync small` works on small alone. it resolves small when its entry in data.toml changed, downloads and places small's files, and removes the files fetchloom placed for small that small no longer pins. every other dataset keeps its entry in data.lock and its files as they are, even one data.toml changed, dropped or added: nothing of theirs is resolved, downloaded, placed, replaced or removed. stderr in a fresh clone of a project with datasets small and large, from a real run:

```text
 Downloaded 1 file · 1 B in 26 ms
     Placed data/small · 1 file linked to the cache
     Synced small into data · 1 other dataset left as it was
```

`--force small` replaces edits in small's files only, and the warning about a file a named sync keeps names that command, so following it replaces nothing in the other datasets. `--verify small` hashes small's files only, `--locked small` checks only small's entry in data.lock, so a job needing small passes while another entry is stale, and `--dry-run` shows what the named sync would do. a name data.toml does not have fails before any request, with exit 2 and the closest name:

```text
error: data.toml has no dataset called smal
  help: did you mean `fetchloom sync small`?
```

a named sync never records the project as fully synced, so the next plain `sync` looks at every dataset again: it places what the named sync left out, and removes the files of a dataset gone from data.toml. a file of another dataset that stands where a named dataset needs a folder stays, even with `--force`, and its warning says to sync that dataset first, or every dataset.

## already in place

a file someone put at a pinned path before fetchloom placed it, such as one downloaded by hand, is adopted when it holds the pinned bytes: sync takes it as placed and counts it as unchanged. when `data.lock` or the source states the file's SHA-256, nothing is downloaded for it; otherwise the download that learns the hash puts the bytes in the cache. when the cache holds them and the drive makes hardlinks, sync then replaces the file with a link to the cache's copy, so one copy stays on disk. the file keeps its bytes until the link takes its name in one rename, and a file changed since sync looked at it stays as it is. stderr, matching a test:

```text
    Adopted data/iris · 1 file already held the pinned bytes, so it is now linked to the cache
```

a file in a dataset with `copy = true`, a file with another name, and a file whose bytes the cache does not hold stay as they are, and the line says so: `so it stays as it is`. `remove` later deletes an adopted file as it deletes the files sync placed, and warns that it was already there. a dry run lists adoptions under `Would adopt`, and a file whose hash only its download can tell under `Would check`.

## folders left behind

sync deletes only files it placed that are unchanged, and only inside the data folder. when a dataset goes from `data.toml`, or its `path` changes, its old folder goes with the last such file. a folder that still holds anything else, such as work of yours or a `.changed` copy, stays, and sync names it (stderr):

```text
warning: data/pets still holds data/pets/own.txt, a file fetchloom did not place, so the folder stays
  help: `fetchloom status` lists it as extra; delete it when you no longer need it
```

when `data_dir` changes, sync places the files in the new data folder and deletes nothing in the old one, which is outside the data folder now. the sync that does so names the old folder, once, since its state then stops recording those files (stderr):

```text
warning: data still holds 2 files fetchloom placed before data_dir changed to inputs/raw, such as data/iris/iris.csv. fetchloom leaves them there
  help: delete data when you no longer need what it holds
```

a dry run warns the same way, and `-v` names each file. when the old folder holds the new one, as when `data_dir` goes from `inputs` to `inputs/raw`, the warning names the files rather than a folder to delete.

## local changes

without `--force`, edited files and conflicting untracked files stay in place. JSON lists them in `result.placement.kept`. unrelated extras are not included there; use [status](status.md) to find them.

save your files before using `--force`. it can overwrite edited and untracked files at locked paths, and names each one as it does, as in `Replaced data/iris/iris.csv · your edit`; JSON lists them in `result.placement.overwritten`, and `--dry-run` warns about each first. it does not delete unrelated extras or edited files no longer needed. a file it replaces, or takes out of a locked file's way, keeps its bytes until the pinned files are in place: sync first renames it to `.fetchloom-<hex>.old` in the same folder, deletes that once the pinned files have their names, and renames it back when placing fails or is stopped. a run cut off midway can leave such a file, which status lists as extra.

a file placed as a hardlink is the cache's own copy, which every project placing that file shares. an edit made in place through any link to it, after making the read-only file writable, changes that shared copy, so sync treats it as damage, not as a project's edit: it marks the cached copy as changed, downloads the file again and puts the pinned file back, without `--force`, in each project whose file is still that copy, the one that made the edit included. each of those projects keeps the changed copy beside the pinned file as `<path>.changed`, or `<path>.changed.2` and on when that name is taken, so the edit is not lost; status lists it as extra until you delete it. each project's `.changed` file is its own: while other projects' links still share the changed copy, sync clones or copies it rather than linking it, so editing one project's `.changed` file changes no other's. to edit a dataset's files, set `copy = true` for it. a file saved anew, as most editors save, is the project's own and stays an edit. the warning reads, with `<path>` replaced by the affected path:

```text
warning: <path> was put back as pinned: it is linked to the cache's copy, which was changed. the changed copy is kept as <path>.changed
  help: to edit a dataset's files, set copy = true for it in data.toml
```

a changed hardlink still shared by other projects whose shared copy the cache no longer holds or records, as after the cache folder was deleted, stays with kept reason `shared` until `fetchloom sync --force`, since nothing tells which project made the change.

on windows, a folder's listing can keep a hardlink's old size and time after another link changed it, so `status` and `sync` open each file placed as a hardlink to read it as it is. an edit made through another project's link then shows here at once, as on linux.

an entry in a locked file's way, such as a link at its path or a file where its folder belongs, stays without `--force`, with kept reason `blocked` and the entry's path as `blocked_by`. with `--force`, sync removes it, a link itself and never what it points to, and places the files; a folder that holds anything stays even then. a file fetchloom placed that no dataset needs now is in nobody's way: sync removes it first, as it removes any such file, so when a pinned file becomes a folder of files, or a folder becomes one file, one sync places the new files. an old file you changed stays, and still keeps the new files out as blocked.

sync compares sizes and times, and hashes only files those cannot vouch for, so with nothing to do it says `Ok all pinned files are in place by size and time, nothing to do`. `--verify` hashes every placed file too: one whose bytes changed though its size and time did not is put back without `--force`, while an edit that shows in its size or time keeps the rule above.

if cache registration fails, sync still succeeds with a warning that gc may not keep the project's files. check cache permissions and run sync again.

## dry run

`sync --dry-run`, or `-n`, prints what sync would change without downloading files for placement, writing `data.lock` or state, changing `data/` or writing the cache. explicit `FETCHLOOM_TRACE` output is still created or truncated. to see whether `data.lock` would change, it resolves changed entries as `lock` does, so a source may still be asked what a link holds; for a kaggle competition whose rules the account may not have accepted, that means reading its smallest file when its listed size is at most 4 MiB. a zip wrapper can make the response larger; the probe uses the 64 MiB API-response limit and keeps none of the bytes. stderr after a dataset `cows` was added to `data.toml`, matching a test, where `[port]` stands for the test server's port:

```text
  Would pin cows, new in data.lock: http://127.0.0.1:[port]/files/cows.csv · 1 file · size unknown
Would fetch cows/cows.csv · size unknown
Would place data/cows · 1 file
         Ok dry run, nothing was changed
```

each file it would download is listed with its size, or `size unknown` when only the download tells. a file whose hash only a download can learn counts as one to place; when a file already stands where it goes, it is listed under `Would check` instead, with no warning, since only the download tells whether it holds the pinned bytes. an archive this machine has not unpacked is listed with `Would open`, and its members are not counted, since only unpacking lists them. with `--force`, files it would overwrite are listed under `Would renew`; without it, they are named in warnings, as a real sync names them. `-v` names each file.

`--locked` still checks the lock first. a project still as a complete sync left it has nothing to do, as a real sync finds, even when the cache was emptied since. offline, a dry run that would need a download fails as the sync would, with exit 20. this includes an edited hardlink whose shared cache object must be downloaded again, even if its read-only flag was restored. the dry run checks those bytes without writing a cache change mark. a dry run takes no project hold, so a sync running at the same time can change what it reports. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

a checkout this user may not write, such as a read-only mount in a job or a container, still syncs when nothing needs writing: `data.toml` and `data.lock` are as the last complete sync left them and every placed file still looks as placed. a sync that would write fails, naming the folder it could not write.

## exit codes

0 includes success with preserved conflicts or a registration warning. a name data.toml does not have exits 2 with `usage`. offline missing data exits 20; changed downloaded bytes exit 30. approval failures exit 1. a missing or stale lock under `--locked` exits 1 with `lock.outdated` before downloads or changes to the manifest, lock or data folder. a missing lock reports `there is no data.lock yet`. other failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| success leaves an edited input | sync preserves conflicts | inspect status and save the edit before choosing `--force` |
| offline sync needs resolution | the manifest changed or pins are missing | prepare the project online before disconnecting |
| inputs reject writes | placement is read-only | set `copy = true` in the dataset and sync |
| the lock changed unexpectedly | sync updates stale locks | use `sync --locked` in a job that requires committed pins |
| status still lists another dataset after `sync <name>` | a named sync leaves the other datasets as they are | run `fetchloom sync` |

## stopping a sync

ctrl-c while a sync resolves or downloads stops it before it places any file. it ends with `Stopped while talking to zenodo.org, so nothing changed`, and a dim line under it says how much the unfinished downloads keep, which is what the next sync goes on from. a sync that already wrote `data.lock`, which it does before it downloads what placing needs, keeps the new `data.lock` and says `so the data folder is as it was` instead.

during placement, the first ctrl-c stops starting new placements and removals. an operation already in the filesystem may finish. completed files stay recorded; unfinished copies never take their final names. run sync again to finish.

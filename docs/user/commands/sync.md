# sync

`sync` restores the files in `data.lock` while preserving local edits by default.

```sh
fetchloom sync
fetchloom sync --offline
fetchloom sync --locked
fetchloom sync --force --dry-run
```

## usage

```sh
fetchloom sync [options]
```

| option | effect |
|---|---|
| `--force` | replace conflicting files at locked paths, including untracked files |
| `--locked` | fail instead of updating data.lock when it is out of date with data.toml, for CI |
| `-n`, `--dry-run` | plan locking, downloads, placement and removal without applying project or cache changes |

[global options](README.md#global-options) also apply.

## what happens

1. find the project and read its manifest and lock.
2. fail if `--locked` finds a missing or stale lock; otherwise update it, retaining unchanged entries.
3. reuse unchanged local files and cached objects, downloading missing bytes when online.
4. check downloads against locked sizes and hashes, then place files and remove unchanged managed files no longer needed.
5. save project state and register the project with the cache.

placement tries a reflink, then a hardlink, then a copy; read-only placement on windows tries hardlinks first. files are read-only by default. recognized synced folders skip hardlinks. `copy = true` requests writable reflinks or copies. see [placement](../concepts.md#placement).

if a locked 32-byte file is replaced upstream by a 15-byte file, stderr reports:

```text
error: iris/iris.csv is not the file that was locked
  expected 32 bytes, got 15
  help: if the new version is intended, run `fetchloom lock --upgrade iris`
```

follow the upgrade hint only when you intend to change the input. [recovery](../concepts.md#recovery) covers expired download URLs and interrupted transfers.

## local changes

without `--force`, edited files and conflicting untracked files stay in place. JSON lists them in `result.placement.kept`. unrelated extras are not included there; use [status](status.md) to find them.

save your files before using `--force`. it can overwrite edited and untracked files at locked paths. it does not delete unrelated extras or edited files no longer needed.

a hardlink edit can change every project sharing the cache object. when the local file shows that change, sync preserves it with kept reason `shared`; it cannot tell which project edited it. this is the usual behavior on linux. save the edit, then use `fetchloom sync --force` in each affected project to restore its pins.

the kept-shared warning uses this wording, with `<path>` replaced by the affected path:

```text
<path> was edited, and the file is shared with other projects, so they see the edit too. it stays; `fetchloom sync --force` puts the original back
```

on windows, a folder's listing can keep a hardlink's old size and time after another link changed it, so `status` and `sync` open each file placed as a hardlink to read it as it is. an edit made through another project's link then shows here at once, as on linux, and stays with the `shared` warning until `sync --force`.

if cache registration fails, sync still succeeds with a warning that gc may not keep the project's files. check cache permissions and run sync again.

## dry run

`sync --dry-run`, or `-n`, prints what sync would change without downloading files for placement, writing `data.lock` or state, changing `data/` or writing the cache. explicit `FETCHLOOM_TRACE` output is still created or truncated. to see whether `data.lock` would change, it resolves changed entries as `lock` does, so a source may still be asked what a link holds; for a kaggle competition whose rules the account may not have accepted, that means reading its smallest file when its listed size is at most 4 MiB. a zip wrapper can make the response larger; the probe uses the 64 MiB API-response limit and keeps none of the bytes. stderr after a dataset `cows` was added to `data.toml`, matching a test:

```text
 Would lock cows · 1 file in data.lock
Would fetch cows/cows.csv · size unknown
Would place data/cows · 1 file
         Ok dry run, nothing was changed
```

each file it would download is listed with its size, or `size unknown` when only the download tells. a file whose hash only a download can learn counts as one to place. an archive this machine has not unpacked is listed with `Would open`, and its members are not counted, since only unpacking lists them. with `--force`, files it would overwrite are listed under `Would renew`; without it, they are named in warnings, as a real sync names them. `-v` names each file.

`--locked` still checks the lock first. a project still as a complete sync left it has nothing to do, as a real sync finds, even when the cache was emptied since. offline, a dry run that would need a download fails as the sync would, with exit 20. this includes an edited hardlink whose shared cache object must be downloaded again, even if its read-only flag was restored. the dry run checks those bytes without writing a cache change mark. a dry run takes no project hold, so a sync running at the same time can change what it reports. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## exit codes

0 includes success with preserved conflicts or a registration warning. offline missing data exits 20; changed downloaded bytes exit 30. approval failures exit 1. a missing or stale lock under `--locked` exits 1 with `lock.outdated` before downloads or changes to the manifest, lock or data folder. a missing lock reports `there is no data.lock yet`. other failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| success leaves an edited input | sync preserves conflicts | inspect status and save the edit before choosing `--force` |
| offline sync needs resolution | the manifest changed or pins are missing | prepare the project online before disconnecting |
| inputs reject writes | placement is read-only | set `copy = true` in the dataset and sync |
| the lock changed unexpectedly | sync updates stale locks | use `sync --locked` in a job that requires committed pins |

## stopping placement

the first ctrl-c stops starting new placements and removals. an operation already in the filesystem may finish. completed files stay recorded; unfinished copies never take their final names. run sync again to finish.

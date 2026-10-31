# sync

`sync` restores the files in `data.lock` while preserving local edits by default.

```sh
fetchloom sync
fetchloom sync --offline
fetchloom sync --locked
```

## usage

```sh
fetchloom sync [options]
```

| option | effect |
|---|---|
| `--force` | replace conflicting files at locked paths, including untracked files |
| `--locked` | fail instead of updating data.lock when it is out of date with data.toml, for CI |

[global options](README.md#global-options) also apply.

## what happens

1. find the project and read its manifest and lock.
2. fail if `--locked` finds a missing or stale lock; otherwise update it, retaining unchanged entries.
3. reuse unchanged local files and cached objects, downloading missing bytes when online.
4. check downloads against locked sizes and hashes, then place files and remove unchanged managed files no longer needed.
5. save project state and register the project with the cache.

placement tries a reflink, then a hardlink, then a copy. files are read-only by default. recognized synced folders skip hardlinks. `copy = true` requests writable reflinks or copies. see [placement](../concepts.md#placement).

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

on windows, a directory listing can still show the recorded size and time after another link changed. a cache change mark then triggers a byte check; if that proves an otherwise unchanged alias damaged, sync repairs it and lists its path in `result.placement.restored`. an unseen change may need the editing project to sync, `cache verify`, or `sync --force` to trigger detection.

if cache registration fails, sync still succeeds with a warning that gc may not keep the project's files. check cache permissions and run sync again.

## exit codes

0 includes success with preserved conflicts or a registration warning. offline missing data exits 20; changed downloaded bytes exit 30. approval failures exit 1. a missing or stale lock under `--locked` exits 1 with `lock.outdated` before downloads or changes to the manifest, lock or data folder. other failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| success leaves an edited input | sync preserves conflicts | inspect status and save the edit before choosing `--force` |
| offline sync needs resolution | the manifest changed or pins are missing | prepare the project online before disconnecting |
| inputs reject writes | placement is read-only | set `copy = true` in the dataset and sync |
| the lock changed unexpectedly | sync updates stale locks | use `sync --locked` in a job that requires committed pins |

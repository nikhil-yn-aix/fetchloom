# sync

`sync` restores the files in `data.lock` while preserving local edits by default.

```sh
fetchloom sync
fetchloom sync --offline
```

## usage

```sh
fetchloom sync [options]
```

| option | effect |
|---|---|
| `--force` | replace conflicting files at locked paths, including untracked files |

[global options](README.md#global-options) also apply.

## what happens

1. find the project and read its manifest and lock.
2. update the lock if it is missing or stale; unchanged entries keep their pins.
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

sync repairs hardlinks it finds changed through another project. it warns and lists repaired paths in `result.placement.restored`. on windows, another project's edit can remain invisible until that project syncs, cache verification runs, or `sync --force` reads each hardlink by name.

if cache registration fails, sync still succeeds with a warning that gc may not keep the project's files. check cache permissions and run sync again.

## exit codes

0 includes success with preserved conflicts or a registration warning. offline missing data exits 20; changed downloaded bytes exit 30. approval failures exit 1. other failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| success leaves an edited input | sync preserves conflicts | inspect status and save the edit before choosing `--force` |
| offline sync needs resolution | the manifest changed or pins are missing | prepare the project online before disconnecting |
| inputs reject writes | placement is read-only | set `copy = true` in the dataset and sync |
| the lock changed unexpectedly | sync updates stale locks | use `lock --check` first in a job that requires committed pins |

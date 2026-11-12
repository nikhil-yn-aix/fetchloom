# path

`path` returns an absolute dataset, file or subfolder path, syncing that dataset first when needed.

```sh
fetchloom path iris
fetchloom path iris/Iris.csv
fetchloom --offline path iris/Iris.csv
fetchloom path iris --locked
```

## usage

```sh
fetchloom path [options] <name>[/file]
```

| option | effect |
|---|---|
| `--locked` | fail instead of updating data.lock when the dataset's entry is out of date with data.toml, for CI |

[global options](README.md#global-options) also apply. use `/` between the dataset name and file path on both platforms. the target starts with a dataset's name: an empty one, or a path on this machine such as `/etc/passwd`, is refused with `usage`, exit 2, quoting it as typed.

## what happens

1. find the dataset in the project manifest.
2. check whether the requested member can exist in the lock or a cached archive tree.
3. if the dataset is not ready, sync it alone, as `fetchloom sync <name>` does: no other dataset is resolved, downloaded or changed. in a git repository, add the data folder to `.gitignore`, as `sync` does. with `--locked`, first fail as `sync --locked <name>` does when data.lock is missing or the dataset's entry is out of date with data.toml, before any request.
4. print its absolute location on stdout. JSON returns it as `result.path`.

`-v` adds one line on stderr about where the bytes are: a placed file is a link to its file in the cache folder, or a copy of it; `fetchloom cache` shows the cache. [placement](../concepts.md#placement) says when you get which.

a ready dataset takes no `.fetchloom/lock` hold and prints no closing summary. member validation, readiness and the answer share one lock read and one plan, of the dataset and of those whose folders meet its folder, which a clash with its files could involve. a ready answer reads only their pins, the state's records in its folder and a listing of that folder, so it costs what the dataset holds, not what the project holds. while waiting for another command, a changed state file triggers a fresh readiness check: a reader whose dataset is now ready answers without taking the lock; otherwise fetchloom takes it and rereads the project before deciding whether sync is still needed. a subfolder is accepted when it contains locked files. when an archive tree is unknown, sync may be needed before checking its members.

stderr excerpt for a path absent from a dataset:

```text
error: dataset has no file or folder called not-a-file.csv
```

a [recipe](../recipes.md) that changed since its build keeps the result data.lock pins, so `path` answers its folder and warns what changed, as sync does, naming the `fetchloom build` that makes it again. it reads the pins and data.toml entries of the datasets the recipe is made from to tell, and fetches nothing of them. a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read) has no folder in the data folder, so `path` refuses it:

```text
error: raw has place = false, so it has no folder in data/
  data.toml:2   place = false
  help: set place = true on datasets.raw in data.toml, then run `fetchloom sync raw`
```

`path` returns a location, not proof of intact bytes. sync may have kept an edited file there. use [status](status.md) and inspect sync's preserved conflicts when correctness depends on unchanged inputs.

`--locked` never writes data.lock. a dataset already in place answers as usual, since nothing needs writing; one that needs a sync first, such as a dataset added to data.toml by hand, fails with `lock.outdated` instead of being pinned and downloaded. only the named dataset's entry counts, so another dataset's stale entry stops nothing.

## exit codes

0 returns a location. an unknown dataset, absent member, dataset with `place = false` or dataset whose archives contain no regular files exits 2 with `usage`. a [recipe](../recipes.md) `data.lock` pins no result of yet, or whose pinned result neither the cache nor a mirror holds, exits 60 with `build.needed`, since only `fetchloom build` makes a result. a missing lock, or a stale entry for the dataset, under `--locked` exits 1 with `lock.outdated`. implicit sync can also return its download, integrity or local [exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| status still lists other datasets as missing after `path` | path syncs only the dataset it names | run `fetchloom sync` |
| a script's `path` changed data.lock | a dataset added to data.toml by hand gets pinned by the sync `path` runs for it | pass `--locked` in scripts and CI, and run `fetchloom lock` yourself |
| a requested member is rejected before syncing | the lock or cached tree already rules it out | choose a path the dataset holds |
| an empty archive has no dataset folder | extraction produced no regular files | set `extract = false` and sync to keep the packed archive |
| a notebook needs a filesystem object | CLI output is text | use the [python helper](../scripting.md#python-helper) for `pathlib.Path` |

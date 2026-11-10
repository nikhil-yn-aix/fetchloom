# path

`path` returns an absolute dataset, file or subfolder path, syncing first when needed.

```sh
fetchloom path iris
fetchloom path iris/Iris.csv
fetchloom --offline path iris/Iris.csv
```

## usage

```sh
fetchloom path [options] <name>[/file]
```

there are no command-specific options. [global options](README.md#global-options) apply. use `/` between the dataset name and file path on both platforms.

## what happens

1. find the dataset in the project manifest.
2. check whether the requested member can exist in the lock or a cached archive tree.
3. if the dataset is not ready, sync the whole project and, in a git repository, add the data folder to `.gitignore`, as `sync` does.
4. print its absolute location on stdout. JSON returns that location as a string in `result`.

`-v` adds one line on stderr about where the bytes are: a placed file is a link to its file in the cache folder, or a copy of it; `fetchloom cache` shows the cache. [placement](../concepts.md#placement) says when you get which.

a ready dataset takes no `.fetchloom/lock` hold and prints no closing summary. member validation, readiness and the answer share one lock read and one plan. while waiting for another command, a changed state file triggers a fresh readiness check: a reader whose dataset is now ready answers without taking the lock; otherwise fetchloom takes it and rereads the project before deciding whether sync is still needed. a subfolder is accepted when it contains locked files. when an archive tree is unknown, sync may be needed before checking its members.

stderr excerpt for a path absent from a dataset:

```text
error: dataset has no file or folder called not-a-file.csv
```

`path` returns a location, not proof of intact bytes. sync may have kept an edited file there. use [status](status.md) and inspect sync's preserved conflicts when correctness depends on unchanged inputs.

## exit codes

0 returns a location. an unknown dataset, absent member or dataset whose archives contain no regular files exits 2 with `usage`. implicit sync can also return its download, integrity or local [exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| looking up one file downloads other datasets | a missing dataset triggers a whole-project sync | prepare the project first, or use `--offline` to forbid requests |
| a requested member is rejected before syncing | the lock or cached tree already rules it out | choose a path the dataset holds |
| an empty archive has no dataset folder | extraction produced no regular files | set `extract = false` and sync to keep the packed archive |
| a notebook needs a filesystem object | CLI output is text | use the [python helper](../scripting.md#python-helper) for `pathlib.Path` |

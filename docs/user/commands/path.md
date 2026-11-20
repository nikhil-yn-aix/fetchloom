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

[global options](global-options.md) also apply. use `/` between the dataset name and file path on both platforms. the target starts with a dataset's name, or is a path to a dataset's folder or to something in it as `ls` shows it, read from the folder you are in: `data/iris/Iris.csv` from the project's folder, `../data/iris/Iris.csv` from a folder in it, or `Iris.csv` inside `data/iris`, each answered as `iris/Iris.csv` is. a file name alone that one dataset's folder holds, such as `Iris.csv` from the project's folder, gets ``did you mean `fetchloom path iris/Iris.csv`?``, and so does a path in the data folder with a typo in a dataset's folder, such as `data/irs/Iris.csv`. an empty target, or a path on this machine outside the data folder such as `/etc/passwd`, is refused with `usage`, exit 2, quoting it as typed.

## what happens

1. find the dataset in the project manifest.
2. check whether the requested member can exist in the lock or a cached archive tree.
3. if the dataset is not ready, sync it alone, as `fetchloom sync <name>` does: no other dataset is resolved, downloaded or changed. with `--locked`, first fail as `sync --locked <name>` does when data.lock is missing or the dataset's entry is out of date with data.toml, before any request.
4. print its absolute location on stdout. JSON returns it as `result.path`.

on windows, a script that captures the path in powershell or cmd sets the console to utf-8 first, or reads `result.path` from `--json`, whose document is ASCII, as [paths with accents in scripts](../guides/windows.md#paths-with-accents-in-scripts) shows; otherwise a folder name with an accent arrives garbled.

`-v` adds, after [what the run spends](../guides/speed-and-memory.md#see-what-a-run-uses), one line on stderr about where the bytes are: a placed file is a link to its file in the cache folder, or a copy of it; `fetchloom cache` shows the cache. [placement](../concepts/cache-and-placement.md#placement) says when you get which.

a ready dataset prints no closing summary, and the check costs what the dataset holds, not what the project holds. a subfolder is accepted when it holds pinned files. when an archive's files are not known yet, a sync may come before the check. a path the dataset does not hold, from a real run:

```text
error: iris has no file or folder called not-a-file.csv
  iris holds Iris.csv, README.md and database.sqlite
  help: name a path it holds, such as `fetchloom path iris/Iris.csv`
```

`path` returns a location, not proof of intact bytes: sync may have kept an edited file there. use [status](status.md) when correctness depends on unchanged inputs.

`--locked` never writes data.lock, so scripts and CI pass it and run `fetchloom lock` themselves. a dataset already in place answers as usual, since nothing needs writing; one that needs a sync first, such as a dataset added to data.toml by hand, fails with `lock.outdated` instead of being pinned and downloaded. only the named dataset's entry counts, so another dataset's stale entry stops nothing.

## exit codes

0 returns a location. an unknown dataset, absent member or dataset whose archives contain no regular files exits 2 with `usage`. a missing lock, or a stale entry for the dataset, under `--locked` exits 1 with `lock.outdated`. implicit sync can also return its download, integrity or local [exit codes](../reference/exit-codes.md).

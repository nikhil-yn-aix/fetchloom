# list

`list` shows the datasets in a project, their references, how many files each puts in its folder and their size, the folders and licenses. with a name it shows that dataset's files.

```sh
fetchloom list
fetchloom list iris
fetchloom list --json
```

## usage

```sh
fetchloom list [options] [name]
```

`name` is a dataset in `data.toml`. an unknown name is a usage error, exit 2, that suggests the closest name. there are no command-specific options. [global options](README.md#global-options) apply.

## what it reads

`list` reads `data.toml`, `data.lock` and what the cache recorded of each archive, prints a table on stdout, ordered by dataset name, and changes nothing. it does not download, sync or look at placed files. the license comes last, cut at 32 characters; on a narrow terminal the reference is left out first, then the license.

a dataset without a lock entry says it is not locked yet. a [recipe](../recipes.md) shows what it is made from in place of a reference, and says it is not built yet until data.lock pins its result. a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read) shows `not placed` in place of its folder. counts and sizes are of the files a dataset puts in its folder: an archive counts as the files it unpacked to here, less those a [members table](../reference/data-toml.md#files-inside-archives) leaves out, and as itself until a sync unpacks it. a stale lock can still supply these values; [lock --check](lock.md) checks the lock and [status](status.md) the placed files.

with a name, `list` prints those files on stdout, one per line: the path inside the dataset folder, then its size, the sizes lined up on the right. at a terminal, a line on stderr comes first that names the dataset with its title, how many files and bytes it holds, and its license and DOI when the lock has them:

```text
  iris · 2 files · 4.5 KiB · license cc0-1.0
Iris.csv   4.4 KiB
iris.data     15 B
```

a dataset without a lock entry prints no files and says to run `fetchloom sync`.

JSON returns `result.datasets`, with each dataset's `name`, `ref`, `from`, `resolved`, `files`, `bytes`, `folder`, `placed`, `title`, `license`, `doi`, `access` and `locked_files`, each locked file's `path`, `size`, `sha256` and `url`; [JSON output](../reference/json-output.md#command-results) gives each field. `access` says who may read the data, as its source said when it was pinned: `public`, `login` for an account, `terms` for accepted terms, or `unknown`. it is a hint before sharing the files, not a license check. `url` is null for a recipe's result and for a link holding a space or a control character, which is never shown.

## exit codes

0 means the listing completed, including an empty project. ctrl-c interrupts a listing with exit 130, including while its lock is being read or its answer is being built. missing or invalid project files use the [shared exit codes](../reference/exit-codes.md).

# list

`list` shows the datasets in a project, their references, locked sizes and destination folders. with a name it shows that dataset's files.

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

`list` finds the nearest `data.toml` and reads it with `data.lock` if present. it prints a table on stdout, ordered by dataset name, and changes nothing. it does not download, sync or inspect placed files.

a dataset without a lock entry says it is not locked yet. counts and bytes come from locked source files, so an archive counts as one packed file, not its extracted members. a stale lock can still supply these values; use [lock --check](lock.md) to check freshness and [status](status.md) to inspect local files.

with a name, `list` prints that dataset's locked files on stdout, one per line: the path inside the dataset folder, then its size. at a terminal, its title, license and DOI come first on stderr when the lock has them. a dataset without a lock entry prints no files and says to run `fetchloom sync`.

JSON returns `result.datasets`, an array of `name`, `ref`, `resolved`, `files`, `bytes`, `folder`, `title`, `license`, `doi` and `locked_files` records. `locked_files` lists each locked file as `path` and `size`; `files` stays the count. `resolved`, `files`, `bytes` and `locked_files` are null without a lock entry, and `title`, `license` and `doi` are null when the lock does not have them. with a name the array holds only that dataset. `folder` is project-relative and respects `data_dir` and the dataset's `path` setting. an empty manifest returns an empty array.

## exit codes

0 means the listing completed, including an empty project. ctrl-c interrupts a listing with exit 130, including while its lock is being read or its answer is being built. missing or invalid project files use the [shared exit codes](../reference/exit-codes.md).

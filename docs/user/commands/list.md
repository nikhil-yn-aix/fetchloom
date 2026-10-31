# list

`list` shows the datasets in a project, their references, locked sizes and destination folders.

```sh
fetchloom list
fetchloom list --json
```

## usage

```sh
fetchloom list [options]
```

there are no command-specific options. [global options](README.md#global-options) apply.

## what it reads

`list` finds the nearest `data.toml` and reads it with `data.lock` if present. it prints a table on stdout, ordered by dataset name, and changes nothing. it does not download, sync or inspect placed files.

a dataset without a lock entry says it is not locked yet. counts and bytes come from locked source files, so an archive counts as one packed file, not its extracted members. a stale lock can still supply these values; use [lock --check](lock.md) to check freshness and [status](status.md) to inspect local files.

JSON returns `result.datasets`, an array of `name`, `ref`, `resolved`, `files`, `bytes` and `folder` records. `resolved`, `files` and `bytes` are null without a lock entry. `folder` is project-relative and respects `data_dir` and the dataset's `path` setting. an empty manifest returns an empty array.

## exit codes

0 means the listing completed, including an empty project. missing or invalid project files use the [shared exit codes](../reference/exit-codes.md).

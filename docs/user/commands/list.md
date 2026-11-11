# list

`list` shows the datasets in a project, their references, locked sizes, destination folders and licenses. with a name it shows that dataset's files.

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

`list` finds the nearest `data.toml` and reads it with `data.lock` if present. it prints a table on stdout, ordered by dataset name, and changes nothing. the license the source states comes last, cut at 32 characters; on a terminal too narrow for the whole table the reference is left out first, then the license. it does not download, sync or inspect placed files.

a dataset without a lock entry says it is not locked yet. a [recipe](../recipes.md) shows what it is made from in place of a reference, and says it is not built yet until data.lock pins its result. a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read) shows `not placed` in place of its folder. counts and bytes come from locked source files, so an archive counts as one packed file, not its extracted members. a stale lock can still supply these values; use [lock --check](lock.md) to check freshness and [status](status.md) to inspect local files.

with a name, `list` prints that dataset's locked files on stdout, one per line: the path inside the dataset folder, then its size. at a terminal, its title, license and DOI come first on stderr when the lock has them. a dataset without a lock entry prints no files and says to run `fetchloom sync`.

JSON returns `result.datasets`, an array of `name`, `ref`, `from`, `resolved`, `files`, `bytes`, `folder`, `placed`, `title`, `license`, `doi`, `access` and `locked_files` records. `locked_files` lists each locked file as `path`, `size`, `sha256` and `url`, the link it downloads from; `files` stays the count. `url` is null for a link that holds a login, a secret such as `token=`, a space or a control character, which fetchloom never pins itself, and is never shown; [export](export.md) leaves such links out the same way. `access` says who may read the data, as its source said when it was pinned: `public`, `login` for an account, `terms` for accepted terms, or `unknown` when the source said nothing. it is a hint before sharing the files, not a licence check. `ref` is null for a recipe, and `from`, the datasets a recipe is made from, is null for a fetched dataset. a recipe's pinned result gives `files`, `bytes` and `locked_files`, with null `url`s. `resolved`, `files`, `bytes`, `access` and `locked_files` are null without a lock entry, and `title`, `license` and `doi` are null when the lock does not have them. with a name the array holds only that dataset. `folder` is project-relative and respects `data_dir` and the dataset's `path` setting. `placed` is false for a dataset with `place = false`, whose `folder` is where its files would land and holds none of them. an empty manifest returns an empty array.

## exit codes

0 means the listing completed, including an empty project. ctrl-c interrupts a listing with exit 130, including while its lock is being read or its answer is being built. missing or invalid project files use the [shared exit codes](../reference/exit-codes.md).

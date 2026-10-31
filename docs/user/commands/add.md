# add

`add` puts a dataset in your manifest, pins its files and syncs the whole project.

```sh
fetchloom add iris
fetchloom add zenodo:3608990
fetchloom add zenodo:3608990 --as flowers --select "*.csv"
```

## usage

```sh
fetchloom add [options] <link>
```

| option | effect |
|---|---|
| `--as <name>` | choose the dataset name and its default folder under `data/` |
| `--select <glob>` | keep files matching any supplied pattern; repeat for more patterns |
| `--exclude <glob>` | drop matching files; repeat for more exclusions |
| `--no-extract` | keep archives packed and record `extract = false` |

[global options](README.md#global-options) also apply. [sources](../sources/README.md) lists accepted links and references. plain words use [catalog search and selection](search.md#choosing-a-dataset); quote several words, as in `fetchloom add "fashion mnist"`. the manifest stores the chosen canonical id, not the search words.

## what happens

1. find the nearest project manifest, or create one in the current folder.
2. resolve the reference, filter its listed files and choose a dataset name.
3. write the entry in `data.toml` and pin selected files in `data.lock`.
4. sync the whole project, using cached bytes or downloading missing files.
5. in a git repository, add the data folder to `.gitignore` if needed.

stderr excerpt for a single-file addition on a filesystem that supports links:

```text
     Locked iris · 1 file in data.lock
     Placed data/iris · 1 file linked from the cache
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

commit `data.toml` and `data.lock`, not `data/`. names follow the [manifest rules](../reference/data-toml.md). a zenodo file-page link selects that file unless you supply `--select`.

adding an existing reference with identical filters and extraction settings syncs it without another entry. different settings fail unless you supply an unused `--as` name. edit `data.toml` to change an existing entry.

if a new addition fails to sync, fetchloom attempts to restore both `data.toml` and `data.lock`, including their absence before the command. if restoration fails, the error supplies recovery steps and the previous contents as JSON strings to decode into the named files. cached downloads and files already placed are not rolled back.

## generated names

the [dataset naming rules](../reference/data-toml.md#dataset-names) choose a name from a file, title or reference and handle collisions. use `--as` to choose your own.

## exit codes

0 means the command succeeded, including when sync kept local conflicts. inspect `result.placement.kept` in JSON. 1 means download approval was missing; 2 means invalid usage. source, network, integrity, access and local failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a pattern matches nothing | it targets an archive member or a different source path | inspect listed paths and [pattern rules](../reference/data-toml.md#patterns) |
| a shell expands a pattern | the glob was not quoted | use `--select "**/*.csv"` |
| the same reference rejects new filters | the existing entry has different settings | edit its manifest entry, or use an unused `--as` name |
| adding fails for inline `datasets` | `add` cannot extend an inline table | use `[datasets.name]` tables |
| a large download stops in CI | it needs approval | check the size, then pass `--yes` if approved |

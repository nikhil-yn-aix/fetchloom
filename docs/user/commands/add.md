# add

`add` puts a dataset in your manifest, pins its files and syncs the whole project.

```sh
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

[global options](README.md#global-options) also apply. [sources](../sources/README.md) lists accepted links and references.

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

if a new addition fails to sync, fetchloom attempts to restore the previous manifest. if restoration fails too, the error includes a backup as a JSON string to decode into `data.toml`, or names the entry to remove. cache, lock and placement changes are not all rolled back.

## generated names

without `--as`, a single file supplies a name of at most 40 characters. otherwise a title supplies at most its first four words and 32 characters, dropping trailing words such as `and`, `of` and `the`.

when neither supplies a name, fetchloom uses the reference's last part containing a letter: `hf:datasets/scikit-learn/iris` gives `iris`, while `zenodo:123` gives `zenodo-123`.

a taken generated name gets `-2`, `-3` and so on. a taken explicit `--as` name is refused.

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

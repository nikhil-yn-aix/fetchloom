# remove

`remove` deletes a dataset entry and its unchanged managed files while keeping local edits.

```sh
fetchloom remove iris
fetchloom remove iris --dry-run
```

## usage

```sh
fetchloom remove [options] <name>
```

| option | effect |
|---|---|
| `-n`, `--dry-run` | plan removal without applying project or cache changes |

[global options](README.md#global-options) also apply. the argument is a dataset name from `data.toml`, not a source reference.

## what happens

1. open the cache and plan removal before changing project metadata.
2. remove the dataset from `data.toml` and `data.lock`.
3. delete unchanged managed files belonging to it. in a folder it shares with other datasets, or in the data folder itself for `path = "."`, only its own files go, since each placed file belongs to one dataset.
4. leave edited files and files fetchloom did not place where they are, and warn about each edited file and each folder those keep.

a dataset a [recipe](../recipes.md) is made from is not removed while any recipe reads it: `remove` exits 2, naming every recipe that reads it, as `clean and count are made from iris`, and changes nothing. removing a recipe drops its pin from `data.lock` too.

nothing is downloaded. an existing file adopted because it matched the lock counts as managed. `remove` can delete it if unchanged, and then warns, since you put it there: `warning: removed data/iris/iris.csv, a file that was already there when fetchloom adopted it`; `--dry-run` warns the same way first. removal compares the saved size and modification time, so touching a file can keep it even when its bytes still match the pin and status reports no edit.

a dataset folder that still holds files fetchloom did not place, such as your own outputs or the `.changed` copies sync kept, stays with them, and remove names it (stderr), with `-v` naming each file:

```text
warning: data/pets still holds 2 files fetchloom did not place, such as data/pets/notes.txt, so the folder stays
  help: `fetchloom status` lists them as extra; delete them when you no longer need them
```

removing a table also removes its attached comments and strips leading blank lines. other manifest entries remain. commit the manifest and lock after removal.

if deleting files fails after metadata was written, fix the reported error and run `fetchloom sync` to finish removal. removing a project entry does not immediately clear its bytes from the shared cache; [cache gc](cache.md#gc) handles unneeded objects.

## dry run

`remove --dry-run`, or `-n`, makes the same plan and stops before writing: `data.toml`, `data.lock`, `data/` and the cache stay as they are, and nothing is asked of the network. explicit `FETCHLOOM_TRACE` output is still created or truncated. `-v` names each file it would delete. stderr for a dataset `a` with one placed file, matching a test snapshot:

```text
Would remove a from data.toml and data.lock
Would remove 1 file from data/ that no dataset needs
         Ok dry run, nothing was changed
```

edited files that a real removal would keep, and the folders that keep files fetchloom did not place, are named in warnings, as they are without `--dry-run`. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## exit codes

0 means removal completed, even when edited files remain. invalid usage exits 2; local or invalid-project failures use 50. [exit codes](../reference/exit-codes.md) lists all kinds.

## common mistakes

| symptom | cause | fix |
|---|---|---|
| some dataset files remain | they were edited, or fetchloom did not place them; the warnings name them | inspect and move or delete them yourself |
| cached bytes remain | other projects may use them, and gc has an age rule | run `cache gc` when cleanup is needed |
| metadata changed but a file could not be deleted | permissions or an open handle blocked deletion | fix that error, then sync |
| an identical file that was already there was removed, with a warning | matching adopted files are managed | keep independent work outside managed inputs |

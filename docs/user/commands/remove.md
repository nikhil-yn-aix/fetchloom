# remove

`remove` takes a dataset out of `data.toml` and `data.lock` and deletes the unchanged files fetchloom placed for it, keeping your edits.

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

1. plan the removal before changing anything.
2. remove the dataset from `data.toml` and `data.lock`.
3. delete the unchanged files fetchloom placed for it. in a folder it shares with other datasets, or in the data folder itself for `path = "."`, only its own files go.
4. leave edited files and files fetchloom did not place where they are, and warn about each edited file and each folder those keep.

a dataset a [recipe](../recipes.md) is made from is not removed while any recipe reads it: `remove` exits 2, naming every recipe that reads it, as `clean and count are made from iris`, and changes nothing. removing a recipe drops its pin from `data.lock` too.

nothing is downloaded. a file sync [adopted](sync.md#already-in-place) counts as placed, so `remove` deletes it if unchanged, and warns, since you put it there: `warning: removed data/iris/iris.csv, a file that was already there when fetchloom adopted it`. removal compares the saved size and modification time, so touching a file keeps it even when its bytes still match the pin.

a dataset folder that still holds files fetchloom did not place, such as your own outputs or the `.changed` copies sync kept, stays with them, and remove names it (stderr), with `-v` naming each file:

```text
warning: data/pets still holds 2 files fetchloom did not place, such as data/pets/notes.txt, so the folder stays
  help: `fetchloom status` lists them as extra; delete them when you no longer need them
```

removing a table also removes its comments; other entries stay. commit `data.toml` and `data.lock` afterwards.

if deleting files fails after `data.toml` and `data.lock` were written, fix the reported error and run `fetchloom sync` to finish. the bytes stay in the shared cache until [cache gc](cache.md#gc) frees them.

## dry run

`remove --dry-run`, or `-n`, makes the same plan and stops before writing: `data.toml`, `data.lock`, `data/` and the cache stay as they are. `-v` names each file it would delete. stderr for a dataset `a` with one placed file, matching a test snapshot:

```text
      Would remove a from data.toml and data.lock
      Would remove 1 file from data/ that no dataset needs
         Ok dry run, nothing was changed
```

the files a real removal would keep, and the folders they keep, are named in warnings, as without `--dry-run`. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## exit codes

0 means removal completed, even when edited files remain. invalid usage exits 2, as does a dataset a recipe reads; local or invalid-project failures use 50. `remove` with no name exits 2 too, and lists the datasets it could remove:

```text
error: remove needs <name>
  datasets: iris, wine
  help: name one of them, as in `fetchloom remove iris`
```

[exit codes](../reference/exit-codes.md) lists all kinds.

## common mistakes

| symptom | cause | fix |
|---|---|---|
| some dataset files remain | they were edited, or fetchloom did not place them; the warnings name them | inspect and move or delete them yourself |
| cached bytes remain | other projects may use them, and gc has an age rule | run `cache gc` when cleanup is needed |
| metadata changed but a file could not be deleted | permissions or an open handle blocked deletion | fix that error, then sync |
| an identical file that was already there was removed, with a warning | sync adopted it as placed | keep your own files outside the data folder |

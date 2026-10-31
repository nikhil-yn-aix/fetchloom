# remove

`remove` deletes a dataset entry and its unchanged managed files while keeping local edits.

```sh
fetchloom remove iris
```

## usage

```sh
fetchloom remove [options] <name>
```

there are no command-specific options. [global options](README.md#global-options) apply. the argument is a dataset name from `data.toml`, not a source reference.

## what happens

1. open the cache and plan removal before changing project metadata.
2. remove the dataset from `data.toml` and `data.lock`.
3. delete unchanged managed files belonging to it.
4. leave edited and unrelated extra files in place, warning about edits.

nothing is downloaded. an existing file adopted because it matched the lock counts as managed. `remove` can delete it if unchanged.

stdout excerpt from `fetchloom --help`:

```text
  remove  remove a dataset and the unchanged files fetchloom placed for it
```

removing a table also removes its attached comments and strips leading blank lines. other manifest entries remain. commit the manifest and lock after removal.

if deleting files fails after metadata was written, fix the reported error and run `fetchloom sync` to finish removal. removing a project entry does not immediately clear its bytes from the shared cache; [cache gc](cache.md#gc) handles unneeded objects.

## exit codes

0 means removal completed, even when edited files remain. invalid usage exits 2; local or invalid-project failures use 50. [exit codes](../reference/exit-codes.md) lists all kinds.

## common mistakes

| symptom | cause | fix |
|---|---|---|
| some dataset files remain | they were edited or were unrelated extras | inspect and move or delete them yourself |
| cached bytes remain | other projects may use them, and gc has an age rule | run `cache gc` when cleanup is needed |
| metadata changed but a file could not be deleted | permissions or an open handle blocked deletion | fix that error, then sync |
| a previously existing identical file was removed | matching adopted files are managed | keep independent work outside managed inputs |

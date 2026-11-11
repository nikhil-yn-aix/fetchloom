# drop

`drop` leaves files out of their dataset for good, such as bad samples, by writing patterns that leave them out into `data.toml`. it asks no server.

```sh
fetchloom drop data/pets/images/dog_010_corrupt.png
fetchloom drop data/pets/images/blurry --dry-run
```

## usage

```sh
fetchloom drop [options] <path>...
```

| option | effect |
|---|---|
| `-n`, `--dry-run` | show what would change on disk without changing it |

[global options](README.md#global-options) also apply. each path is a file or a folder in the data folder, from the current folder, or from the folder `-C` names. it may name a file you deleted already. a folder stands for every file data.lock places under it. a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read) has no files in the data folder, so its files are named by the path they would have there, as in [raw data only recipes read](#raw-data-only-recipes-read). on windows `\` separates folders too, and names are compared the way a disk that ignores case compares them.

## what happens

1. find the files data.lock places at each path, from what this machine recorded about each archive.
2. write a pattern for each file into `data.toml`, escaped so it matches that file alone:
   - a file inside an archive gets its path inside the archive, in the archive's [members table](../reference/data-toml.md#files-inside-archives). in a dataset that is one archive, that path starts with the folder all its files share, which placing leaves out.
   - a file the source lists gets its path in the dataset's `exclude`, and leaves `data.lock` too. the pin is rewritten as relocking the same listing would pin it, so `sync --locked` takes it and nothing is resolved again.
   - a folder gets one pattern, such as `pets-v1/images/blurry/**`, when that pattern leaves out exactly the files under it, and one pattern for each file otherwise.
   - leaving out every file an archive keeps leaves out the archive itself, in the dataset's `exclude`, and its members table goes.
3. delete the unchanged files it leaves out, as [`remove`](remove.md) does. a file someone changed stays, with a warning. a dataset with `place = false` has none to delete.

stderr when the curator drops a broken image from a dataset that is one zip, `pets.zip`:

```text
   Excluded 1 file of pets.zip in data.toml
    Removed 1 file no dataset needs
```

`data.toml` then reads:

```toml
[datasets.pets]
ref = "https://example.org/pets.zip"

[datasets.pets.members."pets.zip"]
exclude = ["pets-v1/images/dog_010_corrupt.png"]
```

a file of `train.zip` stays left out of `train.zip` alone: in a dataset where `test.zip` holds a file at the same path, that one stays.

commit `data.toml`, and `data.lock` when the drop changed it; the closing `next:` line names them at a terminal. a teammate's next `fetchloom sync` deletes the file from their data folder too, and downloads nothing. the archive stays in the cache whole until [`cache gc`](cache.md#gc) frees what no project keeps.

an `exclude` written by hand in a dataset does not reach a file inside an archive. the sync that lists the dataset's source again, as the first one after the exclude is written, warns about one that names such a file and matches no file the source lists, and gives the drop command that leaves it out:

```text
warning: exclude "data/train.csv" in datasets.dataset names a file inside dataset.zip, which exclude does not reach
  help: run `fetchloom drop data/dataset/data/train.csv`
```

## raw data only recipes read

a dataset with `place = false` keeps its files out of the data folder, but a [recipe](../recipes.md) still reads them, so a bad sample there still reaches the result. name the file by the path it would have in the data folder, which is where it lands in the folder a build's step reads too. stderr when a researcher drops a black frame from `images`, the zip `shapes.zip` that the recipe `thumbs` reads:

```text
$ fetchloom drop data/images/images/img_009.png
   Excluded 1 file of shapes.zip in data.toml
```

nothing is deleted, since nothing of `images` is placed. the pattern changes what `thumbs` is made from, so `fetchloom build --dry-run` then says `Would run thumbs · images changed`, and the build makes it without the file.

drop leaves files out; it never renames or moves one. to bring a file back, delete its pattern from `data.toml` and run `fetchloom sync`: a file inside an archive returns from the cache, a mirror or the source, and a file the source lists is pinned again, which asks the source.

## refusals

each refusal changes nothing.

| refusal | exit | what to do |
|---|---|---|
| a path outside the data folder, or one that names no file of a dataset, which the error says | 2, `usage` | check the path; delete a file of your own yourself |
| a file left out already | 2, `usage` | nothing: the error names the pattern in data.toml that leaves it out. delete that pattern and run `fetchloom sync` to bring the file back |
| a file inside an archive this machine never unpacked | 2, `usage` | run `fetchloom sync` first, so fetchloom knows what the archive holds |
| every file of a dataset | 2, `usage` | run [`fetchloom remove <name>`](remove.md) to take the dataset out |
| a file of a [recipe's](../recipes.md) result | 2, `usage` | choose its files inside the recipe's step |
| a drop that leaves one archive the only file of its dataset | 2, `usage` | a dataset that is one archive places its files straight in its folder, so the archive's files would move. keep the files, or write the exclude by hand and run `fetchloom sync` |
| a file the source lists, while data.toml changed the dataset since data.lock was written | 1, `lock.outdated` | run `fetchloom sync`, then drop again |
| a dataset or a members table written inline | 50, `project.invalid` | write it as a table, as `[datasets.pets]` |

## dry run

`drop --dry-run`, or `-n`, makes the same plan and stops before writing: `data.toml`, `data.lock` and `data/` stay as they are. `-v` names each file. stderr for the image above:

```text
Would exclude 1 file of pets.zip in data.toml
Would remove 1 file no dataset needs
         Ok dry run, nothing was changed
```

## JSON

`--json` returns `result.project`; `result.dropped`, one `{path, dataset, archive, pattern}` for each file left out, by path, with `archive` null for a file the source lists; `result.lock_changed`; and `result.placement`, as [JSON output](../reference/json-output.md#command-results) describes. a dry run adds `dry_run` and `changes`.

## exit codes

0 means every file is left out, even when a changed file stays. [exit codes](../reference/exit-codes.md) lists every kind.

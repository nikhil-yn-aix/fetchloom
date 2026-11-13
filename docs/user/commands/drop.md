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

[global options](README.md#global-options) also apply. each path is a file or a folder in the data folder, from the current folder or the folder `-C` names, and may name a file you deleted already. a folder stands for every file data.lock places under it. a path is taken as written: `*`, `?` and `[` are part of a name, never a pattern, as [refusals](#refusals) shows. a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read) has no files in the data folder, so its files are named by the path they would have there. on windows `\` separates folders too, and names are compared ignoring case.

## what happens

1. find the files data.lock places at each path, from what this machine recorded about each archive.
2. write a pattern for each file into `data.toml`, escaped so it matches that file alone:
   - a file inside an archive gets its path inside the archive, in the archive's [members table](../reference/data-toml.md#files-inside-archives). in a dataset that is one archive, that path starts with the folder all its files share, which placing leaves out.
   - an archive that a table under a [glob key](../reference/data-toml.md#one-table-for-many-shards) covers gets a table of its own, copied from the glob's, before the new pattern goes in, so the pattern leaves out a file of that archive alone and the archive keeps what the glob chose.
   - a file the source lists gets its path in the dataset's `exclude`, and leaves `data.lock` too, rewritten as relocking the same listing would, so `sync --locked` takes it and nothing is resolved again.
   - a folder gets one pattern, such as `pets-v1/images/blurry/**` in a members table or `sub/**` in the dataset's `exclude`, when that pattern leaves out exactly the files under it, and one pattern for each file otherwise. files named one by one, as a shell expands `images/*.png`, get one pattern each, so a file that lands in that folder later stays.
   - leaving out every file an archive keeps leaves out the archive itself, in the dataset's `exclude`, and its members table goes, as does a glob key's table that then matches no archive.
3. delete the unchanged files it leaves out, as [`remove`](remove.md) does. a file someone changed stays, with a warning. a dataset with `place = false` has none to delete.

stderr when the curator drops a broken image from a dataset that is one zip, `pets.zip`:

```text
   Excluded 1 file of pets.zip in data.toml
    Removed 1 file from data/ that no dataset needs
```

`data.toml` then reads:

```toml
[datasets.pets]
ref = "https://example.org/pets.zip"

[datasets.pets.members."pets.zip"]
exclude = ["pets-v1/images/dog_010_corrupt.png"]
```

a file of `train.zip` stays left out of `train.zip` alone: in a dataset where `test.zip` holds a file at the same path, that one stays.

with ten-sample shards whose files have the same names in each, and `select = ["*.wav", "*.txt"]` under `[datasets.shards.members."shard-*.tar"]`, `fetchloom drop data/shards/shard-000001/00000005.wav` says on stderr:

```text
   Excluded 1 file of shard-000001.tar in data.toml
  hint: shard-000001.tar now has its own members table, copied from members."shard-*.tar", so later changes to that table do not reach it
    Removed 1 file from data/ that no dataset needs
```

and adds to `data.toml`:

```toml
[datasets.shards.members."shard-000001.tar"]
select = ["*.wav", "*.txt"]
exclude = ["00000005.wav"]
```

the `00000005.wav` of `shard-000000.tar` and `shard-000002.tar` stays, and so does the choice of the other shards. edit both tables to change what every shard keeps.

commit `data.toml`, and `data.lock` when the drop changed it; the closing `next:` line names them, and the build first when a [recipe](../recipes.md) is made from the dataset. a teammate's next `fetchloom sync` deletes the file from their data folder too, and downloads nothing. the archive stays in the cache whole until [`cache gc`](cache.md#gc) frees it.

an `exclude` written by hand does not reach a file inside an archive. the next sync that lists the source warns about one that names such a file, and gives the drop that leaves it out:

```text
warning: exclude "data/train.csv" in datasets.dataset names a file inside dataset.zip, which exclude does not reach
  help: run `fetchloom drop data/dataset/data/train.csv`
```

## raw data only recipes read

a dataset with `place = false` keeps its files out of the data folder, but a [recipe](../recipes.md) still reads them, so a bad sample there still reaches the result. name the file by the path it would have in the data folder, which is where it lands in the folder a build's step reads too. stderr when a researcher drops a black frame from `images`, the zip `shapes.zip` that the recipe `thumbs` reads:

```text
$ fetchloom drop data/images/images/img_009.png
   Excluded 1 file of shapes.zip in data.toml
  next: run `fetchloom build thumbs`, then commit data.toml, data.lock and data.lock.d
```

nothing is deleted, since nothing of `images` is placed. data.lock still pins the result `thumbs` made with the file, so the `next:` line names the build, and `fetchloom build --dry-run` says `Would run thumbs · images changed`.

drop never renames or moves a file. to bring one back, delete its pattern from `data.toml` and run `fetchloom sync`.

## refusals

each refusal changes nothing.

| refusal | exit | what to do |
|---|---|---|
| a path outside the data folder, or one that names no file of a dataset, which the error says | 2, `usage` | check the path: for a path one name off a file or folder drop knows, the help gives the command it likely meant, as `` did you mean `fetchloom drop data/iris/iris.csv`? ``. delete a file of your own yourself |
| a pattern, such as `'data/dot/*.wav'` in quotes | 2, `usage` | add the pattern the help gives to `exclude` in data.toml and run `fetchloom sync`, as below |
| a file left out already | 2, `usage` | nothing: the error names the pattern in data.toml that leaves it out. delete that pattern and run `fetchloom sync` to bring the file back |
| a file inside an archive this machine never unpacked | 2, `usage` | run `fetchloom sync` first, so fetchloom knows what the archive holds |
| every file of a dataset | 2, `usage` | run [`fetchloom remove <name>`](remove.md) to take the dataset out |
| a file of a [recipe's](../recipes.md) result | 2, `usage` | choose its files inside the recipe's step |
| a drop that leaves one archive the only file of its dataset | 2, `usage` | a dataset that is one archive places its files straight in its folder, so the archive's files would move. keep the files, or write the exclude by hand and run `fetchloom sync` |
| a file the source lists, while data.toml changed the dataset since data.lock was written | 1, `lock.outdated` | run `fetchloom sync`, then drop again |
| a dataset or a members table written inline | 50, `project.invalid` | write it as a table, as `[datasets.pets]` |

stderr for a pattern in quotes, where the dataset `dot` is the tar `dot-000000.tar`:

```text
$ fetchloom drop 'data/dot/*.wav'
error: data/dot/*.wav names no file of dot
  drop takes paths, not patterns, so * is part of the name it looks for
  help: to leave out every file a pattern matches, add "*.wav" to exclude in members."dot-000000.tar" of datasets.dot in data.toml by hand, then run `fetchloom sync`
```

the pattern reads paths inside the archive, so in a dataset that is one archive whose files share a top folder, it starts with that folder. without the quotes, the shell names each `.wav` file in `data/dot`, and drop leaves out each with a pattern of its own.

## dry run

`drop --dry-run`, or `-n`, makes the same plan and stops before writing: `data.toml`, `data.lock` and `data/` stay as they are. `-v` names each file. stderr for the image above:

```text
      Would exclude 1 file of pets.zip in data.toml
      Would remove 1 file from data/ that no dataset needs
         Ok dry run, nothing was changed
```

## stopping

drop plans everything before it writes, which in a dataset of many files takes a while, so a line says `Planning the drop of data/tiny/d050`, or `of 50 paths` for several. ctrl-c while it plans stops it before it writes anything, exit 130. stderr:

```text
   Stopping (press ctrl-c again to quit at once)
    Stopped at your request, so nothing changed
```

once drop starts writing `data.toml`, a ctrl-c lets it finish, as [interruptions](../concepts.md#interruptions) explains.

## JSON

`--json` returns `result.project`, `result.dropped`, one `{path, dataset, archive, pattern}` for each file left out, with `archive` null for a file the source lists, `result.copied`, one `{dataset, archive, from}` for each archive given its own members table, `result.lock_changed` and `result.placement`, as [JSON output](../reference/json-output.md#command-results) describes; a dry run adds `dry_run` and `changes`.

## exit codes

0 means every file is left out, even when a changed file stays. [exit codes](../reference/exit-codes.md) lists every kind.

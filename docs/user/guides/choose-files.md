# Choose which files to keep

## leaving out bad samples

deleting a file from the data folder does not last: data.lock still pins it, so the next sync puts it back and says so:

```text
$ rm data/uci/iris.names
$ fetchloom sync
    Placing 1 file
     Placed data/uci · 1 file linked to the cache
   Put back data/uci/iris.names, which was deleted or moved, since data.lock pins it
  hint: to leave a file out for good, run `fetchloom exclude data/uci/iris.names`
     Synced 2 datasets into data
```

`fetchloom exclude` leaves a file out for good by writing a pattern into data.toml, so every clone leaves it out too. it works for files inside archives as well:

```text
$ fetchloom exclude data/uci/bezdekIris.data
   Excluded 1 file of iris.zip in data.toml
    Removed 1 file from data/ that no dataset needs
```

data.toml then holds:

```toml
[datasets.uci]
ref = "https://archive.ics.uci.edu/static/public/53/iris.zip"

[datasets.uci.members."iris.zip"]
exclude = ["bezdekIris.data"]
```

[exclude](../commands/exclude.md) covers the patterns it writes and how to bring a file back.

## choosing files

`--select` and `--exclude` match the whole path a source lists, such as `all/train-0.parquet`, before anything downloads. `*` and `?` stop at `/`, so `*train*` matches only files at the dataset's top, and `**/*train*` matches them in every folder. quote patterns so your shell passes them unchanged. each select pattern must match at least one path before exclusions, and some files must remain. an exclude pattern that matches no path leaves nothing out, so it gets a `source.exclude_unmatched` warning, with the `**/` form when that would match. patterns do not reach members inside an archive; a [members table](../reference/data-toml.md#files-inside-archives) in data.toml does, and [`fetchloom exclude`](../commands/exclude.md) writes one.

the limit on how many files a dataset lists applies before filtering, and filters cannot get around a source's access checks, such as zenodo's, figshare's or dataverse's. a selected google docs entry is different: exclude it, and the fixed files around it still download.

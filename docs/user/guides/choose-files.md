# Choose which files to keep

## leaving out bad samples

deleting a file from the data folder does not last: data.lock still pins it, so the next sync puts it back and says so:

```text
$ rm data/uci/iris.names
$ fetchloom sync
     Placed data/uci · 1 file linked to the cache
warning: put back data/uci/iris.names, which was deleted or moved, since data.lock pins it
  help: to leave a file out for good, run `fetchloom drop data/uci/iris.names`; keep files of your own in a folder outside data/
     Synced 2 datasets into data
```

`fetchloom drop` leaves a file out for good by writing a pattern into data.toml, so every clone leaves it out too. it works for files inside archives as well:

```text
$ fetchloom drop data/uci/bezdekIris.data
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

[drop](../commands/drop.md) covers the patterns it writes and how to bring a file back.

## choosing files

`--select` and `--exclude` match the paths a source lists, before anything downloads. quote them so your shell passes them unchanged. each select pattern must match at least one path before exclusions, and some files must remain. patterns do not reach members inside an archive; a [members table](../reference/data-toml.md#files-inside-archives) in data.toml does, and [`fetchloom drop`](../commands/drop.md) writes one.

the limit on how many files a dataset lists applies before filtering, and filters cannot get around a source's access checks, such as zenodo's, figshare's or dataverse's. a selected google docs entry is different: exclude it, and the fixed files around it still download.

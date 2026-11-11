# working with raw data

the files fetchloom places in the data folder are the raw inputs: pinned in data.lock, read-only, and put back by every sync. your own work goes beside them, and the few changes that belong to the inputs themselves, such as leaving out a bad sample, go into data.toml so every clone makes them too.

the examples come from one real run, in order, in a project holding `penguins`, one CSV file, and `uci`, the UCI iris zip, which unpacks to four files. output is stderr.

## where your own files go

write results, cleaned tables and features outside the data folder, such as in `processed/` beside it. fetchloom never looks there. code finds the inputs with `fetchloom path <name>`, so it keeps working when a dataset's folder changes.

a file written inside the data folder is kept too: `status` lists it as extra and `sync` leaves it there.

```text
$ fetchloom status
      Extra data/penguins/clean.csv
      Found 1 difference from data.lock
```

tools that write their own index or cache files beside the files they read leave such extras as well.

to record how a processed dataset is made from the raw one, so others can make it again, declare the step as a [recipe](recipes.md). once a recipe makes what your code reads, the raw dataset can leave the data folder: `place = false` on it keeps it pinned and in the cache for builds, as [keep only the result](recipes.md#keep-only-the-result) shows.

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
    Removed 1 file no dataset needs
```

data.toml then holds:

```toml
[datasets.uci]
ref = "https://archive.ics.uci.edu/static/public/53/iris.zip"

[datasets.uci.members."iris.zip"]
exclude = ["bezdekIris.data"]
```

[drop](commands/drop.md) covers the patterns it writes and how to bring a file back.

## editing raw files

placed files are read-only, so writing to one fails with permission denied. this protects the cache's copy: a hardlinked file is the same file in every project linked to it. for a dataset whose files you edit, set `copy = true` in data.toml:

```toml
[datasets.penguins]
copy = true
ref = "https://raw.githubusercontent.com/mwaskom/seaborn-data/master/penguins.csv"
```

`status` then lists each read-only file as outdated, and `sync` replaces it with a writable clone or copy of its own:

```text
$ fetchloom sync
     Placed data/penguins · 1 file copied, 13.2 KiB
     Synced 2 datasets into data
```

an edited file stays. `status` lists it as edited, and `sync` keeps it and says so:

```text
$ fetchloom sync
warning: data/penguins/penguins.csv has changed, so it stays
  help: `fetchloom sync --force` replaces it
     Synced 2 datasets into data, 1 file left unchanged; see the warnings above
```

the edit lives on this machine only. a teammate's sync places the pinned bytes, so a change others need belongs in a recipe, or, for a file that should not be there at all, in a drop.

## reorganizing

`path` in data.toml chooses each dataset's folder below the data folder. datasets may share one, and `path = "."` puts a dataset's files in the data folder itself, for scripts that read `data/train.csv`. `--select` and `--exclude` choose which source files a dataset keeps, and a members table chooses files inside an archive. [data.toml](reference/data-toml.md) has the rules.

a layout of your own, such as train and validation folders or renamed files, is your own work: a script can write it outside the data folder, as links to the raw files or as copies, and a [recipe](recipes.md) can make it a dataset of its own.

## freeing space

`fetchloom remove <name>` takes a dataset out of data.toml, data.lock and the data folder. a file you edited stays:

```text
$ fetchloom remove penguins
warning: data/penguins/penguins.csv is no longer needed, but it has changed, so it stays
  help: delete it if you do not need it
    Removed penguins
```

the cache keeps the bytes. `fetchloom cache gc` removes what no project needs once it is a day old, and `--now` removes it at once, here the CSV and the dropped member:

```text
$ fetchloom cache gc --now
    Removed 2 files · 17.6 KiB from the cache
    Cleaned the cache, keeping files needed by 1 project
```

[cache](commands/cache.md) says how gc decides what a project needs.

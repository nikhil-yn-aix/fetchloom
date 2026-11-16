# Edit input files

the files fetchloom places in the data folder are the raw inputs: pinned in data.lock, read-only, and put back by every sync. your own work goes beside them, and the few changes that belong to the inputs themselves, such as leaving out a bad sample, go into data.toml so every clone makes them too.

the examples come from one real run, in order, in a project holding `penguins`, one CSV file, and `uci`, the UCI iris zip, which unpacks to four files. output is stderr, except the rows `status` lists, which are stdout.

## where your own files go

write results, cleaned tables and features outside the data folder, such as in `processed/` beside it. fetchloom never looks there. code finds the inputs with `fetchloom path <name>`, so it keeps working when a dataset's folder changes.

a file written inside the data folder is kept too: `status` lists it as extra and `sync` leaves it there.

```text
$ fetchloom status
      Extra data/penguins/clean.csv
      Found 1 difference from data.lock
```

tools that write their own index or cache files beside the files they read leave such extras as well.

## editing raw files

placed files are read-only, so writing to one fails with permission denied. that protects the cache's copy, since a hardlinked file is the same file in every project linked to it. for a dataset whose files you edit, set `copy = true` in data.toml:

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

once data.lock pins other bytes for that file, as after `fetchloom lock --upgrade`, the warning adds that the file is an edit of an earlier version, not what data.lock pins now. a file whose time moved while its bytes stayed as fetchloom placed them, as `touch` or a restore from a backup leaves it, is no edit: `status` lists it as outdated, and `sync` replaces it with the new pin.

the edit lives on this machine only. a teammate's sync places the pinned bytes, so a change others need belongs in a fixed copy added as a dataset of its own, as `fetchloom add ./fixes/measure.csv --as m_fixed`, which is pinned like any other, or, for a file that should not be there at all, in an [exclude](../commands/exclude.md).

## reorganizing

`path` in data.toml chooses each dataset's folder below the data folder. datasets may share one, and `path = "."` puts a dataset's files in the data folder itself, for scripts that read `data/train.csv`. `--select` and `--exclude` choose which source files a dataset keeps, and a members table chooses files inside an archive. [data.toml](../reference/data-toml.md) has the rules.

a layout of your own, such as train and validation folders or renamed files, is your own work: a script can write it outside the data folder, as links to the raw files or as copies.

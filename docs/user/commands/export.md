# export

`export` writes the SHA-256 of each file in the data folder, or the link and SHA-256 of each file a project downloads, in forms other tools read.

```sh
fetchloom export > data.sha256
fetchloom export --format links iris
fetchloom export --format aria2 -o sources.aria2
fetchloom --json export
```

## usage

```sh
fetchloom export [options] [name]...
```

`name` is a dataset in `data.toml`; with none, every dataset is exported. an unknown name is a usage error, exit 2, that suggests the closest name.

| option | effect |
|---|---|
| `--format sha256` | the default: one `<sha256>  <path>` line for each file in the data folder, archive members included, as `sha256sum -c` reads them |
| `--format links` | one `<link> <sha256>` line for each file downloaded: a plain file, or an archive as one packed file |
| `--format aria2` | each file downloaded as an `aria2c -i` input file, with `out=` its place below the data folder and `checksum=sha-256=` |
| `-o`, `--output <file>` | write to `<file>` instead of stdout. a file already there is kept, and the command fails with `local.not_empty` |
| `--force` | with `-o`, replace a file already there. it is moved aside until the new one has its name |

[global options](README.md#global-options) also apply.

## what it writes

`export` reads `data.toml`, `data.lock` and what the cache recorded of each archive it unpacked. it asks no server anything, takes no project hold, makes no cache and reads no file in the data folder: the hashes are the pins in `data.lock`, so `sha256sum -c` finds a file someone edited. a [recipe](../recipes.md)'s result is in the `sha256` form like any placed dataset, from the list of its files `data.lock` keeps, and in no list of links, since nothing downloads it, which those forms [say](#what-it-leaves-out). its files reach the data folder when a build places them, or a sync from a cache or [mirror](../mirrors.md) that holds them, so in a fresh clone `sha256sum -c` cannot open them until `fetchloom build` has run. stdout of `sha256sum -c` in such a clone, after a sync that left `clean` out:

```text
data/clean/clean.csv: FAILED open or read
data/iris/iris.csv: OK
```

after `fetchloom build`, which ran the step and placed `clean`, both lines say `OK`.

the `sha256` paths start at the project's folder, so check them from there. from a real run with a zenodo dataset and a zip from UCI, which unpacks to four files:

```sh
$ fetchloom export -o data.sha256
   Exported 5 files to data.sha256
$ cat data.sha256
b1f0b46c3e27350cbf0f75fb3b7d36adcd997c443e764bf09255126feeb11f31  data/iris/iris.csv
fc3a4464fd2138ca649be5e0311270f5cf1f045c8e3ff1cd458f663dfe4f6790  data/uci/Index
0fed2a99db77ec533a62dc66894d3ec6df3b58b6a8f3cf4a6b47e4086b7f97dc  data/uci/bezdekIris.data
6f608b71a7317216319b4d27b4d9bc84e6abd734eda7872b71a458569e2656c0  data/uci/iris.data
71a09fb3ee237614cdb9c09d06e8ae1f610ce2a274c0d77272fea8a233e5eea3  data/uci/iris.names
$ sha256sum -c data.sha256
data/iris/iris.csv: OK
data/uci/Index: OK
data/uci/bezdekIris.data: OK
data/uci/iris.data: OK
data/uci/iris.names: OK
```

`links` and `aria2` name the files `data.lock` pins as downloaded, so an archive is one file and its members have no link: fetchloom never makes one up. files [imported](add.md#files-on-this-machine) from a folder on this machine have no link either, since no source holds them, so the two forms list the other files and say once on stderr how many they leave out, from a real run:

```text
warning: the list leaves out 2 files of flowers: they were imported from a folder on this machine, so no link holds them
  help: share them through a folder mirror, which `fetchloom push --to <folder>` fills
```

the zenodo and UCI project above as an aria2 input file, which aria2c 1.37 downloaded and checked; a file whose bytes differ from its `checksum=` line fails with exit 32:

```sh
$ fetchloom export --format aria2
https://zenodo.org/api/records/3608990/files/iris.csv/content
  out=iris/iris.csv
  checksum=sha-256=b1f0b46c3e27350cbf0f75fb3b7d36adcd997c443e764bf09255126feeb11f31
https://archive.ics.uci.edu/static/public/53/iris.zip
  out=uci/iris.zip
  checksum=sha-256=d11fe30213d36434a0879aab7cb00ce3c812eb7ba2495874438abff7b7b762e9
$ fetchloom export --format aria2 -o sources.aria2
   Exported 2 links to sources.aria2
$ aria2c -i sources.aria2 -d downloads
```

aria2c put the files in `downloads/iris/iris.csv` and `downloads/uci/iris.zip`.

`out=` puts each file in the folder its dataset is placed in, the one `path` in data.toml names, or the data folder itself for `path = "."`. so `aria2c -i sources.aria2 -d data`, run in the project's folder with `data` the folder `data_dir` names, puts each plain file where `fetchloom sync` places it. an archive arrives packed, beside where sync would unpack it.

a dataset with `place = false` has no files in the data folder, so `sha256` leaves it out and [says so](#what-it-leaves-out). `links` and `aria2` still list its files, since a recipe's rebuild reads them, and aria2c puts them in the folder `path` would give the dataset, where sync places nothing.

`-o` writes the whole file under a hidden name beside it, flushes it, then gives it its name without replacing anything; a filesystem that cannot do that refuses. the file is UTF-8 with LF line ends on every system; windows powershell 5.1's `>` would write UTF-16 instead.

## what it leaves out

a form leaves out a dataset it has nothing to write for: `links` and `aria2` a recipe's result, which nothing downloads, and files imported from a folder on this machine, and `sha256` a dataset with `place = false`, which has no file in the data folder. each run says so once on stderr and still exits 0, even when that leaves stdout empty. stderr from real runs in a project whose recipe `clean` copies `iris`, with `place = false` on `tips`; the first run wrote nothing on stdout:

```sh
$ fetchloom export --format links clean
warning: the list leaves out clean: a recipe makes its files, so no link holds them
$ fetchloom export -o data.sha256
   Exported 2 files to data.sha256
warning: the list leaves out tips: place = false keeps its files out of the data folder
```

JSON names each such dataset in `left_out`, with why.

otherwise a text form is complete or not written: `export` stops before writing anything when something the form needs is not known here. `--json` without `-o` lists what is known and names the rest instead.

| what is missing | text forms | JSON field |
|---|---|---|
| a dataset `data.lock` does not pin as `data.toml` asks | all forms stop with `lock.outdated`, exit 1: run `fetchloom lock` first | `unlocked` |
| a [recipe](../recipes.md) `data.lock` pins no result of yet | all forms stop with `build.needed`, exit 60: run `fetchloom build` first | `unlocked` |
| the files inside an archive this machine never unpacked, as in a fresh clone | `sha256` stops with `offline.missing`, exit 20: `fetchloom sync` unpacks it; `links` and `aria2` need no unpacking | `not_unpacked` |
| a link that holds a space or a control character | `links` and `aria2` stop with `project.invalid`, exit 50; fetchloom never pins such a link, so `data.lock` was changed by hand or by someone else. a link that holds a login or a secret such as `token=` stops every command sooner, as [`data.lock` is read](../reference/data-lock.md#encoding-and-validation) | `no_link` |

the refusal names the archive, from a clone whose cache never unpacked it:

```sh
$ fetchloom export
error: the files inside data/uci/iris.zip are not known on this machine
  help: run `fetchloom sync` to unpack them here, then export again
```

a refused link is never shown, in the error or in JSON.

## JSON

`result` has `file`, the absolute path `-o` wrote or null; `placed`, a record for each file in the data folder with `dataset`, `path` from the project's folder, `size`, `sha256` and `unpacked_from`, the archive's path in its dataset or null; `sources`, a record for each file `data.lock` pins as downloaded or imported, with `dataset`, `path` in the dataset, `size`, `sha256` and `url`, null for an imported file or a link that is not written; the arrays `unlocked` (dataset names), `not_unpacked` (archive paths from the project's folder) and `no_link` (`<dataset>/<path>`), which holds imported files too; and `left_out`, a record for each dataset the chosen `--format` leaves out, with `dataset` and `reason`: `recipe` or `imported` for `links` and `aria2`, `not_placed` for `sha256`. records are sorted by path, and `placed` holds only what is known. [JSON output](../reference/json-output.md) has the envelope.

## exit codes

0 means the answer was written, even an empty one, or JSON listed what is known. 1 means a dataset is not pinned yet, 20 that an archive's files are not known here, and 50 that a link cannot be written or that `-o` names a file already there. a folder or `-` for `-o`, and `--force` without `-o`, are usage errors, exit 2. other failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| `sha256sum -c` says no such file | it ran outside the project's folder | run it where `data.toml` is |
| `the files inside ... are not known on this machine` | the cache never unpacked that archive, as in a fresh clone | run `fetchloom sync`, or export `--format links` |
| `data.sha256 already exists` | `-o` never replaces a file without `--force` | choose another name, or add `--force` |

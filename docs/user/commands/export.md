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
| `--format links` | one `<link> <sha256>` line for each file downloaded: a plain file, or an archive as one packed file, as `fetchloom add -i` and `fetchloom get -i` read it. `wget -i` and `aria2c -i` take the whole line as one link, which the server does not know |
| `--format aria2` | each file downloaded as an `aria2c -i` input file, with `out=` its place below the data folder and `checksum=sha-256=` |
| `-o`, `--output <file>` | write to `<file>` instead of stdout. a file already there is kept, and the command fails with `local.not_empty` |
| `--force` | with `-o`, replace a file already there. it is moved aside until the new one has its name |

[global options](README.md#global-options) also apply.

## what it writes

`export` reads `data.toml`, `data.lock` and what the cache recorded of each archive it unpacked. it asks no server anything and reads no file in the data folder: the hashes are the pins in `data.lock`, so `sha256sum -c` finds a file someone edited. a [recipe](../recipes.md)'s result is in the `sha256` form like any placed dataset, and in no list of links, since nothing downloads it. in a fresh clone its files are in the data folder only once `fetchloom build` placed them, or a sync from a cache or [mirror](../mirrors.md) that holds them, so before that `sha256sum -c` cannot open them:

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

`out=` puts each file in the folder its dataset is placed in, so `aria2c -i sources.aria2 -d data`, run in the project's folder, puts each plain file where `fetchloom sync` places it. an archive arrives packed, beside where sync would unpack it.

aria2c writes a file already in its folder again under a new name, as `tips.1.csv` beside `tips.csv`. `--check-integrity=true --auto-file-renaming=false` stops that: aria2c then keeps a file whose SHA-256 matches, downloads one changed at the same size again, and stops with exit 13 at a file of another size; delete that file and run it again. it cannot write the read-only files sync placed, so point it at a folder sync did not fill. a second run with both options, over the files the first wrote, then the check, from real runs of aria2c 1.37 for a project whose `iris` and `tips` have `path = "raw"`:

```sh
$ aria2c -q -i sources.aria2 -d data --check-integrity=true --auto-file-renaming=false
$ sha256sum -c data.sha256
data/raw/iris.csv: OK
data/raw/tips.csv: OK
```

a dataset with `place = false` has no files in the data folder, so `sha256` leaves it out and [says so](#what-it-leaves-out), while `links` and `aria2` still list its files, since a recipe's rebuild reads them.

`-o` writes the whole file under a hidden name beside it, then gives it its name without replacing anything. the file is UTF-8 with LF line ends on every system, while windows powershell 5.1's `>` would write UTF-16.

## what it leaves out

a form leaves out what it has nothing to write for: `links` and `aria2` a recipe's result and files imported from a folder, and `sha256` a dataset with `place = false`. each run says so once on stderr and still exits 0, even when stdout is empty. stderr from real runs in a project whose recipe `clean` copies `iris`, with `place = false` on `tips`; the first run wrote nothing on stdout:

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
| a link that holds a space or a control character, which fetchloom never pins itself | `links` and `aria2` stop with `project.invalid`, exit 50. a link that holds a login or a secret stops every command sooner, as [`data.lock` is read](../reference/data-lock.md#encoding-and-validation) | `no_link` |

the refusal names the archive, from a clone whose cache never unpacked it:

```sh
$ fetchloom export
error: the files inside data/uci/iris.zip are not known on this machine
  help: run `fetchloom sync` to unpack them here, then export again
```

a refused link is never shown, in the error or in JSON.

## JSON

`result` has `file`, the path `-o` wrote or null; `placed`, a record for each file in the data folder; `sources`, a record for each file `data.lock` pins as downloaded or imported, with its `url`; `unlocked`, `not_unpacked` and `no_link`, from the table above; and `left_out`, each dataset the chosen `--format` leaves out with its `reason`. [JSON output](../reference/json-output.md#command-results) gives every field.

## exit codes

0 means the answer was written, even an empty one, or JSON listed what is known. 1 means a dataset is not pinned yet, 20 that an archive's files are not known here, and 50 that a link cannot be written or that `-o` names a file already there. a folder or `-` for `-o`, and `--force` without `-o`, are usage errors, exit 2. other failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| `sha256sum -c` says no such file | it ran outside the project's folder | run it where `data.toml` is |
| `the files inside ... are not known on this machine` | the cache never unpacked that archive, as in a fresh clone | run `fetchloom sync`, or export `--format links` |
| `data.sha256 already exists` | `-o` never replaces a file without `--force` | choose another name, or add `--force` |
| `wget -i` or `aria2c -i` asks for a link ending in `%20` and a hash, and gets 404 | they read a whole line of the `links` form as one link | give aria2c the `aria2` form; `fetchloom add -i` and `get -i` read `links` |
| aria2c leaves `tips.1.csv` beside `tips.csv` | aria2c renames a file already in its folder | run it into an empty folder, or add `--check-integrity=true --auto-file-renaming=false` |

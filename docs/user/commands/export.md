# export

`export` writes the SHA-256 of each file in the data folder, or the link and SHA-256 of each file a project downloads, in forms other tools read, or a citation of each dataset for a paper.

```sh
fetchloom export > data.sha256
fetchloom export --format links iris
fetchloom export --format aria2 -o sources.aria2
fetchloom export --format bibtex -o data.bib
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
| `--format bibtex`, `csl`, `ris` | a citation of each dataset: a BibTeX entry, a CSL-JSON item, as pandoc and zotero read them, or an RIS record, as [citations](#citations) says |
| `--format statement` | a data availability paragraph for a paper: each dataset with its DOI or link, and the SHA-256 of each of its files |
| `-o`, `--output <file>` | write to `<file>` instead of stdout. a file already there is kept, and the command fails with `local.not_empty`, as `data.sha256 already exists`. the project's own `data.toml`, `data.lock` and what `data.lock.d` holds are refused as usage, exit 2, `--force` or not |
| `--force` | with `-o`, replace a file already there. it is moved aside until the new one has its name |

[global options](global-options.md) also apply.

## what it writes

`export` reads `data.toml`, `data.lock` and what the cache recorded of each archive it unpacked. it asks no server anything and reads no file in the data folder: the hashes are the pins in `data.lock`, so `sha256sum -c` finds a file someone edited.

the `sha256` paths start at the project's folder, so run `sha256sum -c` there, or it finds no such file. from a real run with a zenodo dataset and a zip from UCI, which unpacks to four files:

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

a [semantic scholar dataset](../sources/semantic-scholar.md#datasets)'s files are listed by the links data.lock keeps, the API's listing of the dataset with each file's name after `#`, since the links to the files themselves last only a week. other tools cannot download those; fetchloom asks the listing for a fresh link to each file.

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

`-o` writes the whole file under a hidden name beside it, then gives it its name without replacing anything. the file is UTF-8 with LF line ends on every system, while windows powershell 5.1's `>` would write UTF-16.

## citations

`bibtex`, `csl` and `ris` write an entry for each dataset, keyed by its name in `data.toml`. for a dataset whose pin has a DOI, export asks doi.org once, with an `Accept` header for CSL-JSON and no key; doi.org sends the request on to the DOI's registration agency, such as DataCite or Crossref, whose record gives the authors, year, publisher and title. the rest of an entry comes from `data.lock`: the DOI's link, the link the dataset was pinned from when it has no DOI, or else the dataset's page its reference names, as a GEO accession's page at `https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc=GSE60450`, the version after `@` in its pin, the license, and the day it was pinned, as the access date. `data.lock` names no author, publisher or year, so an entry built from it alone is marked incomplete in the file, as `note = {Incomplete: built from data.lock, which gives no author, year or title.}`, and a warning names it, with help that names each field to fill in by hand, as `fill in the authors, year and title by hand before you cite it`. no field is added to `data.lock`.

offline, doi.org is not asked: every entry comes from `data.lock`, the run still exits 0, and a warning says so, as `offline, so doi.org was not asked for the DOI metadata of iris and paper: their entries are built from data.lock alone`. when doi.org gives no record, as for a DOI it does not know or an agency without CSL-JSON, that entry comes from `data.lock` too, with a warning naming the dataset and why. a dataset imported from a folder has no source to cite, so the citations leave it out with a warning.

every word from `data.lock` or a record goes on one line, escaped for its form, so a title cannot end its field or start another entry. the warnings have the kind `export.incomplete`.

the statement asks no server. it names each dataset with its title and its DOI's link, or the link it was pinned from, or its reference, then the SHA-256 and path of each file `data.lock` pins for it, a dataset imported from a folder among them:

```text
Data availability

This work uses 3 datasets. Each is listed below with its DOI or link and the SHA-256 checksum of every file used, so that readers can check that their copies are identical.

iris: DatasetIris, https://doi.org/10.5281/zenodo.101, 1 file:
```

## what it leaves out

a form leaves out what it has nothing to write for: `links` and `aria2` files imported from a folder, and `sha256` a dataset [dropped](drop.md) on this machine, as `warning: the list leaves out pets: it is dropped on this machine, so its files are not in the data folder`. every form leaves out a dataset whose file list `data.lock.d` lacks, as in a clone of a commit that left the list out, with the warning that it [waits for it](../reference/data-lock.md#big-datasets); `export` naming that dataset stops with that warning's words, exit 50. each run says so once on stderr and still exits 0, even when stdout is empty.

JSON names each such dataset in `left_out`, with why.

otherwise a text form is complete or not written: `export` stops before writing anything when something the form needs is not known here. `--json` without `-o` lists what is known and names the rest instead.

| what is missing | text forms | JSON field |
|---|---|---|
| a dataset `data.lock` does not pin as `data.toml` asks | all forms stop with `lock.outdated`, exit 1: run `fetchloom lock` first | `unlocked` |
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

`result` has `file`, the path `-o` wrote or null; `citations`, for `bibtex`, `csl` and `ris`, what each entry names and where it came from; `placed`, a record for each file in the data folder; `sources`, a record for each file `data.lock` pins as downloaded or imported, with its `url`; `unlocked`, `not_unpacked` and `no_link`, from the table above; and `left_out`, each dataset the chosen `--format` leaves out with its `reason`. [JSON output](../reference/json-output.md#command-results) gives every field.

## exit codes

0 means the answer was written, even an empty one, or JSON listed what is known. 1 means a dataset is not pinned yet, 20 that an archive's files are not known here, and 50 that a link cannot be written or that `-o` names a file already there. a folder, `-` or one of the project's own files for `-o`, and `--force` without `-o`, are usage errors, exit 2. other failures use the [shared exit codes](../reference/exit-codes.md).

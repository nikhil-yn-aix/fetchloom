# data.toml

`data.toml` names the datasets a project wants and how to place them.

```toml
[project]
data_dir = "data"

[datasets.iris]
ref = "zenodo:3608990"

[datasets.flowers]
ref = "zenodo:3608990"
select = "*.csv"
exclude = "**/drafts/**"
extract = false
copy = true
path = "measurements"
```

`project` and `datasets` are optional tables. a dataset is fetched from a source, named by `ref`, or made from other datasets by a [recipe](#recipes), named by `from` and `run`. each dataset entry has these keys:

| key | meaning | default |
|---|---|---|
| `project.data_dir` | folder relative to the project root | `"data"` |
| `ref` | a reference from [sources](../sources/README.md), or `"local"` for [imported files](#imported-files) | required, nonempty string |
| `select` | keep files matching any of these patterns | all files; for a `ref` that links one file or folder inside a dataset, that file or everything below that folder |
| `exclude` | drop files matching any of these patterns | none |
| `extract` | unpack recognized archive names | `true` |
| `members` | tables that keep or leave out files inside each archive; see [files inside archives](#files-inside-archives) | none |
| `copy` | request writable placement by reflink or copy, never hardlink | `false` |
| `path` | folder relative to `data_dir`, or `"."` for the data folder itself | the dataset name |
| `from`, `run`, `code`, `env` | a recipe's inputs, command, code and variables; see [recipes](#recipes) | none |

## imported files

`fetchloom add ./flowers` writes `ref = "local"` for files it imports from a folder or a file on this machine. the entry names no path: a path would mean nothing on a teammate's machine and would say where your files are. data.lock pins each file by its path below the folder, its size and its SHA-256, and the cache holds the bytes. `select`, `exclude` and `extract` apply to the imported files as they do to a source's listing.

no source can list an import again, so a changed `select` or `exclude`, or `ref = "local"` written by hand for a dataset data.lock does not pin, is refused with `reference.unresolved`, exit 10, before any request. to pin other files, run `fetchloom remove <name>`, then `fetchloom add <folder> --as <name>`. a changed `extract` keeps the pinned files. [add](../commands/add.md#files-on-this-machine) says how a clone without the files gets them.

## dataset names

names are 1 to 64 ASCII characters: `a-z`, `0-9`, `-`, `_`. the first character must be a letter or digit.

without `--as`, `add` chooses a name from the listing before applying command-line filters, in this order:

| available information | generated name |
|---|---|
| one listed file or a file selected by its source link | its filename stem, limited to 40 characters |
| a title, except for hugging face, whose datasets are known by their id | up to four words and 32 characters, dropping trailing words such as `and`, `of` and `the`; if no first word fits, the 40-character free-text rule applies |
| a reference | its last part if that contains a letter, otherwise the reference without its version; `hf:datasets/scikit-learn/iris` gives `iris`, and `zenodo:123` gives `zenodo-123` |

if the generated name is taken, fetchloom tries `-2` through `-99`, then fails if all are taken. it warns which name it chose:

```text
warning: iris was taken, so this one is iris-2
  help: choose a name with --as <name>
```

an explicitly supplied `--as` name that is taken is refused. a name that breaks the rules above is refused with the rule it breaks, such as its length or the characters it holds, and a suggestion made from it: lowercase, with accents dropped from letters (`ïris` gives `iris`), other characters turned into `-`, at most 40 characters, and `-data` after a name windows keeps for a device, such as `con`. generated names drop accents the same way.

## destination paths

`data_dir` and `path` use `/` on every platform. absolute paths, drive prefixes, backslashes, control characters, empty components, `.` and `..` are refused, with one exception: `path = "."` is the data folder itself, so a dataset's `train.csv` lands at `data/train.csv` for scripts that already read it there. `data_dir = "."` stays refused, since it would mix data with the project's own files. trailing `/` is stripped. the data folder and its child folders must be real directories; see [filesystem placement](../../dev/architecture/placing.md#choose-a-filesystem-method). a data folder that is a link to another disk is refused too, and `data_dir` cannot point there instead: to keep the data on another disk, move the whole project folder to that disk.

`path` names a folder, never a file: `path = "train.csv"` makes a folder named `train.csv`, so the file lands at `data/train.csv/train.csv`. sync places it there and warns, naming the `path` that puts it at `data/train.csv`:

```text
warning: path = "train.csv" makes a folder named train.csv, so the file is data/train.csv/train.csv
  help: path names a folder; to put the file at data/train.csv, set path = "." on datasets.train in data.toml
```

datasets may share a folder, and one may sit in the data folder itself while others sit in folders below it, as long as no two files land on the same path. names are compared the way a disk that ignores case compares them, so `README` from one dataset and `readme` from another clash on every system, and so do two files with the same bytes. a clash names both datasets and the path:

```text
error: datasets test and train would both place data/splits/README
  help: give datasets.test or datasets.train another path in data.toml
```

sync and add refuse a clash before downloading anything when the sources list the file names, as most repositories and buckets do. a name only the server gives, as for a plain link, is checked once the file is downloaded, and the files inside an archive this machine never unpacked once it is unpacked, still before anything is placed or replaced.

each placed file belongs to one dataset, so `remove` and sync delete only that dataset's unchanged files from a shared folder and leave the other datasets' files and your own.

fetchloom takes these folders as written and never expands `~` or a variable, since data.toml comes with whatever repository you clone. a `data_dir` or `path` that starts with `~` or `$`, or names a variable such as `$SCRATCH` or `${SCRATCH}`, is refused rather than made as a folder of that name. names such as `data~old` and `price$` stay usable. on a cluster, put the cache on scratch with `FETCHLOOM_CACHE`, or the whole project there, as [clusters and shared machines](../hpc.md) shows.

changing `data_dir` or a dataset's `path` moves where sync places its files. sync deletes only files it placed that are unchanged, and only inside the data folder, so a folder that still holds anything else stays, and so does the old data folder with every file in it; the sync that leaves one behind names it in a warning, as [sync](../commands/sync.md#folders-left-behind) shows.

`data_dir`, `path` and every placed file path refuse repository metadata components: `.git`, `.hg`, `.svn` and `.fetchloom`, ignoring case. this includes trailing dots or spaces, a `:` stream suffix, the windows short names `git~1`, `hg~1`, `svn~1` and `fetchl~1`, and spellings with the invisible code points hfs+ drops from names (U+200C to U+200F, U+202A to U+202E, U+206A to U+206F and U+FEFF), the set git checks for the same reason. the rule also covers source listings, unpacked archive members and paths inside a `get` folder. names such as `.github` and `.gitignore` remain usable.

file paths refuse control characters, including the bidi controls U+061C, U+200E, U+200F, U+202A to U+202E and U+2066 to U+2069. this covers manifest paths, source listings and archive members. server-supplied file names with bidi controls are refused too, including names decoded from `Content-Disposition`. ordinary right-to-left letters remain usable.

`add` keeps comments and table order, but cannot add to an inline `datasets` table. `remove` also removes comments attached to the deleted table and strips leading blank lines.

`copy = true` applies when a file is placed. setting it on a dataset makes the next sync replace each unchanged read-only file of that dataset with a writable one; `status` lists those files as outdated until then, and an edited file stays. an untracked read-only file with exactly the pinned bytes is also replaced with a writable file, without changing permissions through any other hardlink to it. differing untracked bytes still stay unless force is requested. turning it off leaves writable files as they are.

## recipes

a recipe makes a dataset from others with a command of yours. [recipes](../recipes.md) explains how fetchloom keys and checks them; this section lists the rules data.toml follows.

```toml
[datasets.clean]
from = ["iris"]
run = ["python3", "clean.py", "{iris}", "{out}"]
code = ["clean.py", "lib/"]
env = { SOURCE_DATE_EPOCH = "315532800" }
```

| key | meaning | rules |
|---|---|---|
| `from` | the datasets it is made from, by name | required; a list of names data.toml has, each once, not empty. `out` cannot be one, since `{out}` names the result's folder. recipes cannot be made from each other, directly or through others |
| `run` | the program and its arguments | required; a list of words, since no shell reads it. `{name}` stands for the folder of a dataset in `from`, `{out}` for the result's folder and `{{` and `}}` for one brace, anywhere inside a word. it names every dataset in `from` and `{out}`, and no other name. a brace that is not part of these, a control character or an empty program is refused |
| `code` | project files and folders whose bytes enter the key | optional list. each path uses `/`, may end in `/`, and stays inside the project: no absolute path, drive, backslash, `.` or `..`, no repository metadata such as `.git`, not `data.lock` or `data.lock.d`, and neither inside the data folder nor holding it. a link or a special file on the way is refused when the code is read |
| `env` | variables set for the step alone | optional table of strings. names use letters, digits and `_` and do not start with a digit. names starting with `FETCHLOOM_` are refused, and so are names that look like a secret: a word ending in `TOKEN`, a word holding `SECRET`, `PASSWORD`, `PASSWD` or `CREDENTIAL`, or the words `API_KEY`, `ACCESS_KEY` or `PRIVATE_KEY`, ignoring case. `MAX_TOKENS` and `TOKENIZERS_PARALLELISM` stay usable. a value cannot hold a NUL character |

a recipe takes `copy` and `path` as a fetched dataset does. `ref`, `select`, `exclude` and `extract` belong to fetched datasets: beside `from` they are refused, and choosing files belongs inside the step. every mistake is `project.invalid`, exit 50, pointing at its line:

for a `clean` whose `run`, on line 7 of data.toml, leaves out `{out}`:

```text
error: run in datasets.clean never names {out}, so the step has nowhere to write
  data.toml:7   run = ["python3", "clean.py", "{iris}"]
  help: put {out} where the step takes the folder to write the result in
```

a message about `env` cites its line by number only, as one about `ref` does, since either may hold a secret.

## patterns

`select` and `exclude` each accept a string or an array of strings. an empty array has the default meaning. matching is case-sensitive and covers the whole source path. leading `/` is ignored.

| pattern | matches |
|---|---|
| `*.csv` | CSV files at the top of the dataset |
| `**/*.csv` | CSV files at any depth, including the top |
| `sub-??/data.[ct]sv` | two characters after `sub-`, then `data.csv` or `data.tsv` |
| `**/[!0-9]*` | a file name not starting with a digit, at any depth |

`*`, `?` and character sets stay within one path component. sets accept ranges and `!` or `^` for negation. `**` must be a whole component. backslashes and empty components are errors; braces have no special meaning.

each select pattern must match at least one listed path before exclusions, including a google docs entry in a drive folder. an unmatched pattern shows one example path. a selection that leaves no files fails. listing limits apply before selection.

patterns select the files a source lists, before download and before a server-supplied name replaces a guessed name. they do not reach files inside an archive; a [members table](#files-inside-archives) does. see [archives](../concepts.md#archives) for where those files land.

## files inside archives

`select` and `exclude` choose among the files a source lists, so an archive is one file to them. a `members` table chooses among the files inside one archive, named by the archive's path as data.lock gives it:

```toml
[datasets.pets]
ref = "https://example.org/pets.zip"

[datasets.pets.members."pets.zip"]
exclude = [
  "pets-v1/images/blurry_011.png",
  "pets-v1/images/dog_010_corrupt.png",
]
```

`select` and `exclude` in a members table take the [patterns](#patterns) above and match each file by its path inside the archive, its top folder included, before placing strips that folder. `pets-v1/images/dog_010_corrupt.png` above lands at `data/pets/images/dog_010_corrupt.png`, since a dataset that is one archive loses the folder all its files share. a file a table leaves out is not placed, and every other file lands where it would without the table. a table names one archive, so in a dataset of `train.zip` and `test.zip` that both hold `images/x.png`, a table under `"train.zip"` leaves `test.zip`'s copy in place.

a members table changes which files land, not what data.lock pins, so editing one needs no network: sync removes the files it leaves out and places those it lets back in from the cache, and `sync --locked` accepts it. each of these is refused with `project.invalid`, exit 50, pointing at its line:

| mistake | why |
|---|---|
| a table for an archive data.lock does not pin for the dataset | after an upgrade renames `pets.zip`, the table must follow it, or the files it left out would come back without a word |
| a members table on a dataset with `extract = false` | its archives stay packed, so there are no files inside to choose |
| a `select` pattern that matches no file inside its archive | the error shows a path inside the archive, or the pattern with the archive's top folder in front when that one matches |
| patterns that leave an archive no file | to leave the whole archive out, `exclude` it in the dataset instead |

an `exclude` that matches nothing is fine: the upstream may have fixed the file. the archive is still unpacked whole into the cache.

## encoding and validation

manifest files are UTF-8 TOML. a byte order mark is accepted on read and omitted on write. unknown keys fail; a nearby spelling is suggested when one is available.

diagnostics show only the line number when a cited line contains a reference, including inline tables, or when invalid TOML prevents identifying reference boundaries. reference text can hold credentials, so it is not copied into those diagnostics.

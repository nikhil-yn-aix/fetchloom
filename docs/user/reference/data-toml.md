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

`project` and `datasets` are optional tables, and [`required_version`](#fetchloom-versions) an optional key above them. a dataset is fetched from a source, named by `ref`. each dataset entry has these keys:

| key | meaning | default |
|---|---|---|
| `project.data_dir` | folder relative to the project root | `"data"` |
| `ref` | a reference from [sources](../sources/README.md), `"local"` for [imported files](#imported-files), or `list:` and a list's link or its path in the project for [a list of files](#lists-of-files) | required, nonempty string |
| `select` | keep files matching any of these patterns | all files; for a `ref` that links one file or folder inside a dataset, that file or everything below that folder |
| `exclude` | drop files matching any of these patterns; a web server's folder one leaves out whole, as `big/**` does `big`, is not listed | none |
| `extract` | unpack recognized archive names | `true` |
| `web_page` | pin the web page a link answers with as the file, where a page is otherwise refused; see [web pages as data](../sources/urls-and-dois.md#web-pages-as-data) | `false` |
| `members` | tables that keep or leave out files inside each archive; see [files inside archives](#files-inside-archives) | none |
| `copy` | request writable placement by reflink or copy, never hardlink | `false` |
| `path` | folder relative to `data_dir`, or `"."` for the data folder itself | the dataset name |

## imported files

`fetchloom add ./flowers` writes `ref = "local"` for files it imports from a folder or a file on this machine. the entry names no path: a path would mean nothing on a teammate's machine and would say where your files are. data.lock pins each file by its path below the folder, its size and its SHA-256, and the cache holds the bytes. `select`, `exclude` and `extract` apply to the imported files as they do to a source's listing.

no source can list an import again, so a changed `select` or `exclude`, or `ref = "local"` written by hand for a dataset data.lock does not pin, is refused with `reference.unresolved`, exit 10, before any request. so is an import [renamed](#dataset-names) in data.toml: every import's table reads the same, so its pin cannot tell a renamed import from a new one. the error's help gives both ways on: give the table back its old name, or import the files again under the new one with `fetchloom add <folder> --as <new name>`. to pin other files, run `fetchloom remove <name>`, then `fetchloom add <folder> --as <name>`. a changed `extract` keeps the pinned files. [add](../commands/add.md#files-on-this-machine) says how a clone without the files gets them.

## lists of files

a dataset may be the files a list names, as [a list of files](../commands/add.md#a-list-of-files) shows: `ref = "list:https://example.org/data/registry.txt"` for a list at a link, and `ref = "list:lists/files.txt"` for a list in the project, by its path from the project folder with `/` between folders, which `fetchloom add -i <file>` writes. a link to a checksum list, as `https://example.org/data/SHA256SUMS`, reads as the same list, and `add` writes it with `list:`.

data.toml names the list, never holds it: commit a list in the project with data.toml. data.lock pins each file by its link, so a clone syncs without the list, which is read again only when the dataset is locked again. since data.toml comes with whatever repository you clone, a list path that climbs out of the project, starts at the root of a disk, uses `~` or `$`, or names repository metadata is refused with `reference.invalid`, exit 10, and a list is read only from a real file in the project, never through a link or a link to a folder. a list path data.toml names that is not in the project is refused with `reference.unresolved`, exit 10, when the dataset is locked again.

## dataset names

names are 1 to 64 ASCII characters: `a-z`, `0-9`, `-`, `_`. the first character must be a letter or digit. a name windows keeps for a device, `con`, `prn`, `aux`, `nul`, `com1` to `com9` or `lpt1` to `lpt9`, is refused too, since a dataset's name is its folder on every machine that syncs the project. data.toml and data.lock are both checked this way before anything is downloaded or written.

without `--as`, `add` chooses a name from the listing before applying command-line filters, in this order:

| available information | generated name |
|---|---|
| one listed file, unless the reference is a bucket's folder, ending in `/`, or a file selected by its source link | its name without a known file type, such as `.csv`, `.pdf` or `.gz`, without the known type before a compression type, such as `.gz`, `.bz2`, `.xz` or `.zst`, and without a `.tar` before any type, limited to 40 characters; other dots become `-`, so `x.csv.gz` gives `x`, `data.tar.gz` gives `data`, `2024.05.csv` gives `2024-05` and the arXiv id `1706.03762v7` gives `1706-03762v7` |
| a title, except for hugging face, whose datasets are known by their id | up to four words and 32 characters, dropping trailing words such as `and`, `of` and `the`; if no first word fits, the 40-character free-text rule applies |
| a reference | its last part without the version, taking the parts before it one at a time until the name holds a letter; `hf:datasets/scikit-learn/iris` gives `iris`, `gs://arxiv-dataset/arxiv/arxiv/pdf/2401/` gives `pdf-2401`, and `zenodo:123` gives `zenodo-123` |

if the generated name is taken, fetchloom tries `-2` through `-99`, then fails if all are taken. it warns which name it chose:

```text
warning: iris was taken, so this one is iris-2
  help: choose a name with --as <name>
```

to rename a dataset, change its table's name in data.toml, as `[datasets.iris]` to `[datasets.flowers]`, and the name in each of its [members tables](#files-inside-archives); members tables left under the old name are refused with `project.invalid`. the next `fetchloom sync` or `fetchloom lock` moves the pin in data.lock to the new name without asking the source, as long as the table asks for the same `ref`, `select`, `exclude` and `extract` as before, and says `Renamed iris to flowers in data.lock · its pin stays`; sync then places the files in the new name's folder. an [import](#imported-files) keeps the name it was added with. a rename together with one of those edits, or one fetchloom cannot match to a single old name, resolves the new name again, which may pin newer data.

an explicitly supplied `--as` name that is taken is refused. a name that breaks the rules above is refused with the rule it breaks, such as its length or the characters it holds, and a suggestion made from it: lowercase, with accents dropped from letters (`ïris` gives `iris`), other characters turned into `-`, at most 40 characters, and `-data` after a name windows keeps for a device, such as `con`, whose help asks for another name, such as `--as con-data`. generated names are made the same way, so a file `con.csv` gives `con-data`.

## destination paths

`data_dir` and `path` use `/` on every platform. absolute paths, drive prefixes, backslashes, control characters, empty components, `.` and `..` are refused, with one exception: `path = "."` is the data folder itself, so a dataset's `train.csv` lands at `data/train.csv` for scripts that already read it there. `data_dir = "."` stays refused, since it would mix data with the project's own files. so is a folder some disk cannot store, on every system, since windows clones read the same data.toml: a name windows reserves for devices, such as `NUL` or `con`, one ending in a dot or a space, one with `<`, `>`, `:`, `"`, `|`, `?` or `*`, or one longer than 255 bytes. `data_dir` cannot start with the name of one of the project's own files, `data.toml`, `data.lock` or `data.lock.d`, and none of its folders may be named as a file git reads, `.gitignore`, `.gitattributes` or `.gitmodules`, in any case. a leading `./` and a trailing `/` are stripped, so `./data` is `data`; spaces are kept as written, so `"data "` is refused as a name ending in a space, not read as `data`. the data folder and its child folders must be real directories; see [filesystem placement](../../dev/architecture/placing.md#choose-a-filesystem-method). a data folder that is a link to another disk is refused too, and `data_dir` cannot point there instead: to keep the data on another disk, move the whole project folder to that disk.

the files inside a dataset keep the names their source, folder or archive gives them. windows cannot store some names linux and macos can: one with `<`, `>`, `:`, `"`, `|`, `?` or `*`, one ending in a dot or a space, or a device name such as `con.txt` or `nul`. the run that pins a dataset holding such names places them here as they are and warns once for that dataset, naming them, and its help says what lets a teammate on windows go on: leaving files the source lists out with `exclude`, or keeping an archive that holds such a name packed with `extract = false`, since windows cannot unpack it:

```text
warning: fatbad has 3 files whose names windows and fat disks cannot store: pipe|.csv, star*.csv and what?.csv
  help: a teammate on windows cannot place them; leave them out with exclude in data.toml if one needs to
```

on windows a sync refuses such a file by name before anything is written, and an archive holding any is refused whole, naming each. elsewhere a fat or exfat disk takes what its driver takes while placing: linux's exfat driver refuses `<`, `>`, `:`, `"`, `|`, `?` and `*`, macos 15 took every one of these names on exfat, and neither refuses a device name, which windows keeps for its devices.

`path` names a folder, never a file: `path = "train.csv"` makes a folder named `train.csv`, so the file lands at `data/train.csv/train.csv`. sync places it there and warns, naming the `path` that puts it at `data/train.csv`:

```text
warning: path = "train.csv" makes a folder named train.csv, so the file is data/train.csv/train.csv
  help: path names a folder; to put the file at data/train.csv, set path = "." on datasets.train in data.toml
```

datasets may share a folder, and one may sit in the data folder itself while others sit in folders below it, as long as no two files land on the same path. names are compared the way a disk that ignores case compares them, so `README` from one dataset and `readme` from another clash on every system, and so do two files with the same bytes, and two spellings of one name in unicode, as `café` with one `é` and with `e` and an accent, which macos disks take for one file; the one not composed is shown by its code points, as `cafe\u{301}`. a clash names both datasets and the path:

```text
error: datasets test and train would both place data/splits/README
  help: give datasets.test or datasets.train another path in data.toml
```

sync and add refuse a clash before downloading anything when the sources list the file names, as most repositories and buckets do. a name only the server gives, as for a plain link, is checked once the file is downloaded, and the files inside an archive this machine never unpacked once it is unpacked, still before anything is placed or replaced. a clash between a file the source lists beside an archive and a file inside it is resolved by leaving the one beside it out with the dataset's `exclude`, or the one inside with the archive's [members table](#files-inside-archives) while the archive keeps another file, or by keeping the archive packed with `extract = false`; an add that meets such a clash is undone, so its help names `--exclude` and `--no-extract` instead. two files of one archive that a disk ignoring case sees as one, or that differ only in how their unicode is composed, stop the archive from unpacking at all, before a members table could leave one out, so only `extract = false`, or `--no-extract` for an add, keeps such an archive, packed:

```text
error: the archive holds "Data.csv" and "data.csv", which are one name on disks that ignore case
  in case/case.zip
  help: keep the archive packed with extract = false on its dataset in data.toml, or with --no-extract
```

each placed file belongs to one dataset, so `remove` and sync delete only that dataset's unchanged files from a shared folder and leave the other datasets' files and your own.

fetchloom takes these folders as written and never expands `~` or a variable, since data.toml comes with whatever repository you clone. a `data_dir` or `path` that starts with `~` or `$`, or names a variable such as `$SCRATCH` or `${SCRATCH}`, is refused rather than made as a folder of that name. names such as `data~old` and `price$` stay usable. on a cluster, put the cache on scratch with `FETCHLOOM_CACHE`, or the whole project there, as [clusters and shared machines](../guides/clusters.md) shows.

changing `data_dir` or a dataset's `path` moves where sync places its files. sync deletes only files it placed that are unchanged, and only inside the data folder, so a folder that still holds anything else stays, and so does the old data folder with every file in it; the sync that leaves one behind names it in a warning, as [sync](../commands/sync.md#folders-left-behind) shows.

`data_dir`, `path` and every placed file path refuse repository metadata components, `.git`, `.hg` and `.svn`, and fetchloom's own folder, `.fetchloom`, ignoring case. this includes trailing dots or spaces, a `:` stream suffix, the windows short names `git~1`, `hg~1`, `svn~1` and `fetchl~1`, and spellings with the invisible code points hfs+ drops from names (U+200C to U+200F, U+202A to U+202E, U+206A to U+206F and U+FEFF), the set git checks for the same reason. the rule also covers source listings, unpacked archive members and paths inside a `get` folder. a single file named so only by its link or its server's `Content-Disposition` is kept under that name with `_` in front, as `_.git`. names such as `.github` remain usable, and so does `.gitignore` everywhere but in `data_dir`.

file paths refuse control characters, including the bidi controls U+061C, U+200E, U+200F, U+202A to U+202E and U+2066 to U+2069. this covers manifest paths, source listings and archive members. server-supplied file names with bidi controls are refused too, including names decoded from `Content-Disposition`. ordinary right-to-left letters remain usable.

`add` keeps comments and table order, but cannot add to an inline `datasets` table. `remove` also removes the comments of the table it deletes.

`copy = true` makes the next sync replace each unchanged read-only file of the dataset with a writable copy or clone, as [placement](../concepts/cache-and-placement.md#placement) says; `status` lists those files as outdated until then. turning it off leaves writable files as they are.

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

patterns select the files a source lists, before download and before a server-supplied name replaces a guessed name. they do not reach files inside an archive; a [members table](#files-inside-archives) does. an `exclude` that names a file inside an archive by the path it lands at below the dataset's folder, and matches no file the source lists, makes the sync that lists the source again warn, naming the `fetchloom exclude` that leaves it out. one that left out a listed file, as `train.csv` beside `train.csv.zip`, did its job and gets no warning. see [archives](archives.md) for where those files land.

## files inside archives

`select` and `exclude` choose among the files a source lists, so an archive is one file to them. a `members` table chooses among the files inside one archive, named by the archive's path as data.lock gives it, or inside each archive a [glob key](#one-table-for-many-shards) matches:

```toml
[datasets.pets]
ref = "https://example.org/pets.zip"

[datasets.pets.members."pets.zip"]
exclude = [
  "pets-v1/images/blurry_011.png",
  "pets-v1/images/dog_010_corrupt.png",
]
```

`select` and `exclude` in a members table take the [patterns](#patterns) above and match each file by its path inside the archive, its top folder included, before placing strips that folder. `pets-v1/images/dog_010_corrupt.png` above lands at `data/pets/images/dog_010_corrupt.png`, since a dataset that is one archive loses the folder all its files share. a file a table leaves out is not placed, and every other file lands where it would without the table. a table under an archive's path names that archive alone, so in a dataset of `train.zip` and `test.zip` that both hold `images/x.png`, a table under `"train.zip"` leaves `test.zip`'s copy in place. without such a table the two copies would land on one path, so the run stops before placing anything, naming the path and both archives, as in `two files would land on data/fold/train/0000.txt: one from fold-000000.zip and one from fold-000001.zip`. leave one copy out, keep the archives packed with `extract = false`, or give each archive a dataset of its own. [`fetchloom exclude`](../commands/exclude.md) writes these lines for you from the paths in the data folder.

a members table changes which files land, not what data.lock pins, so editing one needs no network: sync removes the files it leaves out and places those it lets back in from the cache, and `sync --locked` accepts it. each of these is refused with `project.invalid`, exit 50, pointing at its line:

| mistake | why |
|---|---|
| a table for an archive data.lock does not pin for the dataset, or a glob key that matches none | after an upgrade renames `pets.zip`, the table must follow it, or the files it left out would come back without a word |
| two glob keys that match one archive with no table of its own | the archive would have two tables, so the error names both keys and the archive; give it a table of its own, which wins over both |
| a members table on a dataset with `extract = false` | its archives stay packed, so there are no files inside to choose |
| a `select` pattern that matches no file inside its archive, or for a glob key, inside any archive the key covers | the error shows a path inside the archive, or the pattern with the archive's top folder in front when that one matches |
| patterns that leave an archive no file, or for a glob key, every archive it covers | to leave them out, `exclude` them in the dataset instead; a glob key's table may leave some shards none |

an `exclude` that matches nothing is fine: the upstream may have fixed the file. the archive is still unpacked whole into the cache, and [`fetchloom cache gc`](../commands/cache.md#gc) frees the files no project keeps.

### one table for many shards

a key with `*`, `?` or `[` is a [pattern](#patterns) too, matched against each archive's path as data.lock gives it, so one table chooses the same files in every shard of a [webdataset](archives.md#webdataset-shards), and in a shard that arrives later. with six shards of 40 samples each, served from this machine:

```toml
[datasets.shards]
ref = "s3:http://127.0.0.1:19380/speech/shards/"

[datasets.shards.members."shard-*.tar"]
select = ["*.wav", "*.txt"]
```

sync placed only the 40 `.wav` and 40 `.txt` files of each shard, from `data/shards/shard-000000` to `shard-000005`. once `fetchloom lock --upgrade shards` pinned a seventh shard, sync placed its files the same way, as its stderr shows:

```text
     Placed data/shards · 80 files linked to the cache
     Synced 1 dataset into data
```

shards may differ, so each `select` pattern of a glob key needs a file in one archive the key covers, not in each, and a shard the table leaves no file places none: with `.flac` files in one of six shards, `select = ["*.flac", "*.txt"]` placed the `.txt` files of all six and the `.flac` files of that one. a pattern that matches in no shard, as the typo `"*.flca"`, is refused once every shard the key covers is unpacked here: `the select pattern "*.flca" in members."shard-*.tar" of datasets.shards matches no file inside the 6 archives it covers`. so is a table that leaves every shard it covers no file, as `exclude = ["*"]` does, naming the key: `select and exclude in members."shard-*.tar" of datasets.speech leave no file of the 8 archives it covers`. a table under an archive's own path wins over any glob, so `[datasets.shards.members."shard-000003.tar"]` alone chooses for that shard. `*` stays inside one folder: `"shard-*.tar"` does not match `train/shard-000000.tar`, which `"train/shard-*.tar"` or `"**/shard-*.tar"` does.

## fetchloom versions

`required_version` names the fetchloom versions a project works with, written as uv's `required-version` is: comparisons such as `>=0.2.0`, joined by commas, as `">=0.2, <0.4"`. a comparison is `>=`, `>`, `<=`, `<`, `==`, `!=`, `~=` or `===` before a version of numbers and dots; `==0.2.*` and `!=0.2.*` compare only the numbers written, and `~=0.2.1` means 0.2.1 or newer, below 0.3. it goes at the top of data.toml, above every table:

```toml
required_version = ">=0.2.0"

[datasets.iris]
ref = "zenodo:3608990"
```

it is read before every other key, so a project that uses a key a newer fetchloom added says what it needs rather than that the key is unknown. a fetchloom outside the range stops every command that reads data.toml, before it reads or writes anything else, with `project.invalid`, exit 50:

```text
error: this project needs fetchloom >=0.2.0, and this one is 0.1.0
  data.toml:1   required_version = ">=0.2.0"
  help: update fetchloom to a version that matches >=0.2.0, as https://nikhil-yn-aix.github.io/fetchloom/user/start/install/ shows, then run `fetchloom status` again
```

a value that is no range is refused at its line like any other value: `required_version = "0.2.0" in data.toml is refused because 0.2.0 has no comparison before it, such as >=0.2.0 for 0.2.0 or newer`. a version with no comparison, which uv reads as `==`, is refused for that reason, and so is a pre-release such as `0.2.0rc1`, since fetchloom compares release numbers.

## encoding and validation

manifest files are UTF-8 TOML. a byte order mark is accepted on read and omitted on write. unknown keys fail; a nearby spelling is suggested when one is available.

diagnostics show only the line number when a cited line contains a reference, including inline tables, or when invalid TOML prevents identifying reference boundaries. reference text can hold credentials, so it is not copied into those diagnostics.

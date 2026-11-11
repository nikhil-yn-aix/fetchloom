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

`project` and `datasets` are optional tables. each dataset entry has these keys:

| key | meaning | default |
|---|---|---|
| `project.data_dir` | folder relative to the project root | `"data"` |
| `ref` | a reference from [sources](../sources/README.md) | required, nonempty string |
| `select` | keep files matching any of these patterns | all files; for a `ref` that links one file or folder inside a dataset, that file or everything below that folder |
| `exclude` | drop files matching any of these patterns | none |
| `extract` | unpack recognized archive names | `true` |
| `copy` | request writable placement by reflink or copy, never hardlink | `false` |
| `path` | folder relative to `data_dir`, or `"."` for the data folder itself | the dataset name |

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

`path` names a folder, never a file: `path = "train.csv"` makes a folder named `train.csv`, so the file lands at `data/train.csv/train.csv`. use `path = "."` to put it at `data/train.csv`.

datasets may share a folder, and one may sit in the data folder itself while others sit in folders below it, as long as no two files land on the same path. names are compared the way a disk that ignores case compares them, so `README` from one dataset and `readme` from another clash on every system, and so do two files with the same bytes. a clash names both datasets and the path:

```text
error: datasets test and train would both place data/splits/README
  help: give datasets.test or datasets.train another path in data.toml
```

sync and add refuse a clash before downloading anything when the sources list the file names, as most repositories and buckets do. a name only the server gives, as for a plain link, and the files inside an archive are checked once they are downloaded, still before anything is placed or replaced.

each placed file belongs to one dataset, so `remove` and sync delete only that dataset's unchanged files from a shared folder and leave the other datasets' files and your own.

fetchloom takes these folders as written and never expands `~` or a variable, since data.toml comes with whatever repository you clone. a `data_dir` or `path` that starts with `~` or `$`, or names a variable such as `$SCRATCH` or `${SCRATCH}`, is refused rather than made as a folder of that name. names such as `data~old` and `price$` stay usable. on a cluster, put the cache on scratch with `FETCHLOOM_CACHE`, or the whole project there, as [clusters and shared machines](../hpc.md) shows.

changing `data_dir` or a dataset's `path` moves where sync places its files. sync deletes only files it placed that are unchanged, and only inside the data folder, so a folder that still holds anything else stays, and so does the old data folder with every file in it; the sync that leaves one behind names it in a warning, as [sync](../commands/sync.md#folders-left-behind) shows.

`data_dir`, `path` and every placed file path refuse repository metadata components: `.git`, `.hg`, `.svn` and `.fetchloom`, ignoring case. this includes trailing dots or spaces, a `:` stream suffix, the windows short names `git~1`, `hg~1`, `svn~1` and `fetchl~1`, and spellings with the invisible code points hfs+ drops from names (U+200C to U+200F, U+202A to U+202E, U+206A to U+206F and U+FEFF), the set git checks for the same reason. the rule also covers source listings, unpacked archive members and paths inside a `get` folder. names such as `.github` and `.gitignore` remain usable.

file paths refuse control characters, including the bidi controls U+061C, U+200E, U+200F, U+202A to U+202E and U+2066 to U+2069. this covers manifest paths, source listings and archive members. server-supplied file names with bidi controls are refused too, including names decoded from `Content-Disposition`. ordinary right-to-left letters remain usable.

`add` keeps comments and table order, but cannot add to an inline `datasets` table. `remove` also removes comments attached to the deleted table and strips leading blank lines.

`copy = true` applies when a file is placed. setting it on a dataset makes the next sync replace each unchanged read-only file of that dataset with a writable one; `status` lists those files as outdated until then, and an edited file stays. an untracked read-only file with exactly the pinned bytes is also replaced with a writable file, without changing permissions through any other hardlink to it. differing untracked bytes still stay unless force is requested. turning it off leaves writable files as they are.

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

patterns select the files a source lists, before download and before a server-supplied name replaces a guessed name. they do not select members inside an archive. see [archives](../concepts.md#archives) for where those members land.

## encoding and validation

manifest files are UTF-8 TOML. a byte order mark is accepted on read and omitted on write. unknown keys fail; a nearby spelling is suggested when one is available.

diagnostics show only the line number when a cited line contains a reference, including inline tables, or when invalid TOML prevents identifying reference boundaries. reference text can hold credentials, so it is not copied into those diagnostics.

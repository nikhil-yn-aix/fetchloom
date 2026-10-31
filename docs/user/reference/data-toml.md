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
| `ref` | a reference from [scope](../sources/README.md) | required, nonempty string |
| `select` | keep files matching any of these patterns | all files |
| `exclude` | drop files matching any of these patterns | none |
| `extract` | unpack recognized archive names | `true` |
| `copy` | request writable placement by reflink or copy, never hardlink | `false` |
| `path` | folder relative to `data_dir` | the dataset name |

names are 1 to 64 ASCII characters: `a-z`, `0-9`, `-`, `_`. the first character must be a letter or digit.

without `--as`, fetchloom chooses a name in this order:

| available information | generated name |
|---|---|
| one file | its filename, limited to 40 characters |
| a title | at most its first four words and 32 characters, dropping trailing words such as `and`, `of` and `the` |
| a reference | its last part containing a letter; `hf:datasets/scikit-learn/iris` gives `iris`, and `zenodo:123` gives `zenodo-123` |

if the generated name is taken, fetchloom appends `-2`, `-3` and so on. an explicitly supplied `--as` name that is taken is refused.

`data_dir` and `path` use `/` on every platform. absolute paths, drive prefixes, backslashes, control characters, empty components, `.` and `..` are refused. trailing `/` is stripped. the data folder and its child folders must be real directories; see [placement](../../dev/architecture/README.md).

`data_dir`, `path` and every placed file path refuse repository metadata components: `.git`, `.hg`, `.svn` and `.fetchloom`, ignoring case. this includes trailing dots or spaces, a `:` stream suffix, and the windows short names `git~1`, `hg~1`, `svn~1` and `fetchl~1`. the rule also covers source listings, unpacked archive members and paths inside a `get` folder. names such as `.github` and `.gitignore` remain usable.

file paths refuse control characters, including the bidi controls U+061C, U+200E, U+200F, U+202A to U+202E and U+2066 to U+2069. this covers manifest paths, source listings and archive members. server-supplied file names with bidi controls are refused too, including names decoded from `Content-Disposition`. ordinary right-to-left letters remain usable.

`add` keeps comments and table order, but cannot add to an inline `datasets` table. `remove` also removes comments attached to the deleted table and strips leading blank lines.

`copy = true` applies when a file is placed. setting it on a dataset makes the next sync replace each unchanged read-only file of that dataset with a writable one; `status` lists those files as outdated until then, and an edited file stays. turning it off leaves writable files as they are.

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

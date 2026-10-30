# formats

`data.toml` and `data.lock` are UTF-8 TOML. both accept a byte order mark on read and omit it on write. unknown keys fail. the manifest suggests a nearby spelling when it finds one; the lock parser lists allowed fields.

## data.toml

```toml
[project]
data_dir = "data"

[datasets.iris]
ref = "zenodo:3608990"

[datasets.eeg]
ref = "osf:abc12"
select = ["**/*.edf", "participants.tsv"]
exclude = "**/derivatives/**"
extract = false
copy = true
path = "signals"
```

`project` and `datasets` are optional tables. each dataset entry has these keys:

| key | meaning | default |
|---|---|---|
| `project.data_dir` | folder relative to the project root | `"data"` |
| `ref` | a reference from [scope](scope.md#sources) | required, nonempty string |
| `select` | keep files matching any of these patterns | all files |
| `exclude` | drop files matching any of these patterns | none |
| `extract` | unpack recognized archive names | `true` |
| `copy` | request writable placement by reflink or copy, never hardlink | `false` |
| `path` | folder relative to `data_dir` | the dataset name |

names are 1 to 64 ASCII characters: `a-z`, `0-9`, `-`, `_`. the first character must be a letter or digit. generated names are at most 40 characters.

`data_dir` and `path` use `/` on every platform. absolute paths, drive prefixes, backslashes, control characters, empty components, `.` and `..` are refused. trailing `/` is stripped. the data folder and its child folders must be real directories; see [placement](architecture.md#placing-files).

`data_dir`, `path` and every placed file path refuse repository metadata components: `.git`, `.hg`, `.svn` and `.fetchloom`, ignoring case. this includes trailing dots or spaces, a `:` stream suffix, and the windows short names `git~1`, `hg~1`, `svn~1` and `fetchl~1`. the rule also covers source listings, unpacked archive members and paths inside a `get` folder. names such as `.github` and `.gitignore` remain usable.

file paths refuse control characters, including the bidi controls U+061C, U+200E, U+200F, U+202A to U+202E and U+2066 to U+2069. this covers manifest paths, source listings and archive members. server-supplied file names with bidi controls are refused too, including names decoded from `Content-Disposition`. ordinary right-to-left letters remain usable.

`add` keeps comments and table order, but cannot add to an inline `datasets` table. `remove` also removes comments attached to the deleted table and strips leading blank lines.

`copy = true` applies when a file is placed. setting it on a dataset makes the next sync replace each unchanged read-only file of that dataset with a writable one; `status` lists those files as outdated until then, and an edited file stays. turning it off leaves writable files as they are.

### patterns

`select` and `exclude` each accept a string or an array of strings. an empty array has the default meaning. matching is case-sensitive and covers the whole source path. leading `/` is ignored.

| pattern | matches |
|---|---|
| `*.csv` | CSV files at the top of the dataset |
| `**/*.csv` | CSV files at any depth, including the top |
| `sub-??/data.[ct]sv` | two characters after `sub-`, then `data.csv` or `data.tsv` |
| `**/[!0-9]*` | a file name not starting with a digit, at any depth |

`*`, `?` and character sets stay within one path component. sets accept ranges and `!` or `^` for negation. `**` must be a whole component. backslashes and empty components are errors; braces have no special meaning.

each select pattern must match at least one listed path before exclusions, including a google docs entry in a drive folder. an unmatched pattern shows one example path. a selection that leaves no files fails. listing limits apply before selection.

patterns select the files a source lists, before download and before a server-supplied name replaces a guessed name. they do not select members inside an archive. see [archives](cli.md#archives) for where those members land.

## data.lock

for a 32-byte file at `https://example.org/iris.csv`, with no filters:

```toml
version = 1

[[dataset]]
name = "iris"
ref = "https://example.org/iris.csv"
spec = "sha256:84ec087f04e4cd2214221199d51bd1e493d3589043f4e871b5af7ba7072181e8"
resolved = "https://example.org/iris.csv"
locked = "2026-10-28T00:00:00Z"
files = [
  { path = "iris.csv", size = 32, sha256 = "8b1ffd452ff4bb8e22402d04ac3f2eab66202dd50aee35e61d420bdd99b285eb", url = "https://example.org/iris.csv" },
]
```

| field | value |
|---|---|
| `version` | integer `1` |
| `dataset` | array of dataset tables; omitted for an empty lock |
| `dataset[].name` | name in `data.toml` |
| `dataset[].ref` | the manifest reference |
| `dataset[].spec` | `sha256:` followed by 64 hex digits, computed below |
| `dataset[].resolved` | the source reference used to list the files |
| `dataset[].title`, `license`, `doi` | optional strings supplied by the source; omitted when absent |
| `dataset[].locked` | UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ` |
| `dataset[].files` | selected source files, including archives as single files |
| `files[].path` | relative path inside the dataset, with `/` separators |
| `files[].size` | byte count, unsigned 64-bit integer |
| `files[].sha256` | 64 hex digits, without a prefix |
| `files[].md5` | optional source checksum, 32 hex digits |
| `files[].url` | download URL recorded before redirects |
| `files[].unpacks_to` | optional `sha256:` plus the 64-digit archive tree hash |

hashes are written in lowercase. datasets sort by name and files by path. the writer uses the field order above, inline file tables, LF newlines and a final newline. identical lock contents render identically; a fresh resolution can have a different timestamp.

`locked` stays unchanged when `resolved` and `files` stay unchanged, even after re-resolution. `lock` can record a file without downloading it when its SHA-256 and size are known. an archive needs a tree hash before the lock can mark it for extraction.

a newer lock version asks you to update fetchloom. a broken or older lock must be restored from git, or deleted before `fetchloom lock` rebuilds it. rebuilding can pin newer data.

### the spec hash

SHA-256 hashes this UTF-8 text, with a final newline:

```text
ref=<manifest ref>
select=<first select pattern>
select=<next select pattern>
exclude=<first exclude pattern>
extract=true
```

there is one line per pattern, in written order; empty pattern lists add no lines. the last value is `true` or `false`. `copy`, `path` and `project.data_dir` are excluded.

an unchanged spec keeps the existing lock entry unless an upgrade is requested. filter or extraction edits resolve the existing `resolved` reference. changing `ref`, deleting the lock or requesting `--upgrade` resolves the manifest reference instead. an explicitly versioned reference still names that version.

`resolved` is not always immutable. plain URLs, drive links, dropbox links and OSF nodes can change. in particular, changing filters on an OSF node can select newer file versions. the file hashes pin the bytes after resolution.

### what each source records

| source | `resolved` | file `url` |
|---|---|---|
| plain URL | the parsed URL | the same URL |
| zenodo | `zenodo:<concrete record id>` | `https://zenodo.org/api/records/<id>/files/<name>/content` |
| figshare | `figshare:<id>@<version>` | the API's download URL |
| dataverse dataset | `dataverse:<host>/<persistent id>@<major.minor>`; the id includes `doi:` or `hdl:` | `/api/access/datafile/<id>?format=original` on that host |
| dataverse file | `dataverse:<host>/file:<numeric id>@<major.minor>`; a file PID resolves to its numeric id | `/api/access/datafile/<id>?format=original` on that host |
| OSF | `osf:<node or file id>` | the download link with a file version |
| drive file or folder | `drive:<id>` or `drive:folders/<id>`, with `?resourcekey=<key>` when supplied; `open?id=` resolves to one of these | `https://drive.google.com/uc?id=<id>`, with `resourcekey` when supplied |
| dropbox | the shared URL with `dl=1` | the same URL |
| hugging face | `hf:<kind>/<repo>@<commit>` | the repository's `/resolve/<commit>/<path>` URL on `huggingface.co` |
| kaggle | `kaggle:<owner>/<dataset>@<version>` | `https://api.kaggle.com/v1/datasets/download/<owner>/<dataset>/<file>?datasetVersionNumber=<version>&raw=true` |

DOIs route to one of these sources. hugging face resolves a branch or tag to its full commit id before listing files. kaggle encodes each file path as one URL segment.

drive locks keep the file id and resource key, without a confirmation form's fields. transfers start at `https://drive.usercontent.google.com/download?id=<id>&export=download&confirm=t`, with the resource key when needed. a confirmation page can supply the next request without changing the lock URL.

drive folders list no sizes. downloads supply the sizes written to the lock; [approval](cli.md#offline-recovery-and-approval) checks a server's length, or the bytes received when no length is stated. google docs exports can change between downloads. select and exclude match those entries too; only a doc that remains selected stops the command. the error names each selected doc and explains how to exclude it, or you can ask the owner to upload a fixed file.

dropbox folders arrive as one zip, named by `Content-Disposition`. the lock pins the zip; extraction adds its tree hash. an unnamed HTML answer fails instead of entering the cache as dataset bytes.

source metadata keeps its casing. zenodo's iris fixture, for example, supplies `license = "cc-by-4.0"`. supported stated checksums are MD5 and SHA-256; dataverse ignores other checksum algorithms and learns SHA-256 from the download.

when adding or resolving a plain URL, project commands reject login details and recognized secret query keys. `get` permits secret query values for one-off downloads. its JSON file URLs and errors mask recognized query secrets, including errors for malformed links. local URL records keep the raw links, so do not share those records. masking recognizes key names such as `token`, `signature` and `sig`; it cannot identify an arbitrary secret.

### the tree hash

`unpacked/<archive sha256>` contains one UTF-8 line per file, sorted by path:

```text
<path>\t<size in bytes>\t<file sha256>\n
```

`\t` and `\n` above mean a literal tab and LF. paths are NFC, with `/` separators and leading `./` removed. SHA-256 of these exact bytes is `unpacks_to`. there is no header or byte order mark. an empty tree hashes zero bytes.

paths in this record retain their archive folders; placement may strip a shared top folder. directory entries, times and permissions are absent. empty directories are not placed.

## the cache

| platform | default root |
|---|---|
| linux | `$XDG_CACHE_HOME/fetchloom`, else `$HOME/.cache/fetchloom` |
| windows | `%LOCALAPPDATA%\fetchloom` |

nonempty `FETCHLOOM_CACHE` overrides the root and may be relative to the working directory after `-C`.

```text
<cache>/
  CACHEDIR.TAG
  layout
  objects/<first 2 sha256 digits>/<remaining 62 digits>
  partial/<sha256 of URL>.part
  partial/<sha256 of URL>.json
  partial/<sha256 of URL>.lock
  urls/<sha256 of URL>.json
  stated/md5/<first 2 md5 digits>/<remaining 30 digits>
  unpacked/<archive sha256>
  projects/<sha256 of project root>.json
  changed/<object sha256>
  tmp/
```

`layout` is created with `1\n`. an existing value is not validated or migrated. `CACHEDIR.TAG` starts with `Signature: 8a477f597d28d172789f06886806bc55` to let backup tools skip the cache.

objects hold read-only payloads. `tmp/` holds new archive members, atomic record writes, private downloads when file locking is unavailable, and the empty files that test whether the volume can hardlink. these records are local cache indexes, not files to commit.

### URL and partial records

`urls/` records are JSON written for revalidated, unpinned URLs:

| field | value |
|---|---|
| `url` | original download URL, also the UTF-8 input to the filename hash |
| `sha256` | full object hash |
| `size` | total bytes |
| `name` | server-supplied file name, or null |
| `validators.etag` | `ETag` string, or null |
| `validators.last_modified` | `Last-Modified` string, or null |

partial JSON records have `url`, `received`, `sha256`, `length` and the same `validators` object. `received` is the checkpoint byte count; `sha256` hashes that prefix; `length` is the total file size stated by the response, or null. progress is saved about once a second while writing. resumption truncates bytes past the checkpoint and rehashes the prefix.

the `.lock` file coordinates writers; its contents are unused. gc removes an abandoned download's `.part` and `.json` but keeps its `.lock`, since a run waiting for that download may already have it open. without OS file locks, downloads use unique names under `tmp/` and cannot resume across runs.

### other indexes

- an MD5 mapping is `<sha256> <size>\n`, with a literal space and LF. it records a previously checked MD5, including when its object is no longer cached.
- an unpack record uses the tree encoding above. its hash is checked against `data.lock` when one is supplied, and a sync writes only a tree that matches the lock's `unpacks_to`; a cache hit for the whole archive also requires every member object.
- a project registration is JSON with one field, `root`, the project path. its filename hashes the path's OS-encoded bytes. registrations are written after successful project sync, including sync through `add` or `path`. `get` and standalone `lock` do not register.
- a change mark is an empty file named by an object's SHA-256. it is written when fetchloom finds that object's bytes changed: in a project sync's hardlink audit, in the hash a marked or writable object gets before it is placed, in an archive that unpacked to another tree than data.lock pins, in `cache verify`, or when a new copy of an object meets an old one of another size. its modification time records when. gc removes the marks of objects no registered project needs. [placement](architecture.md#placing-files) explains what other projects do with a mark.

archive bytes may be deleted after successful unpacking when no registered project or current lock needs them packed. deletion failures or unreadable project locks leave them cached. members remain ordinary objects. [cache commands](cli.md#cache) explain retention and gc.

## the .fetchloom folder

`.fetchloom` must be a real directory. a symlink or junction is refused before fetchloom writes its lock, state or ignore file.

| path | contents |
|---|---|
| `.fetchloom/.gitignore` | `*` and LF, written when the folder is created |
| `.fetchloom/lock` | OS project lock; file contents are unused |
| `.fetchloom/state` | compact JSON describing tracked placements |
| `.fetchloom/.<name>.<8 hex digits>.tmp` | metadata being written; a hard kill can leave this file behind |
| `.fetchloom/.fetchloom-<16 hex digits>.probe` | an empty hidden file, there only while a removal tests whether the volume can hardlink |

inside a git repository, `add` adds `/<data_dir>/` to the project's `.gitignore` if absent. `.fetchloom/` ignores itself separately.

state fields:

| field | value |
|---|---|
| `version` | integer `1` |
| `files` | object keyed by project-relative path, using `/` |
| `files[path].dataset` | dataset name |
| `files[path].sha256` | locked member or file hash, 64 hex digits |
| `files[path].method` | `reflink`, `hardlink` or `copy`. an adopted file is `hardlink` when it is the cached object's own file, else `copy` |
| `files[path].look.size` | byte count |
| `files[path].look.modified` | modification time in nanoseconds since 1970; zero if unavailable, capped at the largest unsigned 64-bit integer |
| `files[path].writable` | `true` when the file was placed or adopted writable |
| `files[path].checked` | start of the scan used to place, adopt or vouch for the file, in nanoseconds since 1970; marks written during that run still put the file in doubt |
| `synced` | absent until a `sync` records it. `manifest` and `lock` hold the SHA-256 of `data.toml` and `data.lock` as that sync left them, in hex, and `complete` is `true` when every file they ask for ended up in place; [the project loop](architecture.md#the-project-loop) uses it to skip planning |

there is no inode or file-id field. missing, unreadable, malformed or unsupported-version state counts as empty. matching existing files can then be adopted into state and later removed as managed files. metadata comparisons do not prove content integrity.

a file whose `look.modified` is not older than the state file's own modification time is hashed before its look is trusted, since both times come from one filesystem clock and a coarse clock hides an edit made in the same tick. a hash that differs counts as an edit. a sync that finds such files unchanged writes the state again, which settles them.

## settings

there is no config file. [CLI options](cli.md#options) set per-run behavior.

| variable | effect |
|---|---|
| `FETCHLOOM_CACHE` | nonempty cache root override |
| `FETCHLOOM_OFFLINE` | exactly `1` enables offline mode; `--offline` also enables it. offline recovery hints name the flag, variable or both, whichever enabled it |
| `FETCHLOOM_JOBS` | integer from 1 to 256; unset or empty means 8; surrounding whitespace is accepted |
| `FETCHLOOM_LOG` | tracing target filter, such as `fetchloom=debug`; overrides the `-vv` default filter |
| `FETCHLOOM_TRACE` | nonempty path for a chrome trace; relative to the starting directory, before `-C` |
| `NO_COLOR` | nonempty value disables automatic color |
| `CLICOLOR_FORCE` | nonempty value other than `0` forces automatic color unless `NO_COLOR` wins |
| `CLICOLOR` | `0` disables automatic color |
| `TERM` | `dumb` disables automatic color, unicode symbols and interactivity |
| `CI` | nonempty value disables automatic color and interactivity |
| `WT_SESSION`, `TERM_PROGRAM`, `ConEmuANSI` | presence enables unicode in a windows terminal |

explicit `--color always` or `--color never` overrides color variables, not interactivity. in automatic mode the color order is `NO_COLOR`, `CLICOLOR_FORCE`, `CLICOLOR=0`, `TERM=dumb`, `CI`, then whether stderr is a terminal.

an invalid jobs value fails a named command even when it does not download. welcome, bare-project status, help and version bypass that check. an invalid log filter falls back to `-vv`'s default, if requested. explicit logging can write stderr in quiet or JSON mode and disables progress bars.

synced-folder detection reads `OneDrive`, `OneDriveConsumer`, `OneDriveCommercial`, dropbox's `info.json` under `APPDATA`, `LOCALAPPDATA` or the home folder, and the icloud drive folder on windows. system proxy settings and proxy environment variables are read by the HTTP stack.

credentials are read from where each source already keeps them: `HF_TOKEN` or `~/.cache/huggingface/token`, `KAGGLE_API_TOKEN` or `~/.kaggle/access_token`, else `KAGGLE_USERNAME` and `KAGGLE_KEY` or `kaggle.json` in `KAGGLE_CONFIG_DIR` or `~/.kaggle`, `ZENODO_TOKEN`, `FIGSHARE_TOKEN`, `OSF_TOKEN`. a token never appears in `data.toml`, `data.lock`, logs or `--json` output.

`DATAVERSE_TOKEN` is not read. use public dataverse links; an access refusal needs a public copy from the dataset's owner.

## the JSON document

`--json` prints one document on stdout, including usage errors, welcome and version. `--help` still prints text. explicit debug logging may also write stderr. JSON mode never prompts; use `--yes` when approval is needed.

| field | value |
|---|---|
| `schema` | integer `1` |
| `command` | `add`, `remove`, `sync`, `lock`, `status`, `path`, `get`, `cache`, `cache gc`, `cache verify`, or null |
| `status` | `ok` or `error` |
| `result` | command result on success; null on failure |
| `warnings` | array of strings, including in quiet mode |
| `errors` | empty on success; one error object on failure |
| `errors[].kind` | one of the names in [exit codes](#exit-codes) |
| `errors[].message` | summary string |
| `errors[].details` | array of supporting strings |
| `errors[].help` | string or null |
| `work` | counters below, present on success and failure |

there are no structured dataset or path fields in an error. SHA-256 mismatch details contain only the first 12 hex digits, even with `--json` or `-v`. parse `kind` and result fields rather than human messages.

bare `fetchloom` inside a project reports command `status`. outside a project, and for `--version`, command is null and result is `{"version":"0.1.0"}`. on a usage error, command is the prefix clap recognized, or null. `--json` is recognized anywhere before `--` on that error path; other display flags depend on what clap read.

### command results

| command | result fields |
|---|---|
| `add`, `remove`, `sync`, `lock` | `project`: absolute path; `datasets`: name array; `lock_changed`: boolean; `placement`: object below or null |
| `get` | `folder`: absolute path; `files`: file records below |
| `path` | absolute path string, for a dataset, file or subfolder |
| `status` | `project`: absolute path; eight arrays below |
| `cache` | `path`: configured cache root; `objects`: count; `bytes`: payload bytes; `needed`: cached payload bytes needed by registered locks; `projects`: root-path array |
| `cache gc` | `objects`: deleted count; `bytes`: deleted object payload bytes; `leftovers`: deleted partial or temporary file count |
| `cache verify` | `objects`: examined count; `bytes`: examined payload bytes; `changed`: hash array; `stuck`: hash array; `unreadable`: hash array |

project `datasets` names what was added or removed, or existing datasets whose resolved reference or files changed during `lock`. ordinary `sync` and duplicate `add` return an empty array. `remove` always reports `lock_changed = true`, including when the removed dataset had no lock entry. `lock` has null placement. successful `lock --check` also has empty datasets and `lock_changed = false`.

placement has integer counts `placed`, `replaced`, `removed` and `unchanged`; `kept`, an array of `{path, reason}`; and `restored`, an array of replaced paths whose bytes another project changed through a shared hardlink. paths are project-relative. `kept` reasons are `changed` for edited managed files, `foreign` for other bytes at a target, and `unneeded` for edited managed files no longer wanted. adopted identical files count as unchanged. unrelated extra files are not included in `kept`.

`get.files` lists placement targets, so an unpacked archive is replaced by its members. each record has `path`, `size`, `sha256`, `url`, `origin`, `method`, `unpacked_from`. the path is relative to `folder`. origin is `network`, `not_modified` or `cache`; method is `reflink`, `hardlink`, `copy`, or null when already present. `unpacked_from` is the selected archive path or null. unpacked members inherit the archive's URL and origin.

status arrays:

| field | entries |
|---|---|
| `unlocked` | dataset names missing from the lock or with a changed spec |
| `missing` | absent target paths |
| `edited` | managed paths whose size or modification time changed |
| `untracked` | existing target paths without state records |
| `outdated` | unchanged managed paths that `sync` replaces: pinned to other hashes now, or read-only where the dataset asks for `copy = true` |
| `edited_elsewhere` | managed hardlinks showing their recorded size and modification time, whose object the cache marked as changed after fetchloom last knew them, and whose bytes hashing found changed; `sync` puts them back |
| `extra` | scanned paths outside the known targets |
| `not_unpacked` | archive paths whose member records are unavailable on this machine |

status lists all differences in JSON. human output limits each file category to 20 paths; unlocked datasets and not-unpacked archives are not truncated. without an archive record, files already placed from it can also appear as extra.

`status` finding differences and `sync` keeping conflicts still return `ok` and exit 0. inspect the arrays. `cache verify` finding changed bytes returns `integrity.mismatch` and exit 30; when no bytes changed but an object could not be read, it returns `local.io` and exit 50. either error has a null `result`. unreadable objects are named separately in `warnings` and are neither marked nor removed. successful `changed`, `stuck` and `unreadable` arrays are empty.

### work counters

all counts and byte quantities are unsigned integers. timestamps and large counters can exceed javascript's exact integer range.

| field | counts |
|---|---|
| `requests` | explicit metadata or file request attempts, including retries; redirect hops are not counted separately |
| `bytes_down` | body bytes read, including metadata and unsuccessful attempts; excludes headers and the saved resume prefix |
| `retries` | scheduled retry waits |
| `resumes` | accepted nonzero range starts |
| `objects_written` | new objects counted after successful downloads and archive unpacking |
| `cache_hits` | download-layer hash or MD5 shortcuts, not every object used by placement |
| `reflinks`, `hardlinks`, `copies` | new placements by each method, including replacements |
| `elapsed_ms` | elapsed whole milliseconds before rendering the final report |

a warm project can place cached files with zero cache hits. there are no phase timings or bytes-written counters. cache scans silently omit directory and metadata read failures, so their counts can cover fewer objects than the cache contains.

### exit codes

| exit | error kinds |
|---|---|
| 0 | success |
| 1 | `unconfirmed`, `lock.outdated` |
| 2 | `usage` |
| 10 | `reference.invalid`, `reference.unresolved`, `source.invalid` |
| 20 | `network.unreachable`, `network.status`, `network.rate_limited`, `network.refused`, `offline.missing` |
| 30 | `integrity.mismatch`, including size mismatches and changed bytes found by cache verification |
| 40 | `auth.required` |
| 50 | `local.io`, `local.not_empty`, `project.invalid` |
| 130 | `interrupted` |

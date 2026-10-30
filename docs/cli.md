# cli

## a project

```sh
fetchloom add zenodo:3608990
fetchloom path iris
fetchloom sync
```

`add` creates `data.toml` if needed, pins the selected files in `data.lock`, and syncs the project. `path` prints the absolute dataset path on stdout. commit both TOML files, not the data folder.

when every managed file is already in place:

```text
$ fetchloom sync
✓ everything already matches data.lock. nothing to do
```

project commands use the nearest `data.toml` at or above the working directory. `-C <dir>` changes where that search starts. bare `fetchloom` shows a short status inside a project and a welcome outside one.

### add and remove

```sh
fetchloom add zenodo:3608990 --as flowers --select "*.csv"
fetchloom remove flowers
```

`--select` and `--exclude` may be repeated. quote patterns so your shell leaves them alone. a pasted zenodo file link selects that file unless you give `--select`. accepted source references are in [scope](scope.md#sources).

adding an existing reference with the same filters and extraction setting syncs it instead of adding another entry. different settings fail unless `--as` supplies an unused name. edit `data.toml` to change an existing entry.

`remove` changes the manifest and lock, then removes unchanged tracked files no longer needed. it downloads nothing. edited tracked files stay with a warning; unrelated extra files stay without a removal report. a pre-existing file adopted because its bytes matched the lock counts as tracked too.

if syncing a new addition fails, `add` attempts to restore the old manifest. cache, lock and placement changes are not all rolled back.

### lock and upgrade

```sh
fetchloom lock
fetchloom lock --check
fetchloom lock --upgrade iris flowers
fetchloom sync
```

`lock` updates pins without placing files in `data/`. it downloads files whose hashes are unknown and archives whose unpacked tree is unknown. already pinned ordinary files need not be present in the cache.

`--check` changes nothing and checks manifest membership and spec hashes only. success is silent at normal verbosity; a stale lock exits 1. it does not verify file contents.

`--upgrade` with no names considers every dataset. it resolves the manifest references again and revalidates unchecksummed URLs. an explicitly pinned version stays pinned. upgrading changes the lock; run `sync` afterward to place the new bytes. reference edits and mutable sources have the [pinning limits](formats.md#the-spec-hash) described in formats.

### status and hand edits

if `iris.csv` is missing and you added `notes.txt`:

```text
$ fetchloom status
    Missing data/iris/iris.csv
      Extra data/iris/notes.txt
✓ found 2 differences from data.lock
  hint: `fetchloom sync` brings the rest in line. edited and extra files stay as they are
```

`status` compares recorded sizes and modification times, without downloading. it hashes three kinds of file. one is a file whose recorded time is not older than `.fetchloom/state` itself: a clock as coarse as FAT's two seconds gives a file edited just after it was placed the same size and time. `sync` writes the state again once such files prove unchanged, so later runs hash nothing. another is a changed file of the locked size, which counts as placed when it holds the locked bytes: a run cut off after it replaced a file and before it wrote the state leaves the new bytes recorded as the old file. the last is a hardlink whose cached object was marked as changed since fetchloom last knew the file; `sync` records the ones that prove intact, so they are hashed once. `status` also reports edited, untracked and outdated paths, hardlinks changed through another project, changed manifest entries, and archives not yet unpacked here. [JSON status fields](formats.md#command-results) define each category.

on windows, an edit through another project's hardlink can stay invisible to your project's `status` until that project syncs, `cache verify` runs, or `sync --force` reads each hardlink by name.

`sync` keeps edited or conflicting files and warns. it can still exit 0, so scripts must inspect `placement.kept`. a hardlink `status` lists as changed through another project is put back instead, with a warning, and listed in `placement.restored`. `sync --force` replaces conflicts at locked destinations, including files fetchloom did not create. it does not delete unrelated extras or edited files no longer needed.

default placements are read-only. for tools that write into their inputs, request `copy = true` in the dataset entry. see [placement and cache trust](architecture.md#placing-files) before changing a hardlink's permissions.

### path

```sh
fetchloom path iris
fetchloom path iris/iris.csv
fetchloom path eeg/sub-01
```

`path` accepts a dataset, a file, or a subfolder containing locked files. it syncs the whole project when the requested dataset is not ready. a ready dataset takes no project lock. a waiting caller checks again after acquiring the lock.

`path` exits 2 when the dataset has no regular files after extraction and explains how to keep the packed archive instead. `add` reports that its archives contain no regular files and omits the path hint.

`path` prints the location, not an integrity check. sync can leave an edited file there.

## get without a project

```sh
fetchloom get zenodo:3608990
fetchloom get https://example.org/files/train.zip -o training
fetchloom get https://example.org/files/train.zip --no-extract
```

`get` uses the same cache without writing a manifest, lock or project state. stdout is the absolute output folder. a single file uses its filename stem for the folder; several files use a name made from the dataset title or reference. `-o` selects a folder instead. the output folder may be a link to a folder elsewhere; links below it are refused.

for direct downloads, a `Content-Disposition` filename replaces the URL's guessed name. unsafe filename characters are replaced. names are shortened to 200 bytes before a reserved windows device name gets an extra `_` prefix.

on another run, a known SHA-256 or MD5 can find cached bytes. an unpinned URL is checked again with `ETag` or `Last-Modified` validators when available. a 304 response downloads no file body; without validators the file may be downloaded again. changed bytes are accepted by `get`, unlike a locked project. repository references may still need metadata requests even when all file bytes are cached.

a destination with unrelated top-level entries is refused. hidden `.fetchloom-<16 hex digits>.copy` files left by interrupted copies are ignored. nested extras beneath an expected folder can remain. an existing target is kept when it matches; other contents can be replaced only when those bytes also exist in the cache. this is a cache-membership check, not a record of who created the file. `get` has no `--force`; use another folder when you need to keep its contents.

in drive folders, `--select` and `--exclude` match google docs entries as well as stored files. a selected doc is refused because its exported bytes can change between downloads. the error lists every selected doc. for up to five entries it gives exact exclusion flags, such as `--exclude "gdown"`; for more, it asks for an exclusion for each entry. `add` records that exclusion in `data.toml`. selecting only a doc gives the same refusal; excluding every entry still gives the empty-selection error.

## archives

extraction is enabled by default for names ending in `.zip`, `.tar`, `.tar.gz`, `.tar.xz`, `.tar.zst`, `.tar.bz2`, `.tgz`, `.txz`, `.tzst`, `.tbz` or `.tbz2`, ignoring case. the bytes then decide which supported decoder to use.

other names stay intact, including `.fastq.gz`, `.xlsx`, `.npz`, `.pt` and extensionless downloads. `.7z` and `.rar` stay packed with a warning when extraction is enabled. a recognized name whose bytes are not a supported archive also stays intact with a warning. an archive recognized but found unsafe or damaged fails extraction.

`get --no-extract` keeps the download packed. `add --no-extract` writes `extract = false`; existing projects use that manifest setting. select and exclude patterns apply to the listed archives, not their members. nested archives are not unpacked recursively.

paths below are relative to the dataset folder, or the `get` output folder:

| selected files | members in `train.zip` | placed paths |
|---|---|---|
| only `train.zip` | `train/a.csv`, `train/b.csv` | `a.csv`, `b.csv` |
| only `raw/train.zip` | `train/a.csv` | `raw/a.csv` |
| `train.zip` and another file | `train/a.csv`, `train/b.csv` | `train/a.csv`, `train/b.csv` |
| `train.zip` and another file | only `a.csv` | `a.csv` |
| `train.zip` and another file | `a.csv`, `b.csv` | `train/a.csv`, `train/b.csv` |

a sole archive loses one shared top folder. beside other selected files, a shared top folder or lone top-level file comes out as named; otherwise the members go under the archive's stem. the archive's parent path is retained. colliding destinations fail. an archive with no regular-file members creates no dataset folder in a project; `get` still makes its output folder, empty.

members enter the cache before placement. the packed object can then be removed unless a registered project still needs it packed. see [archive checks and limits](architecture.md#archives) and the [tree record](formats.md#the-tree-hash).

## offline, recovery and approval

```sh
fetchloom sync --offline
fetchloom sync --yes
```

`--offline` prevents HTTP and DNS requests by fetchloom. a locked project can use unchanged placed files, cached objects and unpack records. an entry that needs resolution fails offline, even for a plain URL. offline `get` of a direct URL needs its URL record and cached bytes; a repository listing is not cached for offline resolution.

when a locked file's download returns 404 or 410, fetchloom asks its recorded source reference again. a changed download URL is tried with the original size and hashes. the lock is not rewritten. an unchanged URL, another error kind or a plain URL with no new location cannot recover this way.

file-specific failures can be collected while other downloads finish; network, disk and interruption failures stop the batch. an aggregate error names at most ten failed files, and an offline project error at most five missing files. successful downloads stay cached.

resumption needs a saved prefix and either a pinned SHA-256, a strong `ETag`, or a `Last-Modified` from a server that sent no `ETag`. the prefix is hashed again. unsupported ranges, changed validators or damaged saved bytes can force a restart. a hard kill can lose work since the last checkpoint.

approval is required above 1 GiB, not at exactly 1 GiB:

- `get` checks the total known selected size, including cache hits.
- project downloads check known bytes still missing from the cache.
- for an unknown source size, a server's length is checked against the remaining download. if neither states a size, the question comes after that file passes 1 GiB. unknown sizes are checked per file, with at most one such question per batch. other large downloads with unknown source sizes wait for the same answer; a refusal stops them too.

`--yes` or `-y` approves. questions default to no and require interactive stdin and stderr. pipes, CI and JSON mode cannot answer:

```text
✗ downloading huge.bin needs 2.0 GiB, and there is no terminal to ask first
  help: add --yes to download anyway
```

at a download question, the first ctrl-c stops the command with the interrupted error and exit code 130. during other asynchronous work, the first ctrl-c asks it to stop and keep resumable bytes. a second exits immediately with code 130.

## cache

```sh
fetchloom cache
fetchloom cache gc
fetchloom cache verify
```

`cache` prints its root on stdout and reports object bytes and registered projects on stderr. byte totals exclude indexes and partial downloads.

`cache gc` keeps what registered project locks need, including unpacked members. unneeded objects and unpack records become eligible after 24 hours by modification time, not last use. abandoned partials and temporary files are cleaned too. `get` and standalone `lock` do not register projects for retention.

a missing manifest removes a project's registration. an unreadable lock stops inspection or gc rather than guessing what it needs. removing a cache object leaves placed hardlinks, reflinks and copies intact.

`cache verify` rehashes objects, marks and removes those whose bytes differ, and exits 30 if it found any. removal failures are reported too. unreadable objects get their own warning and stay in the cache without a new change mark; when none changed, an unreadable object makes the command exit 50. close any program using the object or check its permissions, then verify again. it does not verify independent copies under `data/`.

## options

| option | effect |
|---|---|
| `-q`, `--quiet` | keep stdout answers and errors; hide the story; interactive questions still apply |
| `-v`, `--verbose` | add details, error kinds and aggregate counters |
| `-vv` | also enable debug logs for fetchloom |
| `--json` | one result document on stdout |
| `--offline` | use local data only |
| `-y`, `--yes` | approve downloads without asking |
| `--color <when>` | `auto` (default), `always` or `never` |
| `-C <dir>` | change the working directory before the command |
| `-h`, `--help` | print help, including after a command |
| `-V`, `--version` | print the version; root-level option |

`-q` conflicts with verbosity. `lock --check` conflicts with `--upgrade`. `fetchloom help concepts` explains data.toml, data.lock, the cache and sync; `fetchloom help <command>` prints the same text as `fetchloom <command> --help`. `fl` runs the same implementation; powershell's `fl` alias means `Format-List`, so use `fetchloom` there.

help follows the same color settings as other output, including `--color` before or after a subcommand or help flag. `--color always` enables color and `--color never` disables it. with `--color auto`, a non-empty `NO_COLOR` disables color even when `CLICOLOR_FORCE` is set.

command-specific options are `add --as <name>`, `get -o/--output <dir>`, repeated `--select <glob>` and `--exclude <glob>` on both, `--no-extract` on both, `sync --force`, and `lock --upgrade [name ...]` or `--check`. defaults come from [settings](formats.md#settings).

## output and errors

stdout carries paths, version, welcome, help or JSON. stderr carries progress, warnings, summaries and errors. a ready `path` needs no closing summary; successful `lock --check` is silent unless verbose or JSON output was requested.

when stdout and stderr are both terminals, `add`, `remove`, `get`, a `lock` that changed data.lock and a `sync` that placed files or changed data.lock end with a dim `next:` line naming the usual next step, such as committing data.toml and data.lock. `-q`, `--json` and a redirected stdout leave it out.

resolved lines omit unavailable metadata, show total size only when every listed size is known, and shorten titles to 50 characters plus an ellipsis. plain URLs have no resolved line. licenses containing `-NC` or `-ND`, ignoring case, get a warning; other restrictions are not interpreted.

on an interactive terminal, resolution waits show spinners and active downloads show bars. several downloads get an overall bar. elapsed time appears after two seconds. after three seconds, a positive rate is shown; remaining time also needs a known total. on narrow terminals, counters and the bar disappear before the file name is shortened. a name that still does not fit keeps its beginning and end with `~` in the middle; the line stays within the terminal width. finished bars clear and the command prints its summaries. debug logging disables bars.

without bars, long resolution, download, unpack and verify batches print a line after two seconds, then at ten-second intervals from the start. quiet and JSON modes suppress these lines. color and unicode follow the [environment rules](formats.md#settings): color is off for pipes and files, with `NO_COLOR` and with `--color never`.

with color on, success is green with ✓, warnings yellow with !, errors red with ✗, names and commands cyan, and details dim. each color comes with a symbol, a label, a verb or backticks, so nothing depends on color alone.

```text
$ fetchloom get
✗ <link> is missing
  usage: fetchloom get [options] <link>
  help: `fetchloom get --help` says what it takes
```

when a download no longer matches its lock:

```text
✗ iris/iris.csv is not the file that was locked
  expected 32 bytes, got 15
  help: if the new version is intended, run `fetchloom lock --upgrade iris`
```

a separate help line is optional when the error itself says what to change or do next. help that says to run a command again names it as typed, such as ``run `fetchloom sync` again``, unless an argument holds a secret. likely spelling mistakes get suggestions; other unknown dataset names list the available names. binary sizes use one decimal place (`4.1 KiB`), durations below a second use milliseconds, and counts group thousands (`1,204 files`). error kinds, exit codes and machine-readable results are in [formats](formats.md#the-json-document).

# cli

## a project

project commands find the nearest `data.toml` at or above your working directory. use `-C <dir>` to start elsewhere. `add` creates a manifest in the current folder when none is found. bare `fetchloom` shows status inside a project and a welcome outside one.

commit `data.toml` and `data.lock`, not the downloaded files. [formats](formats.md#datatoml) defines dataset names, paths and manifest settings; [scope](scope.md#sources) lists accepted source references.

### add and remove

```sh
fetchloom add zenodo:3608990
fetchloom add zenodo:3608990 --as flowers --select "*.csv"
fetchloom remove flowers
```

`add <link>` adds a dataset to `data.toml`, pins its files in `data.lock` and syncs the whole project. it adds the data folder to `.gitignore` inside a git repository. `--as <name>` chooses the dataset name and, by default, its folder under `data/`.

| option on `add` and `get` | effect |
|---|---|
| `--select <glob>` | keep files matching a pattern; repeat to match any of several patterns |
| `--exclude <glob>` | drop matching files; repeat for more exclusions |
| `--no-extract` | keep archives packed; `add` records `extract = false` |

quote patterns so your shell passes them unchanged. they match source paths before download, not archive members. each select pattern must match at least one path before exclusions, and some files must remain. [patterns](formats.md#patterns) defines matching. a zenodo file-page link selects that file unless you supply `--select`.

adding an existing reference with the same filters and extraction setting syncs it without another entry. different settings fail unless you supply an unused `--as` name. to change an existing entry, edit `data.toml`.

`remove <name>` removes one dataset from the manifest and lock, then deletes its unchanged managed files. it downloads nothing. edited files stay with a warning; unrelated extra files stay too. an existing file adopted because it matched the lock counts as managed.

if a new addition fails to sync, `add` attempts to restore the previous manifest. it does not roll back every cache, lock or placement change.

### sync

```sh
fetchloom sync
fetchloom sync --offline
fetchloom sync --force
```

`sync` places the files pinned in `data.lock`. if the manifest changed or has no lock yet, it updates the lock first. unchanged entries keep their pins. it reuses local files and cached bytes, downloading what is missing when online.

files are read-only by default. placement tries a reflink, then a hardlink, then a copy, depending on the filesystem. recognized synced folders skip hardlinks. for writable inputs, set `copy = true` on the dataset in `data.toml`, then sync. this uses a writable reflink or copy, never a hardlink. [placement](dev/architecture.md#place) explains sharing and cache trust.

`--force` can replace edited or untracked files at locked paths. save anything you need first. it does not delete unrelated extras or edited files no longer needed. without it, sync keeps conflicts and reports them. see [status and hand edits](#status-and-hand-edits).

### lock and upgrade

```sh
fetchloom lock
fetchloom lock --check
fetchloom lock --upgrade iris flowers
fetchloom lock --upgrade
```

`lock` updates `data.lock` without placing files. it may download files to learn their hashes or an archive's unpacked tree. already pinned ordinary files need not be in the cache.

| option | effect |
|---|---|
| `--check` | check manifest membership and spec hashes without changing anything; a stale lock exits 1 |
| `--upgrade [name ...]` | resolve those datasets again; with no names, consider every dataset |

`--check` is silent on success at normal verbosity. it does not check file contents. it conflicts with `--upgrade`.

`--upgrade` revalidates unchecksummed URLs. a reference naming a specific version still names that version. after an upgrade, run `fetchloom sync` to place the new inputs and commit the lock. [pinning rules](formats.md#the-spec-hash) cover reference and filter changes.

### status and hand edits

```sh
fetchloom status
```

if `iris.csv` is missing and `notes.txt` is an extra file, stderr shows:

```text
    Missing data/iris/iris.csv
      Extra data/iris/notes.txt
✓ found 2 differences from data.lock
  hint: `fetchloom sync` places missing or updated files and reports any conflicts it keeps
```

`status` downloads nothing and changes no project files. it compares saved sizes and modification times, hashing files when those records need checking. it reports changed manifest entries, missing, edited, untracked, outdated and extra paths, changed shared hardlinks, and archives not yet unpacked here. it is not a full integrity check. [status fields](formats.md#command-results) define the categories and [state records](formats.md#the-fetchloom-folder) explain the checks.

human output shows at most 20 paths per file category. unlocked datasets and not-unpacked archives are not truncated. `--json` includes every difference.

`status` finding differences and `sync` keeping conflicts still exit 0. scripts must inspect status arrays or `result.placement.kept`. sync restores hardlinks found changed through another project, reports a warning and lists them in `result.placement.restored`.

on windows, an edit through another project's hardlink can stay invisible until that project syncs, `cache verify` runs, or `sync --force` reads each hardlink by name. do not make a shared hardlink writable to edit it; request `copy = true` for writable inputs.

### path

```sh
fetchloom path iris
fetchloom path iris/iris.csv
```

`path <name>[/file]` prints an absolute path on stdout. it accepts a dataset, a locked file or a subfolder containing locked files. use `/` between the dataset name and its file path on either platform.

if the requested dataset is not ready, `path` syncs the whole project first. a ready dataset needs no project lock. it returns a location, not proof of intact bytes: sync may have kept an edited file there.

an unknown name or path exits 2 with a usage error. a dataset whose archives contain no regular files also exits 2; set `extract = false` in its manifest entry and sync to keep the packed archive.

## get without a project

```sh
fetchloom get zenodo:3608990
fetchloom get zenodo:3608990 -o flowers
fetchloom get https://www.cs.toronto.edu/~kriz/cifar-10-python.tar.gz --no-extract
```

`get <link>` uses the shared cache and prints the absolute output folder on stdout. it writes no manifest, lock or project state. `-o <dir>` or `--output <dir>` chooses the folder; otherwise a single file uses its filename stem, and several files use a name derived from their title or reference. it also takes the [selection and extraction options](#add-and-remove).

for direct downloads, a server's `Content-Disposition` filename replaces the name guessed from the URL. unsafe characters are replaced and names shortened for filesystem limits. the output folder may itself link elsewhere; links below it are refused.

on another run, a known SHA-256 or MD5 can find cached bytes. unpinned URLs are checked again with `ETag` or `Last-Modified` when available. a 304 response avoids a file-body download; without validators, the file may download again. source listings may still need requests. changed bytes are accepted, so use a project and lock when you need repeatable inputs.

`get` refuses unrelated top-level entries in its destination. it ignores its own leftover copy files; nested extras beneath an expected folder can remain. matching files stay. other contents can be replaced only if those bytes already exist in the cache, which does not prove fetchloom created them. there is no `get --force`: choose another folder to protect your files.

in google drive folders, selection patterns also match google docs entries. a selected doc is refused because its exported bytes can change. exclude the named entries or ask the owner for a fixed file. `add` saves exclusions in `data.toml`; project commands ask you to edit that setting. excluding every entry still fails as an empty selection.

## archives

extraction is enabled for `.zip`, `.tar`, `.tar.gz`, `.tar.xz`, `.tar.zst`, `.tar.bz2`, `.tgz`, `.txz`, `.tzst`, `.tbz` and `.tbz2`, ignoring case. the bytes choose the decoder.

other names stay intact, including `.fastq.gz`, `.xlsx`, `.npz`, `.pt` and extensionless downloads. `.7z`, `.rar` and recognized names containing unsupported bytes stay packed with a warning. unsafe or damaged supported archives fail extraction. nested archives are not unpacked recursively.

use `--no-extract` with `add` or `get`, or `extract = false` in an existing manifest entry, to keep the download packed. select and exclude patterns match the listed archive, not its members.

paths below are relative to the dataset or `get` folder:

| selected files | members in `train.zip` | placed paths |
|---|---|---|
| only `train.zip` | `train/a.csv`, `train/b.csv` | `a.csv`, `b.csv` |
| only `raw/train.zip` | `train/a.csv` | `raw/a.csv` |
| `train.zip` and another file | `train/a.csv`, `train/b.csv` | `train/a.csv`, `train/b.csv` |
| `train.zip` and another file | only `a.csv` | `a.csv` |
| `train.zip` and another file | `a.csv`, `b.csv` | `train/a.csv`, `train/b.csv` |

a sole archive loses one shared top folder. beside other selected files, a shared top folder or lone top-level file keeps its name; otherwise members go under the archive's stem. the archive's parent path is retained. colliding destinations fail.

an archive with no regular files creates no dataset folder in a project; `get` still creates an empty output folder. members enter the cache before placement. packed bytes can then be removed unless a registered project needs them packed. [archive limits](dev/architecture.md#archive) and [tree hashes](formats.md#the-tree-hash) cover validation.

## offline, recovery and approval

```sh
fetchloom sync --offline
fetchloom sync --yes
```

`--offline` prevents fetchloom from making HTTP or DNS requests. a locked project can use unchanged placed files, cached objects and unpack records. an entry needing resolution fails offline, even for a plain URL. offline `get` of a direct URL needs its URL record and cached bytes; repository listings are not cached for offline resolution.

if a locked download returns 404 or 410, fetchloom asks its recorded source for a new URL and checks the download against the original size and hashes. it does not rewrite the lock. an unchanged URL or another kind of failure cannot recover this way.

successful downloads stay cached when another file fails. retries can reuse saved partial bytes when hashes or server validators allow it. unsupported ranges, changed validators or damaged saved bytes can require a restart.

approval is required above 1 GiB, not at exactly 1 GiB:

- `get` checks the total known selected size, including cache hits.
- project downloads check known bytes still missing from the cache.
- when the source gives no size, a server's length is checked against the remaining download. without either size, the question comes after a file passes 1 GiB. unknown sizes are checked per file, with at most one such question per batch.

`--yes` or `-y` approves. questions default to no and need interactive stdin and stderr. pipes, CI and JSON mode cannot answer. for a 2 GiB file, stderr can report:

```text
✗ downloading huge.bin needs 2.0 GiB, and fetchloom cannot ask for confirmation here
  help: add --yes to approve this download
```

at a download question, ctrl-c stops with exit 130. during other asynchronous work, the first ctrl-c requests a stop while preserving resumable bytes; a second exits immediately with code 130.

## cache

```sh
fetchloom cache
fetchloom cache gc
fetchloom cache verify
```

`cache` prints its root on stdout and reports sizes and registered projects on stderr. byte totals cover cached payloads, excluding indexes and partial downloads. [cache locations](formats.md#the-cache) gives the defaults; `FETCHLOOM_CACHE` overrides them.

`cache gc` keeps files needed by registered project locks, including unpacked members. unneeded objects and unpack records become eligible after 24 hours by modification time, not last use. it also cleans abandoned partial and temporary files. a successful project sync registers the project; `get` and standalone `lock` do not.

a missing manifest removes a project's registration. an unreadable lock stops inspection or gc. removing a cached object leaves placed files intact, so bytes removed from the cache are not necessarily disk space freed.

`cache verify` rehashes cached objects. changed bytes are marked for replacement on a later download and cause exit 30. if no bytes changed but some objects could not be read, it exits 50. unreadable objects stay unmarked in the cache with warnings. close programs using them or check permissions, then verify again. failures to write change marks are also reported.

verification does not check independent copies in `data/`. after repairing cache damage, sync each project that needs those files.

## options

| option | effect |
|---|---|
| `-q`, `--quiet` | keep stdout answers and errors; hide other output; interactive questions still apply |
| `-v`, `--verbose` | add details, error kinds and work counters |
| `-vv` | also enable debug logs |
| `--json` | print one result document on stdout, including failures |
| `--offline` | use local data only |
| `-y`, `--yes` | approve downloads without asking |
| `--color <when>` | `auto` (default), `always` or `never` |
| `-C <dir>` | change the working directory before running the command |
| `-h`, `--help` | print help; also accepted after a command |
| `-V`, `--version` | print the version; root-level option |

all except `--version` apply before or after a subcommand. `-q` conflicts with verbosity. command-specific options are listed with [add](#add-and-remove), [sync](#sync), [lock](#lock-and-upgrade) and [get](#get-without-a-project).

```sh
fetchloom help concepts
fetchloom help add
fetchloom help cache gc
```

`help concepts` explains the manifest, lock, cache and sync. `help <command>` matches `<command> --help`. help stays text with `--json`. for a shorter name, add a shell alias, such as `alias flm=fetchloom` in bash or `Set-Alias flm fetchloom` in your powershell profile.

## environment variables

| variable | effect |
|---|---|
| `FETCHLOOM_CACHE` | use this cache folder when nonempty; relative paths start after `-C` |
| `FETCHLOOM_OFFLINE` | exactly `1` enables offline mode |
| `FETCHLOOM_JOBS` | cap active network operations at 1 to 256; unset or empty means 8 |
| `FETCHLOOM_RATE` | cap response-body bytes per second across the process; for example, `500K`, `10M` or `2.5M`; unset or empty means no cap |
| `FETCHLOOM_THREADS` | cap each local worker group and the async runtime's workers at 1 to 256; unset or empty keeps their defaults |
| `FETCHLOOM_SPLIT` | `0` keeps one stream per file; `1` splits files of at least 64 MiB from the first response; unset or other values split such files after one stream moves less than 1 MB/s over 2 s |
| `NO_COLOR` | a nonempty value disables automatic color |

rate suffixes `K`, `M`, `G` and `T` use powers of 1024. `2.5M` allows 2,621,440 bytes per second. the shared rate limit includes metadata and split downloads and permits a burst of one second's bytes or 1 MiB, whichever is larger.

`FETCHLOOM_THREADS` limits each worker group, not the process's total OS thread count. it does not change `FETCHLOOM_JOBS`. [settings](formats.md#settings) defines accepted values, validation, credentials, logging and other color variables. there is no config file.

## output and errors

stdout carries paths from `get`, `path` and `cache`, plus version, welcome, help or JSON. stderr carries progress, warnings, summaries and errors. a ready `path` prints no closing summary; successful `lock --check` is silent unless verbose or JSON output was requested.

interactive runs show spinners or download bars. without bars, long batches report progress after two seconds, then at ten-second intervals from the start. quiet and JSON modes suppress progress. explicit debug logging can still write stderr and disables bars.

when stdout and stderr are terminals, successful changes may end with a `next:` suggestion. `-q`, `--json` and redirected stdout omit it. plain URLs have no resolved-metadata line. other sources show available metadata; a license containing `-NC` or `-ND`, ignoring case, gets a warning.

color is automatic on suitable terminals. `NO_COLOR` disables automatic color even if `CLICOLOR_FORCE` is set. explicit `--color always` or `--color never` wins over color variables, including for help. symbols and words identify success, warnings and errors without relying on color.

with no reference:

```sh
fetchloom get
```

stderr:

```text
✗ <link> is missing
  usage: fetchloom get [options] <link>
  help: `fetchloom get --help` says what it takes
```

if a locked 32-byte file is replaced upstream by a 15-byte file, sync reports on stderr:

```text
✗ iris/iris.csv is not the file that was locked
  expected 32 bytes, got 15
  help: if the new version is intended, run `fetchloom lock --upgrade iris`
```

follow an upgrade hint only when you intend to change the pinned input. error help is optional; the main message may already say what to do. sizes use binary units, and small durations use milliseconds. [agents](agents.md) has scripting recipes; [formats](formats.md#the-json-document) defines JSON fields and exit codes.

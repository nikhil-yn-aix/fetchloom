# formats

`data.toml` and `data.lock` are TOML in UTF-8. a byte order mark is accepted on read and never written. an unknown key is an error that names the closest valid key.

## data.toml

you write this file. `fetchloom add` and `fetchloom remove` edit it in place and keep your comments, order and spacing.

```toml
[project]
data_dir = "data"

[datasets.eeg]
ref = "osf:abc12"
select = ["**/*.edf", "participants.tsv"]
exclude = ["**/derivatives/**"]

[datasets.imdb]
ref = "hf:datasets/stanfordnlp/imdb"

[datasets.weights]
ref = "https://drive.google.com/file/d/1AbCdEf/view?usp=sharing"
extract = false
copy = true
```

| key | meaning | default |
|---|---|---|
| `project.data_dir` | where datasets land: a real folder inside the project, with no symlink or junction on the way | `data` |
| `ref` | where the data comes from: a link, a DOI or an id, as listed in [scope](scope.md#sources) | required |
| `select` | globs of files to keep, matched against the whole path inside the dataset | every file |
| `exclude` | globs of files to drop | none |
| `extract` | unpack zip and tar archives | `true` |
| `copy` | place real, writable copies instead of links, for tools that write into their inputs | `false` |
| `path` | where the dataset lands inside `data_dir` | its name |

dataset names use `a-z`, `0-9`, `-` and `_`. `select` and `exclude` match the files the source lists, before anything downloads, and every member of a downloaded archive is unpacked. a select pattern that matches no file is an error that shows a few real paths, so the shape of the paths is visible.

machine settings never go in `data.toml`, since it travels through git to other machines.

## data.lock

only fetchloom writes this file. the same inputs always give the same bytes: datasets sorted by name and files by path, so a diff shows exactly what changed.

```toml
version = 1

[[dataset]]
name = "iris"
ref = "zenodo:3608990"
spec = "sha256:1c9e04a7b2f1c6d8..."
resolved = "zenodo:3608990"
title = "DatasetIris"
license = "CC-BY-4.0"
doi = "10.5281/zenodo.3608990"
locked = "2026-10-27T17:30:00Z"
files = [
  { path = "iris.csv", size = 4177, sha256 = "b1f0b46c3e27...", md5 = "5bd7e2c4...", url = "https://zenodo.org/records/3608990/files/iris.csv" },
]

[[dataset]]
name = "images"
ref = "figshare:34744689"
spec = "sha256:9a2b..."
resolved = "figshare:34744689@1"
locked = "2026-10-27T17:31:12Z"
files = [
  { path = "images.zip", size = 1830400221, sha256 = "77d1...", md5 = "4adb...", url = "https://ndownloader.figshare.com/files/12345", unpacks_to = "sha256:3e5f..." },
]
```

| key | meaning |
|---|---|
| `version` | the format version. a `data.lock` in a newer format is refused with advice to update fetchloom |
| `spec` | hash of the entry's `ref`, `select`, `exclude` and `extract`. while it matches, the dataset is not resolved again. when it changes, the dataset is resolved again at the version already locked |
| `resolved` | the exact version the reference pointed to, in the form listed below |
| `title`, `license`, `doi` | as the source states them |
| `locked` | when the dataset was last resolved, in UTC |
| `files` | every downloaded file: path, size, SHA-256, the checksum the source stated, and its stable URL |
| `unpacks_to` | for an archive, the tree hash of what it unpacks to, defined below |

- only `fetchloom lock --upgrade` moves a dataset to a newer version. `copy` and `path` change where files go, never what is downloaded.
- the lock lists what was downloaded, not what an archive contains. an archive of 100,000 images is one line.
- a URL that carries a password, a token or a signature is never written. a source that redirects to signed storage is recorded by the stable URL before the redirect, and asked again on each download.

### what each source records

| source | `resolved` | `url` of each file |
|---|---|---|
| URL | the URL | the URL |
| zenodo | `zenodo:<record>`, since a record id is one version | the file's API URL |
| figshare | `figshare:<id>@<version>` | the file's download URL |
| dataverse | `dataverse:<host>/<doi>@<version>` | the access URL with the file id |
| osf | `osf:<project>` | the download URL of the file's locked version |
| hugging face | `hf:<repo>@<commit>` | `/resolve/<commit>/<path>` |
| kaggle | `kaggle:<owner>/<dataset>@<version>` | the API download URL for that version |
| google drive | `drive:<file or folder id>` | `https://drive.google.com/uc?id=<file id>` |
| dropbox | the shared link | the shared link with `dl=1` |

### the tree hash

an archive's `unpacks_to` is the SHA-256 of one line per unpacked file, sorted by path: the path in NFC with `/` between folders, a tab, the size in bytes, a tab, the file's SHA-256 in hex, and a newline. folders, times and permissions are not part of it, so the same archive gives the same tree hash on every platform. any change to this encoding changes the lock's `version`.

## the cache

one per machine by default, any folder when `FETCHLOOM_CACHE` points elsewhere.

```
<cache>/
  CACHEDIR.TAG          tells backup tools to skip this folder
  layout                the cache layout version
  objects/ab/cdef...    one read-only file per object, named by its SHA-256
  partial/              unfinished downloads and what they need to resume
  unpacked/             the file list each archive unpacked to
  stated/md5/ab/...     maps an MD5 a source stated to the object holding those bytes
  projects/             one small file per project that synced from here, for gc
  tmp/                  files being written
```

- the default folder is `~/.cache/fetchloom` on linux, `~/Library/Caches/fetchloom` on macos and `%LOCALAPPDATA%\fetchloom` on windows.
- once an archive is unpacked and verified, its own object is deleted, unless a project on this machine keeps that archive packed. the lock still names it, so another machine downloads and unpacks the same bytes.
- `stated/` exists because zenodo, figshare and dataverse state only MD5. it lets a new project find bytes the cache already holds without downloading them again.

## the .fetchloom folder

each project keeps a small `.fetchloom/` folder beside `data.toml`. it never goes into git; `fetchloom add` adds it and the data folder to `.gitignore` inside a git repository and says so.

| path | holds |
|---|---|
| `state` | size, modification time and file identity of every placed file, so `status` needs no hashing |
| `lock` | held while a command changes the project |

## settings

v0.1 has no config file. flags and environment variables cover every setting, and a flag wins over a variable.

| variable | meaning | default |
|---|---|---|
| `FETCHLOOM_CACHE` | the cache folder | per platform, see [the cache](#the-cache) |
| `FETCHLOOM_OFFLINE` | `1` behaves like `--offline` | unset |
| `FETCHLOOM_JOBS` | downloads running at once | 8 |
| `FETCHLOOM_LOG` | log filter for debugging, such as `debug` | unset |
| `FETCHLOOM_TRACE` | write a timeline of the run to this file, for perfetto | unset |
| `NO_COLOR` | no colors | unset |

credentials are read from where each source already keeps them: `HF_TOKEN` or `~/.cache/huggingface/token`, `~/.kaggle/kaggle.json` or `KAGGLE_USERNAME` and `KAGGLE_KEY`, `ZENODO_TOKEN`, `FIGSHARE_TOKEN`, `DATAVERSE_TOKEN`, `OSF_TOKEN`. a token never appears in `data.toml`, `data.lock`, logs or `--json` output.

## the JSON document

with `--json`, every run prints one document on stdout and nothing else, even a run that stops at a mistake on the command line. `--help` still prints text, since it is for people.

```json
{"schema": 1, "command": "sync", "status": "ok", "result": {}, "warnings": [], "errors": [], "work": {}}
```

- `command` is the command the command line names, such as `sync` or `cache gc`, or null when it names none. `--version` and `fetchloom` alone give `{"version": "0.1.0"}` as `result`.
- `status` is `ok`, `partial` or `error`.
- each error carries `kind`, `message`, `help` and the dataset or path it concerns. a mistake on the command line is kind `usage`.
- `work` holds the counters: requests, bytes downloaded, files placed, cache hits, time per phase.
- a key keeps its meaning for as long as `schema` stays the same.

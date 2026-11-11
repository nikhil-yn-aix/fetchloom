---
name: fetchloom
description: find, download and pin research datasets with the fetchloom CLI. Use when a task needs a dataset from Hugging Face, Kaggle, Zenodo, figshare, OSF, Dataverse, Google Drive, Dropbox, a public S3 or GCS bucket, a DOI or a URL; when a project has data.toml or data.lock; or when code must find where a dataset lives on disk.
license: Apache-2.0
compatibility: needs fetchloom on PATH (`fetchloom --version`)
---

# fetchloom

fetchloom puts the exact files a project needs into `data/`, pinned by SHA-256 in `data.lock`, from one shared cache. `data.toml` lists datasets and `data.lock` pins every file; both belong in git. `data/` does not.

## choose the command

| the task | run |
|---|---|
| a project needs a dataset | `fetchloom add <ref> --json` |
| a project needs several datasets from a list of links | `fetchloom add -i links.txt --json`: a dataset a line, `result.datasets` names them; a SHA-256 after a link checks its one file, and any failure writes nothing |
| a project needs files that are only on this machine | `fetchloom add ./folder --json`: they are copied into the cache and pinned by their bytes, with `ref = "local"` and no path; the folder is left as it was |
| a cloned project has data.toml but no data/ | `fetchloom sync --json` |
| one dataset, without downloading or changing the others | `fetchloom sync <name> --json`; `result.selected` echoes the names. the other datasets keep their data.lock entries and files, and the next plain `fetchloom sync` still checks them all |
| a script needs a dataset's path | `fetchloom path <name>`; stdout is the path, or `result.path` with `--json`, and it syncs that dataset alone first if needed. add `--locked` where data.lock must not change |
| scripts already read `data/train.csv` | set `path = "."` on the dataset in data.toml, then `fetchloom sync --json`: its files land straight in `data/`. datasets may share a folder unless two of their files land on one path, which exits 50 with `project.invalid` naming both |
| download once, no project files | `fetchloom get <ref> --dir <folder> --json` |
| webdataset `.tar` shards | add with `--no-extract`: `fetchloom add <ref> --no-extract --json`. loaders read shards packed, and unpacked they become one file per sample |
| one file at a path the user chose | `fetchloom get <url> -o <file> --json`; `result.file` is its absolute path. it needs one link to one file, else exit 2 naming `--dir` |
| several links, or a list of them | `fetchloom get <ref> <ref> --json` or `fetchloom get -i links.txt --json`; a SHA-256 after a link on its line checks its one file |
| words, not a reference | `fetchloom search <words> --json`, then use a `reference` from `result.candidates`. `--source hf,kaggle,zenodo` and `--limit <n>` narrow or widen it |
| restore a cloned project from a mirror of pinned files: a lab disk, a usb drive or a web server, as when a source is gone | `fetchloom sync --locked --mirror <folder or url> --json`; files not on the [mirror](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/mirrors.md) come from their sources, and a folder mirror works with `--offline` |
| check data/ against the lock | `fetchloom status --json` |
| leave bad samples or unwanted files out of a dataset for good | `fetchloom drop <path>... --dry-run --json` first, then without `--dry-run`: paths are files or folders in data/. `result.dropped` names each file, its dataset, the archive it is inside and the pattern written to data.toml; tell the user to commit data.toml, and data.lock when `result.lock_changed`. it asks no server. a hand-written `exclude` does not reach files inside an archive; drop writes the archive's members table instead |
| inspect signed-in accounts | `fetchloom auth --json` |
| check cached bytes | `fetchloom cache verify --json` |
| git left `<<<<<<<` lines in data.toml or data.lock after a merge | `fetchloom lock --json`. it merges both files without downloading; when it exits 1 with `unconfirmed`, `result.merge.conflicts` names the datasets each side changed its own way: ask the user which side to keep, then run `fetchloom lock --ours <name>` or `--theirs <name>` |
| what the project uses | `fetchloom list --json`; `fetchloom list <name> --json` for one dataset, with `locked_files` paths, sizes, `sha256` and `url`, and `access`, what its source said about who may read it (`public`, `login`, `terms` or `unknown`) |
| a project with recipes, datasets data.toml makes with `from` and `run` | `fetchloom build --dry-run --json` shows each step's command in `result.steps[].run` and what it reads; show the user the commands. a build runs the project's code, so only the user decides to build. this version runs no step |
| checksums for an audit, or links for another downloader | `fetchloom export --json`: `result.placed` (files in data/, archive members included, with `sha256`) and `result.sources` (downloaded files with `url` and `sha256`). `fetchloom export -o data.sha256` writes `sha256sum -c` lines, `--format links` or `--format aria2` the links. a text form is complete or refused; JSON names what is unknown in `unlocked`, `not_unpacked` and `no_link` |

`<ref>` is a link, a DOI or an id: `hf:datasets/org/name`, `kaggle:owner/name`, `kaggle:competitions/slug`, `zenodo:3608990`, `figshare:34744689`, `osf:3eunr`, `dataverse:host/doi:10.7910/DVN/X`, `s3://bucket/prefix/`, `s3:https://host:port/bucket/prefix/` for another S3 service, `gs://bucket/prefix/`, `doi:10.5281/zenodo.3608990` or an https URL. `--select "**/*.csv"` and `--exclude` keep only some files.

## rules that keep a run safe

- pass `--json`. stdout is then one JSON document, with no colors and no questions, whatever the terminal.
- project commands look for `data.toml` from the current folder up to the top of its git repository, never above it. to use a project above the repository, name its folder with `-C <folder>`.
- plain words given to `add` or `get` are a search. without a terminal fetchloom does not guess: it exits 10 with `reference.ambiguous` and lists `result.candidates`. pick one and run again with its `reference`.
- a path such as `./x.csv`, `~/x` or `file:///x` is never searched or sent anywhere. `add` imports what it names, and `get` exits 10 with `reference.invalid`; plain words that name a file or folder in the current folder exit 2 with `usage`, so write them as `./name`. prefer the link, DOI or id where the data is published when there is one, since only that lets a teammate download it.
- a clone whose data.lock pins an import the cache lacks exits 10 with `reference.unresolved`. ask the user where those files are, then run `fetchloom add <folder> --as <name> --json`, which checks them against data.lock and changes neither file; other files exit 50 and change nothing. `lock --upgrade` never changes an import.
- a download over 1 GiB, or the size the user set in `FETCHLOOM_ASK_ABOVE`, stops with exit 1 and `unconfirmed`. ask the user before adding `--yes`; it approves the size, never a choice of dataset. do not raise `FETCHLOOM_ASK_ABOVE` to avoid asking.
- a server on this machine or its local network that only data.toml, data.lock or a `get -i` list names is refused with `network.refused`, exit 20, naming `allowed_origins`. do not set `FETCHLOOM_ALLOWED_ORIGINS` or that setting yourself; ask the user whether they trust that server.
- a private S3 bucket is read with the user's AWS keys only when the `s3_buckets` setting lists it; otherwise it exits 40 with `auth.required`, naming the setting. do not set `FETCHLOOM_S3_BUCKETS` or that setting yourself; ask the user whether they trust that bucket with their keys. an AWS profile that signs in through a role or IAM identity center also exits 40, with the command that exports its keys in `help`.
- mirror-only mode, the `mirrors_only` setting, refuses every request but to the mirrors, with `network.refused`, exit 20, naming the setting. never turn it off, or unset `FETCHLOOM_MIRRORS_ONLY`, yourself: it is the user's or their admin's rule. tell the user which files no mirror holds, from `errors[0].details`.
- never put a token in data.toml, code or a command you show. fetchloom reads keys the source's own tools saved; if none exists, ask the user to run `fetchloom auth login <source>` in their terminal.
- `get` checks every target in an output folder before removing earlier versions; a later conflict leaves earlier files in place.
- `get -o` leaves a file already at the path as it is: holding the same bytes it exits 0, holding others it exits 50 with `local.not_empty`. `--force` replaces it; use it only with the user's agreement.
- `sync` keeps files a person edited and says so. `sync --force` replaces them; use it only with the user's agreement.
- fetchloom leaves a `.rar` packed. its warning, in `warnings` with `--json`, names a command for this system that unpacks it into `<name>-unpacked`, outside the data folder: unrar on linux, unar on macos, 7-Zip on windows. the program may need installing first, so show the command to the user rather than run it.
- a file a person deleted from data/ comes back at the next sync, with a warning, and `result.placement.put_back` names it. never delete placed files to curate a dataset; `fetchloom drop <path>` leaves them out for good.
- `remove`, `drop` and `sync` delete only unchanged files fetchloom placed, and only inside the data folder. a warning names each folder they leave holding other files, and the old data folder once `data_dir` changes; tell the user instead of deleting those yourself.
- a file already at a pinned path that holds the pinned bytes is adopted: sync takes it as placed and, where the drive makes hardlinks, links it to the cache, which makes it read-only. `remove` later deletes it like any placed file and warns that it was already there.
- to preview `add`, `remove`, `drop`, `sync` or `cache gc`, add `--dry-run`. it does not apply project or cache changes; `result.changes` lists the paths. explicit `FETCHLOOM_TRACE` output is still written. a kaggle rules check can read its smallest file when its listed size is at most 4 MiB; wrappers can be larger, under the 64 MiB API-response limit, and none of those bytes is kept.
- in a container or a hosted notebook, the default cache can be read-only or lost with the session; [notebooks and containers](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/notebooks.md) says where to set `FETCHLOOM_CACHE`.

## reading a failure

`errors[0].kind` names what went wrong. `errors[0].fix`, when present, is a runnable suggestion; check that it fits the user's intended inputs before running it.

`status` differences and sync conflicts still exit 0. inspect every status array and `result.placement.kept` before using inputs; a successful `path` is not proof that its bytes match the lock.

| exit | kinds | what to do |
|---|---|---|
| 1 | `unconfirmed`, `lock.outdated` | ask before `--yes`; after a git merge, ask which side of each dataset in `result.merge.conflicts` to keep; for `lock.outdated`, the lock is missing or differs from the manifest, so review the inputs before running `fetchloom lock` |
| 2 | `usage` | fix the command line as `fix` or `help` says |
| 10 | `reference.*`, `source.invalid` | check the id or link; for `reference.ambiguous`, choose a candidate from `result.candidates` |
| 20 | `network.*`, `offline.missing` | retry later; to go online, remove `--offline` and unset `FETCHLOOM_OFFLINE` if it is `1` |
| 30 | `integrity.mismatch` | the bytes, size, archive tree or stated hash differ from the expected pin. check any supplied `--sha256` and the source; this does not prove upstream data changed. `cache verify` means cached bytes differ, possibly through a shared hardlink. preserve edits and ask before `sync --force` restores the pins |
| 40 | `auth.required` | ask the user to sign in in their own terminal with the command in `help`, or to accept terms at the page in `help` |
| 50 | `local.*`, `project.*` | a local I/O or resource failure, file conflict, or broken data.toml; report the cause. `project.changed` means data.toml or data.lock changed while an add downloaded, so it wrote neither: check the change, then run the add again. an unfinished git merge conflict in data.toml or data.lock is for `fetchloom lock`, as `fix` says; it never picks a side, and neither may you: `--ours` and `--theirs` only with the user's choice |
| 60 | `build.needed` | a recipe changed, or was never built, since data.lock pinned it; `details` say what moved. building runs the project's code, so `fix` is null: show the user `fetchloom build --dry-run --json` and build only with their agreement |
| 130 | `interrupted` | the run was stopped; completed placements stay, unfinished copies are discarded; run it again |

`cache gc` failures retain successful counts and `result.not_removed`; `cache verify` uses `result.unreadable`. these entries contain `path`, `kind`, `os_code` and `message`. use the original cause before suggesting permission changes. unreadable cache change marks stop sync and get: repair storage or access, verify and retry; never delete the marks to bypass the refusal.

a reporting failure does not roll back command effects. JSON encoding failures and stdout errors other than a closed pipe use exit 50; inspect project status before retrying a command that changed it. failed add rollbacks name private recovery files under `.fetchloom/`; preserve those files and never paste their contents into logs.

`fetchloom help --json` lists every command and option, for anything this page leaves out.

## python

```python
import fetchloom
path = fetchloom.path("iris")  # syncs iris alone if needed and returns its folder
fetchloom.sync(names=["iris"])  # syncs iris alone and returns the project folder
iris = fetchloom.get("https://example.org/iris.csv", "iris.csv")  # one file, no project
```

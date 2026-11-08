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
| a cloned project has data.toml but no data/ | `fetchloom sync --json` |
| a script needs a dataset's path | `fetchloom path <name>`; stdout is the path, and it syncs first if needed |
| download once, no project files | `fetchloom get <ref> -o <dir> --json` |
| several links, or a list of them | `fetchloom get <ref> <ref> --json` or `fetchloom get -i links.txt --json` |
| words, not a reference | `fetchloom search <words> --json`, then use a `reference` from `result.candidates`. `--source hf,kaggle,zenodo` and `--limit <n>` narrow or widen it |
| check data/ against the lock | `fetchloom status --json` |
| inspect signed-in accounts | `fetchloom auth --json` |
| check cached bytes | `fetchloom cache verify --json` |
| what the project uses | `fetchloom list --json`; `fetchloom list <name> --json` for one dataset, with `locked_files` paths and sizes |

`<ref>` is a link, a DOI or an id: `hf:datasets/org/name`, `kaggle:owner/name`, `kaggle:competitions/slug`, `zenodo:3608990`, `figshare:34744689`, `osf:3eunr`, `dataverse:host/doi:10.7910/DVN/X`, `s3://bucket/prefix/`, `gs://bucket/prefix/`, `doi:10.5281/zenodo.3608990` or an https URL. `--select "**/*.csv"` and `--exclude` keep only some files.

## rules that keep a run safe

- pass `--json`. stdout is then one JSON document, with no colors and no questions, whatever the terminal.
- plain words given to `add` or `get` are a search. without a terminal fetchloom does not guess: it exits 10 with `reference.ambiguous` and lists `result.candidates`. pick one and run again with its `reference`.
- a download over 1 GiB stops with exit 1 and `unconfirmed`. ask the user before adding `--yes`; it approves the size, never a choice of dataset.
- never put a token in data.toml, code or a command you show. fetchloom reads keys the source's own tools saved; if none exists, ask the user to run `fetchloom auth login <source>` in their terminal.
- `get` checks every target in an output folder before removing earlier versions; a later conflict leaves earlier files in place.
- `sync` keeps files a person edited and says so. `sync --force` replaces them; use it only with the user's agreement.
- to preview `add`, `remove`, `sync` or `cache gc`, add `--dry-run`. it does not apply project or cache changes; `result.changes` lists the paths. explicit `FETCHLOOM_TRACE` output is still written. a kaggle rules check can read its smallest file when its listed size is at most 4 MiB; wrappers can be larger, under the 64 MiB API-response limit, and none of those bytes is kept.

## reading a failure

`errors[0].kind` names what went wrong. `errors[0].fix`, when present, is a runnable suggestion; check that it fits the user's intended inputs before running it.

`status` differences and sync conflicts still exit 0. inspect all eleven status arrays and `result.placement.kept` before using inputs; a successful `path` is not proof that its bytes match the lock.

| exit | kinds | what to do |
|---|---|---|
| 1 | `unconfirmed`, `lock.outdated` | ask before `--yes`; for `lock.outdated`, the lock is missing or differs from the manifest, so review the inputs before running `fetchloom lock` |
| 2 | `usage` | fix the command line as `fix` or `help` says |
| 10 | `reference.*`, `source.invalid` | check the id or link; for `reference.ambiguous`, choose a candidate from `result.candidates` |
| 20 | `network.*`, `offline.missing` | retry later; to go online, remove `--offline` and unset `FETCHLOOM_OFFLINE` if it is `1` |
| 30 | `integrity.mismatch` | the bytes, size, archive tree or stated hash differ from the expected pin. check any supplied `--sha256` and the source; this does not prove upstream data changed. `cache verify` means cached bytes differ, possibly through a shared hardlink. preserve edits and ask before `sync --force` restores the pins |
| 40 | `auth.required` | ask the user to sign in in their own terminal with the command in `help`, or to accept terms at the page in `help` |
| 50 | `local.*`, `project.*` | a local I/O or resource failure, file conflict, or broken data.toml; report the cause. `project.changed` means data.toml or data.lock changed while an add downloaded, so it wrote neither: check the change, then run the add again |
| 130 | `interrupted` | the run was stopped; completed placements stay, unfinished copies are discarded; run it again |

`cache gc` failures retain successful counts and `result.not_removed`; `cache verify` uses `result.unreadable`. these entries contain `path`, `kind`, `os_code` and `message`. use the original cause before suggesting permission changes. unreadable cache change marks stop sync and get: repair storage or access, verify and retry; never delete the marks to bypass the refusal.

a reporting failure does not roll back command effects. JSON encoding failures and stdout errors other than a closed pipe use exit 50; inspect project status before retrying a command that changed it. failed add rollbacks name private recovery files under `.fetchloom/`; preserve those files and never paste their contents into logs.

`fetchloom help --json` lists every command and option, for anything this page leaves out.

## python

```python
import fetchloom
path = fetchloom.path("iris")  # syncs if needed and returns the folder
```

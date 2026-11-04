---
name: fetchloom
description: Find, download and pin research datasets with the fetchloom CLI. Use when a task needs a dataset from Hugging Face, Kaggle, Zenodo, figshare, OSF, Dataverse, Google Drive, Dropbox, a public S3 or GCS bucket, a DOI or a URL; when a project has data.toml or data.lock; or when code must find where a dataset lives on disk.
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
| what the project uses | `fetchloom list --json`; `fetchloom list <name> --json` for one dataset, with `locked_files` paths and sizes |

`<ref>` is a link, a DOI or an id: `hf:datasets/org/name`, `kaggle:owner/name`, `kaggle:competitions/slug`, `zenodo:3608990`, `figshare:34744689`, `osf:3eunr`, `dataverse:host/doi:10.7910/DVN/X`, `s3://bucket/prefix/`, `gs://bucket/prefix/`, `doi:10.5281/zenodo.3608990` or an https URL. `--select "**/*.csv"` and `--exclude` keep only some files.

## rules that keep a run safe

- pass `--json`. stdout is then one JSON document, with no colors and no questions, whatever the terminal.
- plain words given to `add` or `get` are a search. without a terminal fetchloom does not guess: it exits 10 with `reference.ambiguous` and lists `result.candidates`. pick one and run again with its `reference`.
- a download over 1 GiB stops with exit 1 and `unconfirmed`. ask the user before adding `--yes`; it approves the size, never a choice of dataset.
- never put a token in data.toml, code or a command you show. fetchloom reads keys the source's own tools saved; if none exists, ask the user to run `fetchloom auth login <source>` in their terminal.
- `sync` keeps files a person edited and says so. `sync --force` replaces them; use it only with the user's agreement.
- to show the user what `add`, `remove`, `sync` or `cache gc` would change before it does, add `--dry-run`. it downloads no files and writes nothing; `result.changes` lists the paths.

## reading a failure

`errors[0].kind` names what went wrong, and `errors[0].fix`, when present, is the command to run next.

| exit | kinds | what to do |
|---|---|---|
| 1 | `unconfirmed`, `lock.outdated` | ask before `--yes`; for `lock.outdated`, data.toml changed, so run `fetchloom lock` |
| 2 | `usage` | fix the command line as `fix` or `help` says |
| 10 | `reference.*`, `source.invalid` | check the id or link; for `reference.ambiguous`, choose a candidate from `result.candidates` |
| 20 | `network.*`, `offline.missing` | retry later, or drop `--offline` |
| 30 | `integrity.mismatch` | from `add`, `sync` or `get`: the source changed its bytes; tell the user, do not force. from `cache verify`: a file in the cache was edited, often through a project's `data/`; `fetchloom sync --force` puts the pinned bytes back, so ask the user first |
| 40 | `auth.required` | ask the user to sign in in their own terminal with the command in `help`, or to accept terms at the page in `help` |
| 50 | `local.*`, `project.invalid` | a disk, permission or file conflict, or a broken data.toml; report the message |
| 130 | `interrupted` | the run was stopped; run it again |

`fetchloom help --json` lists every command and option, for anything this page leaves out.

## python

```python
import fetchloom
path = fetchloom.path("iris")  # syncs if needed and returns the folder
```

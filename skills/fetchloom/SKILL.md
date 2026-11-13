---
name: fetchloom
description: find, download and pin research datasets with the fetchloom CLI. Use when a task needs a dataset from Hugging Face, Kaggle, Zenodo, figshare, Dataverse, Dryad, Mendeley Data, OSF, ENA or SRA runs, GEO, DANDI, OpenNeuro, arXiv papers, a GitHub or GitLab release, Google Drive, Dropbox, an S3, GCS or Azure Blob bucket, a DOI or a URL; when a project has data.toml or data.lock; or when code must find where a dataset lives on disk.
license: Apache-2.0
compatibility: needs fetchloom on PATH (`fetchloom --version`)
---

# fetchloom

fetchloom puts the exact files a project needs into `data/`. `data.toml` lists the datasets, `data.lock` pins every file by SHA-256 and size, and one cache folder keeps each file once for every project. commit `data.toml`, `data.lock` and `data.lock.d` when it exists; never commit `data/`.

pass `--json` to every command: stdout is then one JSON document, with no colors and no questions. write `fetchloom` in commands, scripts and docs, never `fl`, a short name some installs give a person, which elsewhere is missing or another program.

## the usual commands

| task | run | then read |
|---|---|---|
| a project needs a dataset | `fetchloom add <ref> --json` | `result.datasets` names it; its files are in `data/<name>` |
| a cloned project has data.toml but no data/ | `fetchloom sync --json` | `result.placement` counts what it placed; files a person edited stay, in `result.placement.kept` |
| one dataset alone | `fetchloom sync <name> --json` | the other datasets keep their pins and files |
| code needs a dataset's path | `fetchloom path <name>`, or `<name>/<file>` | stdout is the absolute path, `result.path` with `--json`. it syncs that dataset first when needed; `fetchloom path <name> --locked` never changes data.lock |
| check data/ against the lock | `fetchloom status --json` | each array in `result` lists one kind of difference; `unplaced` lists datasets with `place = false`, which is none. it exits 0 either way. `fetchloom status --verify --json` also hashes every placed file |
| fail unless the data is ready, before a script reads it | `fetchloom status --check --json`, or `fetchloom sync --strict --json` | exit 0, or 1 with `data.not_ready` and each file or dataset in the way, with what makes it ready, in `errors[0].details`. an edited file stays |
| check data.lock against data.toml, as in CI | `fetchloom lock --check --json` | exit 0, or 1 with `lock.outdated` and what changed in `errors[0].details`; it changes nothing |
| words, not a reference | `fetchloom search <words> --json` | a `reference` from `result.candidates`; `--source` with `hf`, `kaggle`, `zenodo`, `ena`, `geo`, `dandi`, `openneuro` or `arxiv`, and `--limit <n>`, narrow or widen it, and `ena`, `dandi` and `arxiv` are searched only when named; without `--limit` it returns at most 12 |
| download once, without a project | `fetchloom get <ref> --dir <folder> --json`, `--select`, `--exclude` and `--no-extract` as with `add`; `fetchloom get -i links.txt --json` reads a list. `--dry-run` writes nothing | `result.folder` and `result.files`; for several links, `result.items`. a dry run gives `result.changes.download`, each size null when unknown, and `result.changes.unpack` |
| one file at a path the user chose | `fetchloom get <link> -o <file> --json` | `result.file`. a link to several files exits 2; `--sha256 <hex>` checks the file before it takes the name. a file already there stays: the same bytes exit 0, others exit 50 with `local.not_empty` |

`<ref>` is a link, a DOI or an id: `hf:datasets/org/name`, `kaggle:owner/name`, `kaggle:competitions/slug`, `zenodo:3608990`, `figshare:34744689`, `dataverse:host/doi:10.7910/DVN/X`, `dryad:10.5061/dryad.j1fd7`, `mendeley:rscbjbr9sj@2`, `osf:3eunr`, `ena:SRR390728`, `geo:GSE1000`, `dandi:000027@0.210831.2033`, `openneuro:ds000001@1.0.0`, `arxiv:1706.03762`, `arxiv:cs.LG/2401` for a category's month, `github:owner/repo@tag`, `gitlab:group/project@tag`, `s3://bucket/prefix/`, `s3:https://host:port/bucket/prefix/` for another S3 service, `gs://bucket/prefix/`, `azure:account/container/prefix/` or `doi:10.5281/zenodo.3608990`. a dataset's page link works too.

plain words given to `add` or `get` are a search. without a terminal fetchloom does not guess: it exits 10 with `reference.ambiguous` and lists `result.candidates`; run again with the chosen `reference`. a path such as `./x` or `~/x` is never searched or sent anywhere: `add` copies its files into the cache and pins them by their bytes, and `get` refuses it. a plain word naming a file or folder here exits 2; write it as `./name`. prefer the published link, DOI or id, since only that lets a teammate download the data.

project commands look for `data.toml` from the current folder up to the top of its git repository, never above it; `-C <folder>` names another project.

## reading the JSON

read all of stdout as one document: in a pipe it is one line, at a terminal it is indented over many. every document has `schema` (1), `command`, `status` (`ok` or `error`), `result`, `warnings`, `errors` and `work` (counters). a warning has `kind`, `message`, `details` and `help`; act on its `kind`, such as `git.lock_part_untracked` or `git.code_untracked` (run the `git add` its help names), `place.kept` (an edit stayed), `license.restricted` (tell the user before sharing) or `network.private_origin` (a later run needs `allowed_origins`, the user's call).

on failure, `errors[0]` has `kind`, `message`, `details`, `help` and `fix`: a runnable `fetchloom` command, or null when the next step is the user's call. a `fix` never deletes, overwrites or repins data, runs a build, signs in or answers with `--yes`, and a `help` that offers a choice of ways with `or` has none; check that it fits what the user wants before running it.

never parse stderr. `-q`, `-v`, `--color` and `--progress` change only what a person reads there; `--progress` keeps plain progress lines beside the JSON. `fetchloom help --json` lists every command and option.

a `status` that finds differences and a `sync` that keeps edits both exit 0, so read the arrays and `result.placement.kept` first, or ask for `--check` and `--strict`. a successful `path` does not prove the bytes match the lock. `cache gc` and `cache verify` keep partial results in `result.not_removed` and `result.unreadable`.

## exit codes

| exit | kinds | what to do |
|---|---|---|
| 1 | `unconfirmed` | a question no one answered: a big download, too little disk, data to `push` that asked for an account or terms, what `cache gc --inputs` frees, an `auth logout` that signs other tools out too, or a side to keep after a git merge. ask the user; then `--yes`, or `fetchloom lock --ours <name>` or `--theirs <name>` |
| 1 | `lock.outdated` | data.lock is missing or does not pin what data.toml asks. review the change with the user before `fetchloom lock`, which pins what the sources serve today. a clone with no data.lock should get it from the project's owner |
| 1 | `data.not_ready` | tell the user each line of `errors[0].details`: replacing an edit with `fetchloom sync --force`, or building a recipe, is their call. `fix` is `fetchloom sync` only when a plain sync puts everything in place |
| 2 | `usage` | fix the command line as `help` says |
| 10 | `reference.*`, `source.invalid` | check the id or link, and ask the user about a name `help` guesses with "did you mean"; for `reference.ambiguous`, choose from `result.candidates`. a file data.lock pins whose link is gone: tell the user the folder mirror way on that `help` gives |
| 20 | `network.*`, `offline.missing` | retry later; offline, go online or name a folder mirror that holds the files. `network.refused` for a private address quotes the `allowed_origins` value that would let it through: ask the user whether they trust that server |
| 30 | `integrity.mismatch` | bytes differ from the pin, and `fix` is null. tell the user the two ways on: the pinned bytes from a folder mirror (`fetchloom push --to <folder>` where they are, then `fetchloom sync --mirror <folder>`), or `fetchloom lock --upgrade <name>` if the new bytes are intended. never upgrade on your own |
| 40 | `auth.required` | ask the user to sign in in their own terminal with the command in `help`, or to accept terms at the page it names |
| 50 | `local.*`, `project.*` | report the cause. `project.changed`: data.toml or data.lock changed during an add, which wrote neither; run it again. git's `<<<<<<<` lines in data.toml or data.lock: run `fetchloom lock --json`. a hand-edited or damaged data.lock: `fix` is null, and the user restores it from git |
| 60 | `build.needed`, `build.failed`, `build.differs` | building runs the project's code, so `fix` is null: show the user `fetchloom build --dry-run --json` and build only with their agreement. for `build.failed`, read `errors[0].details` and `.fetchloom/builds/<name>.log` |
| 130 | `interrupted` | running it again continues the downloads |

## offline and CI

- prepare online with `fetchloom sync --json`. then `fetchloom --offline sync --json`, or `FETCHLOOM_OFFLINE=1`, uses only the cache, the data folder and folder mirrors; a file none holds exits 20 with `offline.missing`. search needs the network, and so does a dataset data.lock does not pin yet, unless it is a link an earlier `get` or `add` on this machine downloaded.
- in CI, set `FETCHLOOM_CACHE` to a folder the CI cache keeps, keyed on a hash of `data.lock`, run `fetchloom sync --locked --yes --json`, then `fetchloom cache gc --now` before the cache is saved. `--locked` fails with `lock.outdated` rather than change data.lock; `fetchloom sync --locked --verify --yes --json` also hashes every placed file.
- a job's keys come from the CI system's secrets, in the source's own variables such as `HF_TOKEN` or `KAGGLE_API_TOKEN`. never write a key into data.toml, code, a command or the repository.
- in a container or a hosted notebook, set `FETCHLOOM_CACHE` to a folder that lasts, since the default can be read-only or lost with the session.

## ask the user first

- `--yes` (`-y`): a download over 1 GiB, or `FETCHLOOM_ASK_ABOVE`, or one the cache's disk lacks room for, exits 1 with `unconfirmed`. ask before adding `--yes`, which never picks a dataset, and never raise `FETCHLOOM_ASK_ABOVE` to avoid asking.
- `--force`: on `sync` it replaces files a person edited, on `get -o` and `export -o` a file already at the path.
- `fetchloom build` runs the project's code as the user, and `fetchloom build --accept` pins a rebuild that differs: show `fetchloom build --dry-run --json` first.
- `fetchloom lock --upgrade` pins what the sources serve now, and `fetchloom cache gc --now` frees files less than a day old.
- signing in: the user runs `fetchloom auth login <source>` in their own terminal, and for private buckets and containers the cloud's own sign-in, such as `aws sso login`, `gcloud auth application-default login` or `az login`. never ask for a key in chat, and never put a SAS, a signed link or a link holding a key, such as dataverse's `?key=`, in data.toml: `add` refuses one.
- trust settings are the user's. never set `allowed_origins`, `s3_buckets`, `gcs_buckets`, `azure_containers` or `drive_folders`, or their `FETCHLOOM_` variables: they decide which private servers, buckets, containers and drive folders get the user's keys, and a drive token reads their whole drive. never turn off `mirrors_only`, and never pass `--no-config` or set `FETCHLOOM_NO_CONFIG` unless the user asks: it leaves their whole settings file unread.
- `push` puts data where everyone who can read the folder can copy it: never choose the folder yourself.

## rules that keep data safe

- preview with `--dry-run` (`-n`): `add`, `remove`, `drop`, `sync`, `lock`, `push`, `cache gc` and `get` then change nothing and list the paths in `result.changes`, and `build` lists the commands it would run.
- never delete or chmod placed files: they come back at the next sync, and one can be a hardlink shared with other projects. `fetchloom drop <path>` leaves files out for good; `copy = true` on a dataset in data.toml, then `fetchloom sync --json`, gives writable copies.
- messages and `--json` show each query or fragment value of a link as `***` unless its name is known to be harmless, such as `dl` or `version`: take a link from the user or from data.toml, never from output.
- write results outside `data/`. sync keeps files written inside it, but `status` lists them as extra.
- `remove`, `drop` and `sync` delete only unchanged files fetchloom placed, inside the data folder. a warning names each folder they leave holding other files; tell the user rather than delete them.
- to rename a dataset, rename its table in data.toml, and the `from` and `{name}` of every recipe that reads it; the next `fetchloom lock --json` keeps its pin. `remove` then `add --as` resolves it again and may pin newer data.
- never delete data.lock to get past an error: relocking pins newer data and drops every recipe's pin.
- an archive fetchloom cannot unpack, such as a `.rar`, stays packed with a warning whose help unpacks it: show that command rather than run it. parts such as `scans.zip.001` to `.003` unpack only when one dataset lists every part. add webdataset `.tar` shards with `--no-extract`, since loaders read them packed.
- a failed add names recovery files under `.fetchloom/`: keep them, and never paste their contents, which may hold credentials. never delete cache change marks to get past a refusal; ask the user to repair the storage, then run `fetchloom cache verify`.

## other tasks

| task | run |
|---|---|
| several datasets at once | `fetchloom add <ref> <ref> --json`, or `fetchloom add -i links.txt --json`, one link a line with an optional SHA-256 of its one file. any failure writes nothing. `--as`, `--select` and `--exclude` are for one dataset |
| choose a dataset's files and name | `fetchloom add <ref> --as <name> --select "**/*.csv" --exclude "raw/**" --no-extract --dry-run --json`; `--dry-run` (`-n`) lists the changes in `result.changes`. the same add again exits 0 and changes nothing; a name the project has for another link exits 2 |
| files only on this machine | `fetchloom add ./folder --json` pins them with `ref = "local"`. a clone whose cache lacks them exits 10 with `reference.unresolved`; ask the user where they are, then `fetchloom add <folder> --as <name> --json` checks them against data.lock |
| scripts already read `data/train.csv` | set `path = "."` on the dataset in data.toml, then `fetchloom sync --json` |
| leave bad samples out for good | `fetchloom drop <path>... --dry-run --json`, then without `--dry-run`: paths in data/, never patterns. `result.dropped` names each file and the pattern written to data.toml |
| stop using a dataset | `fetchloom remove <name> --dry-run --json`, then without `--dry-run` |
| newer versions | `fetchloom lock --upgrade <name> --dry-run --json` shows them; only the user decides to pin them |
| a git merge left `<<<<<<<` lines | `fetchloom lock --json` merges both sides. on `unconfirmed`, `result.merge.conflicts` names the datasets to choose for: ask the user, then `fetchloom lock --ours <name>` or `--theirs <name>`. in a rebase, ours is the branch rebased onto |
| what the project uses | `fetchloom list --json`: each dataset's `ref`, `files`, `bytes`, `folder`, `placed`, `access` (`public`, `login`, `terms` or `unknown`) and `locked_files` |
| restore from a mirror | `fetchloom sync --locked --mirror <folder or url> --json`; files the mirror lacks come from their sources, and a folder mirror works offline |
| fill a folder mirror for others | `fetchloom push --to <folder> --dry-run --json`, then without `--dry-run`; `fetchloom push <recipe> --inputs --to <folder>` adds what the recipe is made from. data whose source wanted an account or terms, or did not say, exits 1 with `unconfirmed`, listed in `result.asks` |
| recipes: datasets data.toml makes with `from` and `run` | `fetchloom build --dry-run --json`: `result.steps[].action` and `result.steps[].run` say what it would run. `fetchloom build --json` runs them; `fetchloom build --check --locked --json` reruns every step and compares, changing nothing; `fetchloom build --prepare --json` fetches what a later `fetchloom --offline build --json` needs |
| how much the cache takes | `fetchloom cache --json`: tell the user `result.freeable`, what `cache gc` frees now, not `result.bytes` |
| free cache space | `fetchloom cache gc --dry-run --json`, then `fetchloom cache gc --json`. `--now` also frees files less than a day old; `--inputs` frees raw data only builds read once a folder mirror holds it, and needs `--yes` from the user |
| check cached bytes | `fetchloom cache verify --json`: exit 30 with `result.changed` when bytes changed |
| checksums or links for another tool | `fetchloom export --json`: `result.placed` and `result.sources`. `fetchloom export -o data.sha256` writes `sha256sum -c` lines, `--format links` or `--format aria2` the links; `--force` replaces a file at the `-o` path |
| signed-in accounts | `fetchloom auth --json`, including where the AWS keys, google credentials and azure credentials buckets and containers use come from, in `kind` and `from`, without running anything. the user runs `fetchloom auth login <source>`, with `--host <site>` for a dataverse site or `--with-token` to read a key from stdin; `fetchloom auth logout <source>` signs out, `--host` too |
| why a run uses some cache, mirror or limit | `fetchloom settings --json`: each setting's `value` and `origin` |
| a slow run from far away, or a small machine | `FETCHLOOM_RESOURCES=max` or `lean` before the command; a setting the user set by name still wins |
| shell completion | `fetchloom completions <shell>` for bash, zsh, fish, powershell or elvish; where the script goes is the user's choice |

## python

```python
import fetchloom
path = fetchloom.path("iris")  # syncs iris alone if needed and returns its folder
fetchloom.sync(names=["iris"])  # syncs iris alone and returns the project folder
iris = fetchloom.get("https://example.org/iris.csv", "iris.csv")  # one file, no project
```

# agents

use `--json` when a script or agent needs to read fetchloom's answer. branch on fields and exit codes, not progress messages.

## without a terminal

- stdout carries the answer. stderr carries progress, warnings and errors in normal mode.
- `--json` prints one document on stdout on success or failure, including usage errors. it never prompts. help stays text; explicit debug logging can still write stderr.
- no command waits for input when stdin is not a terminal. a download needing approval fails with `unconfirmed` and exit 1. pass `--yes` only when you approve the download.
- `-q` keeps stdout and errors, hiding other normal output. JSON warnings remain in the document.
- `-C <dir>` starts in that folder; project discovery searches it and its parents for `data.toml`.
- `--offline` prevents HTTP and DNS requests by fetchloom.

approval normally applies above 1 GiB of known bytes needing download. cached checksummed files and unpacked archives do not count; offline runs never ask for download approval. [approval](cli.md#offline-recovery-and-approval) covers unknown sizes.

## the JSON document

| field | value |
|---|---|
| `schema` | integer `1` |
| `command` | command name, such as `add` or `cache gc`; null for the welcome, version or an unrecognized command prefix |
| `status` | `ok` or `error` |
| `result` | command result, or null on failure |
| `warnings` | array of strings, including in quiet mode |
| `errors` | empty on success; one object with `kind`, `message`, `details` and nullable `help` on failure; file downloads also include `failures` |
| `work` | counts for requests, downloaded bytes, retries, resumes, cached objects and placements, plus elapsed milliseconds |

bare `fetchloom` inside a project reports command `status`. help prints text even with `--json`. a killed process may leave no valid JSON; treat a missing or invalid document as a failure too.

[command results](formats.md#command-results) defines the result fields. [work counters](formats.md#work-counters) defines what each counter measures. `bytes_down = 0` does not prove there were no requests, and a warm project can place cached files with `cache_hits = 0`.

## recipes

### add or fetch once

pin a dataset in your project:

```sh
fetchloom --json add zenodo:3608990
```

read `result.project` for the project root and `result.datasets` for added names. repeating the same addition can return an empty name array because the entry already exists.

fetch without creating project files:

```sh
fetchloom --json get zenodo:3608990 -o flowers
```

read `result.folder` for the absolute output folder. `result.files` lists the placed files with paths relative to that folder, sizes and complete SHA-256 hashes. extracted members replace their archive in this list. `origin` describes where bytes came from; `method` is `reflink`, `hardlink`, `copy`, or null for a file already present. a later `get` may accept changed upstream data.

### check and sync pinned inputs

check that the committed manifest and lock agree:

```sh
fetchloom --json lock --check
```

stop if it fails. `lock.outdated` means someone must update and commit the lock before the job continues. `lock --check` checks the manifest against the lock, not the contents of `data/`.

then place the inputs, with approval for a large download:

```sh
fetchloom --json --yes sync
```

exit 0 does not mean every requested file was placed. `result.placement.kept` lists conflicts left in place, each with a project-relative `path` and a `reason`: `changed`, `foreign` or `unneeded`. `restored` lists paths repaired after shared hardlink changes. do not add `--force` automatically; it can overwrite a person's files.

in python, read a sync result and stop on conflicts:

```python
import json
import subprocess

completed = subprocess.run(
    ["fetchloom", "--json", "--yes", "sync"],
    stdin=subprocess.DEVNULL,
    capture_output=True,
    encoding="utf-8",
    check=False,
)
answer = json.loads(completed.stdout)
if answer["schema"] != 1:
    raise RuntimeError("unsupported fetchloom JSON schema")
if completed.returncode != 0 or answer["status"] != "ok":
    error = answer["errors"][0]
    raise RuntimeError(f"{error['kind']}: {error['message']}")
if answer["result"]["placement"]["kept"]:
    raise RuntimeError(answer["result"]["placement"]["kept"])
```

this stops on missing or invalid JSON as well. it does not check unrelated extra files. to inspect the whole project:

```sh
fetchloom --json status
```

read `unlocked`, `removed`, `missing`, `edited`, `untracked`, `outdated`, `edited_elsewhere`, `extra` and `not_unpacked` under `result`. all nine arrays empty means status found no differences; status uses saved file metadata and selective hashing, not a full integrity scan. differences still exit 0.

### find a path

```sh
fetchloom --json path iris/iris.csv
fetchloom -q path iris
```

JSON `result` is an absolute path string. the quiet form prints only the path on stdout. either can sync the whole project if the requested dataset is not ready. neither guarantees that sync replaced an edited file it found there.

### work offline

prepare the project online before disconnecting:

```sh
fetchloom --json --yes sync
```

then use:

```sh
fetchloom --json --offline sync
fetchloom --json --offline path iris
```

unchanged placed files can be enough; restoring missing files needs cached objects and, for extracted archives, unpack records. keep the manifest and lock unchanged. an entry needing resolution fails offline. repository listings are not cached for offline resolution; offline `get` of a direct URL needs its URL record and cached bytes.

`FETCHLOOM_OFFLINE=1` also enables offline mode, including for the python helper. to reconnect, remove `--offline` and unset that variable if it is `1`. [settings](formats.md#settings) defines the environment variables and credentials.

## error kinds and exit codes

| exit | error kinds |
|---|---|
| 0 | success |
| 1 | `unconfirmed`, `lock.outdated` |
| 2 | `usage` |
| 10 | `reference.invalid`, `reference.unresolved`, `source.invalid` |
| 20 | `network.unreachable`, `network.status`, `network.rate_limited`, `network.refused`, `offline.missing` |
| 30 | `integrity.mismatch` |
| 40 | `auth.required` |
| 50 | `local.io`, `local.not_empty`, `project.invalid` |
| 130 | `interrupted` |

read `errors[0].kind` before deciding whether to retry. `help`, when present, suggests a fix; it is human text, not an instruction to execute without checking. download errors include every failed file under `failures`, with its own kind, message, details and help. [exit codes](formats.md#exit-codes) is the format reference.

| kind | next step |
|---|---|
| `unconfirmed` | check the download size and add `--yes` if approved |
| `lock.outdated` | update the lock deliberately, then commit it |
| `offline.missing` | prepare the missing data online before retrying offline |
| `integrity.mismatch` | investigate changed bytes; upgrade pins only if the change is intended |
| `auth.required` | obtain access and set the source's supported credentials |
| `local.not_empty` | preserve existing files and choose another `get -o` folder |
| `local.io` | check disk space, permissions and files held open by other programs |

`cache verify` returns exit 30 for changed cached bytes, or 50 for unreadable objects when none changed. on either failure `result` is null, so read errors and warnings. it does not verify independent project copies.

## python helper

`pip install fetchloom` installs the commands and the `fetchloom` python module. on python 3.9 or later:

```python
import fetchloom

project = fetchloom.sync(project=".", yes=True)
iris = fetchloom.path("iris/iris.csv", project=".", offline=True)
```

both return `pathlib.Path`. `sync()` returns the absolute project root; `path()` returns a dataset, file or subfolder path, syncing the whole project first when needed, which may download data. `project` can be a string or path-like object naming any folder in the project; it defaults to the current directory.

the helper runs the installed command with `--json` and forwards its warnings as python warnings. `sync()` preserves edited files. use the CLI result when you need to inspect `placement.kept` or status arrays.

a failed command raises `fetchloom.Error` with `kind`, `message`, `details`, `help` and `code`. `kind` is `None` when the process returned no JSON answer. a missing executable raises `FileNotFoundError`.

both helpers accept keyword-only `yes: bool = False` and `offline: bool = False`, forwarding `--yes` and `--offline` when true. use `yes=True` only when you approve downloads; otherwise a large download can raise `unconfirmed`. use `offline=True` to forbid network requests, including an implicit sync from `path()`. neither helper accepts `--force`. they inherit environment settings, including `FETCHLOOM_CACHE` and `FETCHLOOM_OFFLINE`.

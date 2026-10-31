# scripting

`--json` gives scripts a versioned result, error kinds and work counters without terminal progress.

```sh
fetchloom --json lock --check
fetchloom --json --yes sync --locked
fetchloom --json status
```

stop when the lock check fails. after sync, inspect kept conflicts before using the data. status differences and kept conflicts still exit 0.

## reading an answer

| field | use |
|---|---|
| `schema` | require the supported integer version, currently `1` |
| `command` | identify the command, or null for welcome, version or an unrecognized prefix |
| `status` | require `ok` for successful completion |
| `result` | read the command-specific fields |
| `warnings` | inspect warnings even in quiet mode |
| `errors` | read `kind`, `message`, `details` and nullable `help`; download failures also include `failures` |
| `work` | inspect request, byte, retry, resume, cache and placement counters |

JSON appears on stdout on success and failure, including usage errors. help remains text, and explicit debug logging can still write stderr. a killed process may leave no valid JSON; treat missing or invalid JSON as failure.

bare `fetchloom` in a project reports command `status`. [JSON output](reference/json-output.md) defines every result and counter. zero `bytes_down` does not prove there were no requests. a warm project can place cached files with zero `cache_hits`.

## checking and syncing

```python
import json
import subprocess

completed = subprocess.run(
    ["fetchloom", "--json", "--yes", "sync", "--locked"],
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

invalid or missing JSON raises before inputs are used. pass `--yes` only when you approve the download. JSON mode cannot answer a question, so an unapproved large download fails with `unconfirmed` and exit 1.

`kept` lists project-relative paths and reasons: `changed`, `shared`, `foreign` or `unneeded`. `restored` lists repaired shared hardlinks. do not add `--force` automatically; it can overwrite a person's files.

kept conflicts do not include unrelated extras. run `fetchloom --json status` and check `unlocked`, `removed`, `missing`, `edited`, `untracked`, `outdated`, `edited_elsewhere`, `extra` and `not_unpacked`. all nine arrays empty means status found no differences. it uses saved metadata and selective hashing, not a full integrity scan.

## adding and fetching

```sh
fetchloom --json add zenodo:3608990
fetchloom --json get zenodo:3608990 -o flowers
```

`add` returns `result.project` and added names in `result.datasets`. repeating an identical addition can return an empty name array because the entry already exists.

`get` returns `result.folder` and `result.files`. file paths are relative to that folder, with sizes and complete SHA-256 hashes. extracted members replace archives in the list. `origin` describes where bytes came from; `method` is `reflink`, `hardlink`, `copy`, or null for an already present file.

`get` writes no project manifest or lock and can accept changed upstream data on another run.

## finding paths

```sh
fetchloom --json path iris/iris.csv
fetchloom -q path iris
```

JSON `result` is an absolute path string. the quiet form prints only the path on stdout. either can sync the whole project when the requested dataset is not ready. neither proves that an edited file was replaced.

`-C` chooses the folder where project discovery starts. the search includes its parents. quiet mode keeps stdout answers and errors; JSON warnings remain in the document.

## offline jobs

follow [offline preparation](concepts.md#offline) before disconnecting. a prepared job can require its lock without allowing requests:

```sh
fetchloom --json --offline sync --locked
fetchloom --json --offline path iris
```

## CI cache

restore `FETCHLOOM_CACHE` using the operating system, architecture and `data.lock` hash. after checkout, with fetchloom [installed from source](getting-started.md#install), these GitHub Actions steps try offline first and allow a download on a cache miss:

```yaml
- uses: actions/cache@v4
  with:
    path: .fetchloom-cache
    key: data-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('data.lock') }}
- name: restore research inputs
  shell: bash
  env:
    FETCHLOOM_CACHE: ${{ github.workspace }}/.fetchloom-cache
  run: |
    fetchloom sync --locked --offline || fetchloom sync --locked --yes
```

`--locked` fails with `lock.outdated`, exit 1, when the manifest and lock differ. neither attempt updates stale pins. remove `--yes` if the job must not approve a large download. this recipe checks command success; jobs that reject preserved edits must also inspect [placement results](#checking-and-syncing).

## errors and exit codes

branch on `errors[0].kind`, not progress messages. [exit codes](reference/exit-codes.md) lists every kind and a next step. `help` is human advice to check, not an instruction to execute unchecked.

download errors include each failed file under `failures`, with its kind, message, details and help. the first failed file in job order sets the overall kind and exit code.

`cache verify` returns 30 for changed cached bytes, or 50 for unreadable paths when none changed. `cache gc` returns 50 for cleanup failures. both retain findings and counts in `result` alongside the error. verification does not check independent project copies.

## python helper

install from source with python 3.9 or later and the [rust build prerequisites](getting-started.md#install). maturin builds the executable and packages it with the helper; no PyPI package is published yet.

```sh
pip install "git+https://github.com/nikhil-yn-aix/fetchloom"
```

on python 3.9 or later:

```python
import fetchloom

project = fetchloom.sync(project=".", yes=True, strict=True)
iris = fetchloom.path("iris/iris.csv", project=".", offline=True)
```

| function | return |
|---|---|
| `fetchloom.sync(project=".", *, yes=False, offline=False, strict=False)` | absolute project root as `pathlib.Path` |
| `fetchloom.path(name, project=".", *, yes=False, offline=False)` | dataset, file or subfolder as `pathlib.Path` |

`project` accepts a string or path-like object naming any folder in the project and defaults to the current directory. `path()` may sync the whole project and download data when the dataset is not ready.

both helpers run the installed executable with `--json` and forward warnings as python warnings. `sync()` preserves edits and forwards the CLI warnings. `strict=True` raises `fetchloom.Error` with kind `conflict.kept` if sync kept any conflicts; `details` contains their project-relative paths and `code` is `None`. it does not overwrite them or audit unrelated extras. use CLI JSON when you need status arrays.

`yes`, `offline` and sync's `strict` are keyword-only booleans, all false by default. `yes=True` approves downloads. without approval, a large download can raise `unconfirmed`. `offline=True` forbids requests, including an implicit sync from `path()`.

neither helper exposes `--force` or `--locked`. both inherit [environment settings](reference/environment.md), including `FETCHLOOM_CACHE` and `FETCHLOOM_OFFLINE`.

a failed command raises `fetchloom.Error` with `kind`, `message`, `details`, `help` and `code`. `kind` is `None` when the process returned no JSON answer. a missing executable raises `FileNotFoundError`.

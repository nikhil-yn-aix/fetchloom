# scripting

`--json` gives scripts a versioned result, error kinds and work counters without terminal progress.

```sh
fetchloom --json lock --check
fetchloom --json --yes sync --locked --strict
fetchloom --json status --check
```

stop when the lock check fails. `--strict` and `--check` fail with `data.not_ready`, exit 1, while the data is not ready to use, as when sync kept an edited file. without them, status differences and kept conflicts exit 0, so inspect them before using the data.

## reading an answer

| field | use |
|---|---|
| `schema` | require the supported integer version, currently `1` |
| `command` | identify the command, or null for welcome, version or an unrecognized prefix |
| `status` | require `ok` for successful completion |
| `result` | read the command-specific fields |
| `warnings` | inspect warnings even in quiet mode: branch on each one's `kind`, listed in [warning kinds](reference/json-output.md#warning-kinds), never on its `message` |
| `errors` | read `kind`, `message`, `details`, nullable `help` and nullable `fix`, a command to run next; download failures also include `failures` |
| `work` | inspect request, byte, retry, resume, cache and placement counters |

JSON appears on stdout on success and failure, including usage errors and help: `fetchloom help --json` describes every command and option. a killed process may leave no valid JSON, so treat missing or invalid JSON as failure.

a long job that a person watches can still show how it goes: `--progress` keeps plain progress lines on stderr: a download or a lookup says what it does at once, other work after 2 s, then each says how far it got every 10 s, while stdout holds the one document. the lines are for a person, so read stdout and stderr apart and never parse them.

```sh
fetchloom --json --progress get https://example.org/big.bin
```

while the download runs, stderr shows:

```text
Downloading big.bin · 1.9 MiB
Downloading 1.4 MiB of 1.9 MiB · 10 s
```

[JSON output](reference/json-output.md) defines every result and counter.

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

pass `--yes` only when you approve the download: JSON mode cannot answer a question, so an unapproved large download fails with `unconfirmed` and exit 1. never add `--force` by itself, since it overwrites a person's files.

`kept` lists the files sync left as they were, each with its reason, and does not include other extra files. for those, run `fetchloom --json status` and check its arrays, which [status](commands/status.md#categories) explains: every array empty, but `unplaced`, means status found no differences. it compares sizes and times, not every byte, unless `--verify` asks it to hash every placed file.

a job that needs one dataset names it, as in `fetchloom --json --yes sync --locked small`: only small is downloaded, placed and checked against data.lock, and `result.selected` echoes `["small"]`.

## adding and fetching

```sh
fetchloom --json add https://huggingface.co/datasets/scikit-learn/iris
fetchloom --json get https://zenodo.org/records/3608990 --dir flowers
fetchloom --json get https://example.org/iris.csv -o iris.csv
```

`add` returns the added names in `result.datasets`, empty when the entry already existed. `get` returns `result.folder` and `result.files`, each file with its path in that folder, size and SHA-256, and with `-o`, `result.file`, the file's absolute path. `fetchloom --json get -i links.txt` returns one record for each link in `result.items`, with its own `status`, `folder`, `files` and `error`; a run where some links failed still exits nonzero, so read `result.items` to see which arrived. [JSON output](reference/json-output.md#command-results) lists the fields. `get` pins nothing, so another run can take changed bytes.

## finding paths

```sh
fetchloom --json path iris/Iris.csv
fetchloom -q path iris
fetchloom --json path iris --locked
```

JSON `result.path` is the absolute path, and the quiet form prints only the path on stdout. either syncs the dataset alone when it is not ready, which does not prove an edited file was replaced. `--locked` keeps that sync from changing data.lock: it fails with `lock.outdated`, exit 1, before any request. `-C` chooses the folder where the search for the project starts.

## offline jobs

follow [offline preparation](concepts.md#offline) before disconnecting. a prepared job can require its lock without allowing requests:

```sh
fetchloom --json --offline sync --locked
fetchloom --json --offline path iris
```

## CI cache

[CI](ci.md) keeps the cache between jobs, keyed on `data.lock`, with recipes for github actions and gitlab ci.

## errors and exit codes

branch on `errors[0].kind`, never on messages. [exit codes](reference/exit-codes.md) lists every kind and a next step. `help` is advice for a person to check, and `fix` a command to run only once it fits what you meant. download errors list each failed file under `failures`, and the first failed file in job order sets the overall kind and exit code.

## python helper

install from source with python 3.10 or later and the [rust build prerequisites](getting-started.md#install). maturin builds the executable and packages it with the helper; no PyPI package is published yet.

```sh
pip install "git+https://github.com/nikhil-yn-aix/fetchloom"
```

`python -m fetchloom` runs the program the package installs, also where the environment's scripts folder is not on PATH: in a notebook whose kernel lacks it, `!fetchloom` says `command not found`, while `!{sys.executable} -m fetchloom sync` works.

on python 3.10 or later:

```python
import fetchloom

project = fetchloom.sync(project=".", yes=True, strict=True)
fetchloom.sync(names=["iris"], yes=True)
iris = fetchloom.path("iris/Iris.csv", project=".", offline=True)
```

a notebook without a project can fetch one file where it wants it, as [`get -o`](commands/get.md#one-file) does:

```python
import fetchloom
import pandas as pd

frame = pd.read_csv(fetchloom.get("https://example.org/iris.csv", "data.csv"))
```

| function | return |
|---|---|
| `fetchloom.sync(project=".", *, names=(), yes=False, offline=False, strict=False, locked=False, verify=False)` | absolute project root as `pathlib.Path` |
| `fetchloom.path(name, project=".", *, yes=False, offline=False, locked=False)` | dataset, file or subfolder as `pathlib.Path` |
| `fetchloom.get(link, path=None, *, folder=None, sha256=None, force=False, yes=False, offline=False)` | with `path`, the file written there; otherwise the folder, `folder` or a new one named after the download; as `pathlib.Path` |

`project` names any folder in the project and defaults to python's working folder. a script started from another folder, as a job scheduler starts one, needs it, as in `fetchloom.path("iris", project=Path(__file__).parent)`. finding no data.toml there or above raises `project.invalid`.

`names`, a list of dataset names or one name as a string, syncs those datasets alone, as `fetchloom sync <name>...` does. a string is always one name, so pass several as a list. `path()` may sync the dataset it names and download its data when it is not ready.

`get()` passes `path` as `-o`, `folder` as `--dir`, and `sha256`, `force`, `yes` and `offline` as their flags; a leading `~` in `path`, `folder` or `project` is your home folder, as pandas reads it. `path` and `folder` cannot both be given, and `force=True` needs `path`: either raises kind `usage` before anything runs. a file already at `path` holding the same bytes is returned, so running a notebook cell again is safe; one holding other bytes raises kind `local.not_empty` unless `force=True` replaces it.

without `path`, an archive is unpacked into the folder `get()` returns, and one whose members share one top folder loses it, as [archives](concepts.md#archives) shows: a `flowers.zip` holding `flowers/iris.csv` gives a folder `flowers` with `iris.csv` in it. the files in that folder may be read-only links to the cache's one copy, shared with every project holding the same bytes, while the file `get()` writes at `path` is your own writable copy. to change data, write a new file, such as `df.to_csv("results/iris-clean.csv")`.

the helpers run the executable installed with the package, else one in the python environment's scripts folders, else the first on PATH, with `--json --progress`, and pass each warning on as a python warning, its help after its message, and the progress lines to `sys.stderr`, so a notebook shows a long download while it runs:

```text
Downloading big.bin · 2.9 MiB
Downloading 1.4 MiB of 2.9 MiB · 10 s
```

a notebook's stop button interrupts python, which stops the program too, and the next call continues from the bytes already downloaded. killing only the python process leaves the program running until it has placed the file.

`sync()` keeps edited files and passes the warnings on. `strict=True` passes [`--strict`](commands/sync.md#local-changes), so it raises `fetchloom.Error` with kind `data.not_ready` and code 1 when the data is not ready to use once the sync is done: a file sync kept, a recipe's result not on this machine, or a dataset data.lock does not pin as data.toml asks, each named in `details` with what makes it ready. use the CLI's JSON when you need status arrays. `fetchloom.__version__` is the installed package's version.

`yes`, `offline`, `locked`, sync's `strict` and `verify` and get's `force` are keyword-only booleans, false by default, passed as `--yes`, `--offline`, `--locked`, `--strict`, `--verify` and `--force`. without `yes=True`, a large download raises `unconfirmed`, and `locked=True` raises `lock.outdated` rather than update data.lock, for pipelines that must run on committed pins. `path()` and `sync()` do not expose `--force`.

all three read the [environment settings](reference/environment.md). set `FETCHLOOM_CACHE` to an absolute folder, as `os.environ["FETCHLOOM_CACHE"] = os.path.abspath("cache")`, since a relative one is read from the project folder. a source's key, such as `HF_TOKEN` in `os.environ`, reaches fetchloom the same way.

a failed command raises `fetchloom.Error` with `kind`, `message`, `details`, `help` and `code`, the exit code. `kind` is `None` when the program printed no JSON, and the `message` then ends with the last lines it printed; `code` is `None` for the errors the helper raises itself, such as `usage` when `get()` is given both `path` and `folder`. branch on `kind`. in messages, a flag the helper sets for one of its keywords is named by that keyword, so `-o` reads `the path argument`, while a whole command in backticks keeps its flags, so it still runs at a terminal. a missing executable raises `FileNotFoundError`, whose message points at the [install steps](https://github.com/nikhil-yn-aix/fetchloom#install).

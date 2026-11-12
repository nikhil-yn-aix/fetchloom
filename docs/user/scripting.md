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
| `errors` | read `kind`, `message`, `details`, nullable `help` and nullable `fix`, a command to run next; download failures also include `failures` |
| `work` | inspect request, byte, retry, resume, cache and placement counters |

JSON appears on stdout on success and failure, including usage errors and help: `fetchloom help --json` describes every command and option. explicit debug logging can still write stderr. a killed process may leave no valid JSON; treat missing or invalid JSON as failure.

a long job that a person watches can still show how it goes: `--progress` keeps the plain lines a download or a wait prints on stderr where no bar shows, after 2 s and then every 10 s, while stdout holds the one document. `-q` hides them. the lines are for a person to read, so read stdout and stderr apart and never parse the lines.

```sh
fetchloom --json --progress get https://example.org/big.bin
```

while the download runs, stderr shows:

```text
Downloading big.bin · 1.9 MiB
Downloading 1.4 MiB of 1.9 MiB · 10 s
```

bare `fetchloom` in a project reports command `status`. [JSON output](reference/json-output.md) defines every result and counter. zero `bytes_down` does not prove there were no requests. `placed_from_cache` counts the files a run placed from bytes the cache already held, and `downloads_skipped` only the downloads the cache made unnecessary.

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

`kept` lists project-relative paths and reasons: `changed`, `shared`, `foreign`, `unneeded` or `blocked`, which adds the path of the entry in the way as `blocked_by`. `restored` lists files put back that no edit of the project's explained: links to a changed cached copy, and files `sync --verify` found changed. `changed_copies` names, as `{path, kept_as}`, where each of those links kept its changed copy beside the pinned file. do not add `--force` automatically; it can overwrite a person's files.

kept conflicts do not include unrelated extras. run `fetchloom --json status` and check `unlocked`, `removed`, `missing`, `edited`, `untracked`, `adoptable`, `outdated`, `damaged`, `blocked`, `extra`, `unneeded` and `not_unpacked`. every array empty means status found no differences. it uses saved metadata and selective hashing, not a full integrity scan, unless `--verify` asks it to hash every placed file.

a job that needs one dataset names it, as in `fetchloom --json --yes sync --locked small`: only small is downloaded and placed, `--locked` checks only small's entry in data.lock, and `result.selected` echoes `["small"]`. the other datasets keep their data.lock entries and files as they are, so `kept` and `placement` speak of small alone, and the next plain `sync` still checks every dataset. a name data.toml does not have fails with `usage`, exit 2, before any request.

## adding and fetching

```sh
fetchloom --json add https://huggingface.co/datasets/scikit-learn/iris
fetchloom --json get https://zenodo.org/records/3608990 --dir flowers
fetchloom --json get https://example.org/iris.csv -o iris.csv
```

`add` returns `result.project` and added names in `result.datasets`. repeating an identical addition can return an empty name array because the entry already exists.

`get` returns `result.folder` and `result.files`. file paths are relative to that folder, with sizes and complete SHA-256 hashes. extracted members replace archives in the list. `origin` describes where bytes came from; `method` is `reflink`, `hardlink`, `copy`, or null for an already present file. with `-o`, `result.file` is the file's absolute path and `result.folder` its folder.

`fetchloom --json get -i links.txt` returns one record for each link in `result.items`, with its own `status`, `folder`, `files` and `error`. a run where some links failed still exits nonzero; read `result.items` to see which links arrived. [JSON output](reference/json-output.md#command-results) lists the fields.

`get` writes no project manifest or lock and can accept changed upstream data on another run.

## finding paths

```sh
fetchloom --json path iris/Iris.csv
fetchloom -q path iris
fetchloom --json path iris --locked
```

JSON `result.path` is the absolute path. the quiet form prints only the path on stdout. either syncs the requested dataset alone when it is not ready. neither proves that an edited file was replaced. `--locked` keeps that sync from changing data.lock: when data.lock is missing or the dataset's entry is out of date with data.toml, it fails with `lock.outdated`, exit 1, before any request.

`-C` chooses the folder where project discovery starts. the search includes its parents, up to the top of the git repository it starts in. quiet mode keeps stdout answers and errors; JSON warnings remain in the document.

## offline jobs

follow [offline preparation](concepts.md#offline) before disconnecting. a prepared job can require its lock without allowing requests:

```sh
fetchloom --json --offline sync --locked
fetchloom --json --offline path iris
```

## CI cache

[CI](ci.md) keeps the cache between jobs, keyed on `data.lock`, with recipes for github actions and gitlab ci.

## errors and exit codes

branch on `errors[0].kind`, not progress messages. [exit codes](reference/exit-codes.md) lists every kind and a next step. `help` is human advice to check, not an instruction to execute unchecked.

download errors include each failed file under `failures`, with its kind, message, details and help. the first failed file in job order sets the overall kind and exit code.

`cache verify` returns 30 for changed cached bytes, or 50 for unreadable paths when none changed. `cache gc` returns 50 for cleanup failures. both retain findings and counts in `result` alongside the error. verification does not check independent project copies.

## python helper

install from source with python 3.10 or later and the [rust build prerequisites](getting-started.md#install). maturin builds the executable and packages it with the helper; no PyPI package is published yet.

```sh
pip install "git+https://github.com/nikhil-yn-aix/fetchloom"
```

`python -m fetchloom` runs the program the package installs, which reaches it where the environment's scripts folder is not on PATH. in a notebook whose kernel lacks that folder on PATH, `!fetchloom` says `command not found`, while `!{sys.executable} -m fetchloom sync` runs the program in a cell. the release wheels will also install `fl`, a short name for it.

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

`project` accepts a string or path-like object naming any folder in the project and defaults to the current directory, python's working folder. a script started from another folder, as a job scheduler starts one, needs it, as in `fetchloom.path("iris", project=Path(__file__).parent)` for a script kept in the project. without it, finding no data.toml in that folder or above it raises `project.invalid`, whose `help` says to give `project=` the folder that holds data.toml, or names the folder where `fetchloom add` would start a new project.

`names`, a list of dataset names or one name as a string, syncs those datasets alone, as `fetchloom sync <name>...` does; without it, `sync()` syncs every dataset. a string is always one name: `names="plain flowers"` and `names="plain,flowers"` each ask for one dataset of that name and fail with `usage`, so pass several as a list. `path()` may sync the dataset it names and download that dataset's data when it is not ready. it passes `name` after its options and `--`, as `sync()` passes `names`, so a name such as `-h` is looked up as a dataset, never read as an option.

`get()` passes `path` as `-o`, `folder` as `--dir`, and `sha256`, `force`, `yes` and `offline` as their flags; relative paths start at the current directory, and a leading `~` in `path`, `folder` or `project` is your home folder, as pandas reads it. `path` and `folder` cannot both be given, and `force=True` needs `path`: either raises `fetchloom.Error` with kind `usage` before anything runs. a file already at `path` holding the same bytes stays and is returned, so running a notebook cell again is safe, and a file still holding what the link served last time only asks the server whether it changed, so an unchanged large file needs no `yes=True`; one holding other bytes raises kind `local.not_empty` unless `force=True` replaces it. with `sha256`, a file already holding those bytes is returned without a download, and so is, with `offline=True`, a file still holding what the link served last time, a hugging face link's included.

without `path`, an archive is unpacked into the folder `get()` returns, and one archive whose members share one top folder loses that folder, as [archives](concepts.md#archives) shows: a `flowers.zip` holding `flowers/iris.csv` gives a folder named `flowers` with `iris.csv` in it, so read `folder / "iris.csv"`, not `folder / "flowers" / "iris.csv"`. the files in that folder, as in a project's data folder, may be read-only links to the cache's one copy, shared with every project and folder holding the same bytes. the file `get()` writes at `path` is your own writable copy. to change data, write a new file, such as `df.to_csv("results/iris-clean.csv")`: making a linked input writable and editing it changes the shared copy, which other projects then read until their next `sync()` puts the pinned bytes back.

the helper first looks for the executable beside the imported package or in its installation prefix. it then checks the current and base interpreter scripts, the preferred user scripts directory, and PATH. this keeps a `pip --target` or `pip --prefix` package from using an unrelated interpreter-level executable when its own executable is present.

the helpers run the installed executable with `--json --progress` and forward warnings as python warnings. each progress line the program prints goes to `sys.stderr` as it comes, so a notebook shows a long download while it runs: what it downloads after 2 s, then how far it got every 10 s. a quick call prints nothing.

```text
Downloading big.bin · 2.9 MiB
Downloading 1.4 MiB of 2.9 MiB · 10 s
```

a notebook's stop button interrupts python, which stops the program too, and the next call continues from the bytes already downloaded. killing only the python process, as `kill <pid>`, `kill -9 <pid>` or the system's out-of-memory killer does, leaves the program running: it finishes the download and places the file. a call for the same link that starts meanwhile waits for it, and after 2 s says so on stderr: `Waiting big.bin · another fetchloom is downloading it`. a `get()` with the same `path` then returns the file the program wrote, asking the server at most whether its bytes changed; other calls find the bytes in the cache, unless that `get()` moved them out of it for its file, and then download them again.

`sync()` preserves edits and forwards the CLI warnings. `strict=True` raises `fetchloom.Error` with kind `conflict.kept` if sync kept any conflicts; `details` contains their project-relative paths, `help` the `fetchloom sync --force` command for the same names, and `code` is `None`. unless `project` is the current folder, that command names the project's root after `-C`, so it replaces files only in the project `sync()` checked, even when run from another folder, such as a notebook's own project. it does not overwrite them or audit unrelated extras. a file sync put back as pinned matches the lock again, so it raises nothing; its warning says what was put back. use CLI JSON when you need status arrays. `fetchloom.__version__` is the installed package's version.

`yes`, `offline`, `locked`, sync's `strict` and `verify` and get's `force` are keyword-only booleans, all false by default. `yes=True` approves downloads. without approval, a large download can raise `unconfirmed`. `offline=True` forbids requests, including an implicit sync from `path()`. `locked=True` passes `--locked`, so a data.lock that is missing or out of date with data.toml raises `lock.outdated` instead of being updated; use it in pipelines that must run on committed pins. `verify=True` passes `--verify`, so sync checks the SHA-256 of every placed file and puts back one whose bytes changed though its size and time did not, as a CI job's `sync --locked --verify` does.

`path()` and `sync()` do not expose `--force`; `get(force=True)` replaces only the file `path` names. all three inherit [environment settings](reference/environment.md), including `FETCHLOOM_CACHE` and `FETCHLOOM_OFFLINE`. set `FETCHLOOM_CACHE` to an absolute folder, as `os.environ["FETCHLOOM_CACHE"] = os.path.abspath("cache")`: a relative one is read in the folder `-C` names, which is `project` for `path()` and `sync()` and the current folder for `get()`, so one notebook can fill two caches. a source's key reaches fetchloom the same way: `HF_TOKEN` set in `os.environ` is used for gated hugging face data without `fetchloom auth login hf`, as [auth](commands/auth.md) says.

a failed command raises `fetchloom.Error` with `kind`, `message`, `details`, `help` and `code`. `kind` is `None` when the process returned no JSON answer, whose `message` ends with the last 20 lines it printed on stderr, and `code` is `None` when the helper raised the error itself, with no process to exit: `get()`'s `usage` errors about its arguments and `strict=True`'s `conflict.kept`. branch on `kind`, which is set for both. printed, or in a traceback, the error reads like the command line's: the message, then each detail and the help on a line of its own. a command `help` names is the one a person types: without the helper's own `--json --progress`, and without `-C .` when `project` is the current folder. in the message, details, help and warnings, a flag the helper sets for one of its keywords is named by that keyword instead: `add --force` reads `pass force=True`, `-o` reads `the path argument`, `-C` reads `the project argument`, and an example made of such flags alone, such as `` `-o results/iris.csv` `` or `` `--output=results/iris.csv` ``, reads `` `path='results/iris.csv'` ``. a whole command in backticks keeps its flags, so it still runs at a terminal or in a notebook's `!` line: an unreachable link's help says to run `` `fetchloom get --output=u.csv -- <link>` `` again. what fetchloom repeats in double quotes, such as search words, stays as you gave it. a missing executable raises `FileNotFoundError`, whose message points at the [install steps](https://github.com/nikhil-yn-aix/fetchloom#install).

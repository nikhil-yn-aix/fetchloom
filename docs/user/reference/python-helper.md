# Python helper

install from source with python 3.10 or later and the [rust build prerequisites](../start/install.md). maturin builds the executable and packages it with the helper; no PyPI package is published yet.

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

a notebook without a project can fetch one file where it wants it, as [`get -o`](../commands/get.md#one-file) does:

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

without `path`, an archive is unpacked into the folder `get()` returns, and one whose members share one top folder loses it, as [archives](archives.md) shows: a `flowers.zip` holding `flowers/iris.csv` gives a folder `flowers` with `iris.csv` in it. the files in that folder may be read-only links to the cache's one copy, shared with every project holding the same bytes, while the file `get()` writes at `path` is your own writable copy. to change data, write a new file, such as `df.to_csv("results/iris-clean.csv")`.

the helpers run the executable installed with the package, else one in the python environment's scripts folders, else the first in an absolute folder on PATH, on windows one with a drive or a share and a root, as `C:\tools`, never one in the working folder or a relative or empty PATH entry, such as `\tools` or `C:tools`, which a cloned project could hold, with `--json --progress`, and pass each warning on as a python warning, its help after its message, and the progress lines to `sys.stderr`, so a notebook shows a long download while it runs:

```text
Downloading big.bin · 2.9 MiB
Downloading 1.4 MiB of 2.9 MiB · 10 s
```

a notebook's stop button interrupts python, which stops the program too, and the next call continues from the bytes already downloaded. killing only the python process leaves the program running until it has placed the file.

`sync()` keeps edited files and passes the warnings on. `strict=True` passes [`--strict`](../commands/sync.md#local-changes), so it raises `fetchloom.Error` with kind `data.not_ready` and code 1 when the data is not ready to use once the sync is done: a file sync kept, a recipe's result not on this machine, or a dataset data.lock does not pin as data.toml asks, each named in `details` with what makes it ready. use the CLI's JSON when you need status arrays. `fetchloom.__version__` is the installed package's version.

`yes`, `offline`, `locked`, sync's `strict` and `verify` and get's `force` are keyword-only booleans, false by default, passed as `--yes`, `--offline`, `--locked`, `--strict`, `--verify` and `--force`. without `yes=True`, a large download raises `unconfirmed`, and `locked=True` raises `lock.outdated` rather than update data.lock, for pipelines that must run on committed pins. `path()` and `sync()` do not expose `--force`.

all three read the [environment settings](environment.md). set `FETCHLOOM_CACHE` to an absolute folder, as `os.environ["FETCHLOOM_CACHE"] = os.path.abspath("cache")`, since a relative one is read from the project folder. a source's key, such as `HF_TOKEN` in `os.environ`, reaches fetchloom the same way.

a failed command raises `fetchloom.Error` with `kind`, `message`, `details`, `help` and `code`, the exit code. `kind` is `None` when the program printed no JSON, and the `message` then ends with the last lines it printed; `code` is `None` for the errors the helper raises itself, such as `usage` when `get()` is given both `path` and `folder`. branch on `kind`. in messages, a flag the helper sets for one of its keywords is named by that keyword, so `-o` reads `the path argument`, while a whole command in backticks keeps its flags, so it still runs at a terminal. such a command leaves out what the helper passes on its own: `--json`, `--progress`, the `--` before names and, when `project` is the working folder, `-C`. a guess at a mistyped name given to `path()` names the call that finds it, as ``did you mean `fetchloom.path("iris")`?``. a missing executable raises `FileNotFoundError`, whose message points at the [install steps](https://github.com/nikhil-yn-aix/fetchloom#install).

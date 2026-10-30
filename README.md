# fetchloom

fetchloom fetches research data into a project folder. `data.toml` names the datasets; `data.lock` pins each selected file by SHA-256 and size.

a shared local cache reuses the bytes across projects. files land in `data/` by reflink, then hardlink, then copy. recognized zip and tar archives unpack by default.

## install

on linux:

```sh
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh
```

on windows, in powershell:

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.ps1 | iex"
```

the installer downloads the latest release, checks it against the release's `SHA256SUMS`, and puts `fetchloom` and `fl` in `~/.local/bin` on linux or `%LOCALAPPDATA%\Programs\fetchloom\bin` on windows. when that folder is not on your PATH, it adds it to your shell profiles on linux or to your user PATH on windows. to install another version, give it as the first argument or in `FETCHLOOM_VERSION`:

```sh
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh -s -- 0.1.0
```

the release binaries are x86_64 only: windows 10 or later, and linux with any C library, since they are static.

with pip, on python 3.9 or later:

```sh
pip install fetchloom
```

the wheel installs both commands and a python module that runs `fetchloom` with `--json`:

```python
import fetchloom

iris = fetchloom.path("iris")  # a pathlib.Path; syncs the project first if iris is missing
project = fetchloom.sync()
```

a failure raises `fetchloom.Error`, whose `kind` and `message` come from the JSON error. [agents](docs/agents.md#error-kinds-and-exit-codes) lists the kinds. both functions take `project=`, a folder in the project, which defaults to the working directory.

from source, with rust 1.99.1 and a C toolchain:

```sh
git clone https://github.com/nikhil-yn-aix/fetchloom.git
cd fetchloom
cargo install --path . --locked
fetchloom --version
```

```text
fetchloom 0.1.0
```

each way installs both `fetchloom` and `fl`. use `fetchloom` in powershell, where `fl` already means `Format-List`.

## quick start

`fetchloom` with no arguments, outside a project, prints the first steps:

```text
fetchloom 0.1.0

fetchloom puts the exact research data a project needs into its data/ folder.

  data.toml, the shopping list: which datasets you want
  data.lock, the receipt: the exact files and their fingerprints
  the cache, the pantry: shared by every project on this machine
  sync, restocking: data/ ends up matching the receipt

try it with zenodo:3608990, a 4 KB iris.csv:

  fetchloom add zenodo:3608990   put iris.csv in data/iris and pin it
  fetchloom sync                 restock data/ in a project you cloned
  fetchloom get zenodo:3608990   fetch it once, without a project

learn more with `fetchloom help concepts` and `fetchloom <command> --help`
```

in your research project:

```sh
fetchloom add zenodo:3608990
```

the story goes to stderr. the time and the placement method depend on your machine, and the `next:` line shows only on a terminal:

```text
   Resolved zenodo:3608990 · DatasetIris · 1 file · 4.1 KiB · cc-by-4.0
 Downloaded 1 file · 4.1 KiB in 354 ms
     Locked iris · 1 file in data.lock
     Placed data/iris · 1 file linked, no extra disk
✓ added iris. its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
  next: commit data.toml and data.lock; anyone who clones runs `fetchloom sync`
```

`data.toml` now lists `iris`, `data.lock` pins `iris.csv` by SHA-256 and size, and the file is in `data/iris/`. `fetchloom path iris` prints that folder's absolute path.

commit `data.toml` and `data.lock`. in a clone of that project, run:

```sh
fetchloom sync
```

another sync with unchanged managed files reports on stderr:

```text
✓ everything already matches data.lock. nothing to do
```

for a download without a project:

```sh
fetchloom get zenodo:3608990
```

`get` prints its output folder and writes no project files. it can accept newer bytes on a later run; use `add` and a lock when you need pinned inputs.

files are read-only by default. `sync` preserves hand edits unless you use `--force`; it may still exit successfully with conflicts kept. use `status` to inspect them, or read the JSON result in scripts. [CLI](docs/cli.md) covers selection, writable inputs, archives, offline use and cache cleanup, and [agents](docs/agents.md) covers driving fetchloom from scripts.

## docs

| doc | use it for |
|---|---|
| [vision](docs/vision.md) | why pin published inputs |
| [scope](docs/scope.md) | current sources and boundaries |
| [cli](docs/cli.md) | commands and their output |
| [agents](docs/agents.md) | driving fetchloom from scripts and agents |
| [formats](docs/formats.md) | manifest, lock, cache records and JSON |
| [architecture](docs/architecture.md) | module boundaries and data flow |
| [decisions](docs/decisions.md) | reasons behind the design |
| [plan](docs/plan.md) | built milestones and remaining work |
| [performance](docs/performance.md) | measurements and budgets |
| [engineering](docs/engineering.md) | building, testing and releases |
| [style](docs/style.md) | writing rules |

## license

apache-2.0. see [LICENSE](LICENSE) and [NOTICE](NOTICE).

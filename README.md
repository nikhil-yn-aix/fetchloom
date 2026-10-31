# fetchloom

fetchloom puts the exact research data your project needs in `data/` on windows and linux. `data.toml` is your shopping list, `data.lock` is the receipt pinning each file by SHA-256 and size, and the shared cache is your pantry for reuse across projects.

```text
fetchloom add zenodo:3608990
     Locked iris · 1 file in data.lock
     Placed data/iris · 1 file linked from the cache
      Added iris, its files are in data/iris
cat data.lock
files = [
  { path = "iris.csv", size = 4177, sha256 = "b1f0b46c3e27350cbf0f75fb3b7d36adcd997c443e764bf09255126feeb11f31", md5 = "395d6abd001abf3760a1a794ef4c453c", url = "https://zenodo.org/api/records/3608990/files/iris.csv/content" },
]
fetchloom sync # in a fresh clone
```

stderr is shown for `add`, and only the lock's `files` field for `cat`; linking requires filesystem support. commit `data.toml` and `data.lock` to git before syncing a fresh clone.

## why

| when you need | what fetchloom does |
|---|---|
| reproducible inputs | records resolved sources, file sizes and SHA-256 in the lock, then checks downloads against those pins. cached objects are not rehashed on every use. [formats](docs/formats.md#datalock), [cache checks](docs/dev/architecture.md#cache) |
| fast repeat work | a second project reuses files already in the shared cache without downloading them. reflinks and hardlinks avoid another full copy where supported. [placement](docs/dev/architecture.md#place), [performance](docs/dev/performance.md#how-it-stays-fast) |
| honest source handling | respects announced rate limits within bounded waits, retries transient failures and resumes when hashes or validators make it safe. checks the source's MD5 or SHA-256 when pinning a download. [network behavior](docs/dev/architecture.md#net) |
| scripts and notebooks | `--json` returns one document on stdout; exit codes distinguish usage, network and integrity failures. the python helper returns `pathlib.Path` objects. [agents](docs/agents.md) |

## install

on linux:

```sh
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh
```

on windows, in powershell:

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.ps1 | iex"
```

the installer checks the download against the release's `SHA256SUMS`. it installs in `~/.local/bin` on linux or `%LOCALAPPDATA%\Programs\fetchloom\bin` on windows, adding the folder to your shell profiles or user PATH when needed. to choose a version, pass it as the first argument or set `FETCHLOOM_VERSION`:

```sh
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh -s -- 0.1.0
```

release binaries are x86_64 only: windows 10 or later, or linux with any C library. macos is not supported.

with pip, on python 3.9 or later:

```sh
pip install fetchloom
```

this also installs the [python helper](docs/agents.md#python-helper).

from source, with rust 1.99.1 and a C toolchain:

```sh
git clone https://github.com/nikhil-yn-aix/fetchloom.git
cd fetchloom
cargo install --path . --locked
fetchloom --version
```

each method installs one program, `fetchloom`. for a shorter name, add a shell alias.

## quick start

run `fetchloom` with no arguments to see where to start:

```text
fetchloom 0.1.0

fetchloom puts the exact research data a project needs into its data/ folder.

  data.toml, the shopping list: which datasets you want
  data.lock, the receipt: the exact files and their fingerprints
  the cache, the pantry: shared by projects using it
  sync, restocking: fill data/ from the receipt

try it with zenodo:3608990, a 4 KB iris.csv:

  fetchloom add zenodo:3608990   put iris.csv in data/iris and pin it
  fetchloom sync                 restock data/ in a project you cloned
  fetchloom get zenodo:3608990   fetch it once, without a project

learn more with `fetchloom help concepts` and `fetchloom <command> --help`
```

### add

run in your project to download iris and pin it:

```sh
fetchloom add zenodo:3608990
```

stderr excerpt, when the filesystem supports links:

```text
     Locked iris · 1 file in data.lock
     Placed data/iris · 1 file linked from the cache
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

`add` writes `data.toml` and `data.lock`, fills `data/iris`, and adds the data folder to `.gitignore` in a git repository. commit the two TOML files, not the downloaded data.

### sync

in a fresh clone, restore the pinned files:

```sh
fetchloom sync
```

cached files need no download. if you edit `data.toml`, sync updates the lock first; unchanged entries keep their pins. to work without network access after preparing the data:

```sh
fetchloom sync --offline
```

### status

check for missing, edited or extra files without downloading anything:

```sh
fetchloom status
```

if `iris.csv` is missing and `notes.txt` is an extra file, stderr shows:

```text
    Missing data/iris/iris.csv
      Extra data/iris/notes.txt
      Found 2 differences from data.lock
  hint: `fetchloom sync` places missing or updated files and reports any conflicts it keeps
```

`sync` preserves hand edits and can exit 0 with conflicts left in place. check `status` before using `sync --force`, which can replace edited and untracked files at locked paths. [status and hand edits](docs/cli.md#status-and-hand-edits) explains the checks.

### path

find the dataset folder or a file from a script or notebook:

```sh
fetchloom path iris
fetchloom path iris/iris.csv
```

`path` prints an absolute path on stdout. it syncs the whole project first if the dataset is not ready.

### get

download once without creating a manifest or lock:

```sh
fetchloom get zenodo:3608990
```

stderr excerpt, when the filesystem supports links:

```text
     Placed iris/iris.csv · linked from the cache
    Fetched iris.csv into iris
```

`get` prints the absolute output folder on stdout. use `-o flowers` to choose another folder. a later `get` can accept changed upstream data and may still request source metadata even when it reuses cached bytes; use `add` for pinned inputs.

recognized zip and tar archives unpack by default; `--no-extract` keeps them packed. files are read-only by default. for an analysis that writes into its inputs, set `copy = true` on the dataset in `data.toml`, then sync. keep your results outside managed inputs. [cli](docs/cli.md) covers commands and archive handling.

## sources

use `get` to try a source, or replace it with `add` to pin it in your project:

| source | command |
|---|---|
| URL | `fetchloom get https://raw.githubusercontent.com/mwaskom/seaborn-data/master/iris.csv` |
| DOI | `fetchloom get doi:10.5281/zenodo.3608990` |
| zenodo | `fetchloom get zenodo:3608990` |
| figshare | `fetchloom get figshare:34744689@1` |
| dataverse | `fetchloom get dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP` |
| OSF | `fetchloom get osf:3eunr` |
| hugging face | `fetchloom get hf:datasets/openai/openai_humaneval@7dce6050a7d6d172f3cc5c32aa97f52fa1a2e544` |
| kaggle | `fetchloom get kaggle:uciml/breast-cancer-wisconsin-data@2` |
| google drive | `fetchloom get drive:15bpMH3clXwBJf9cdGpXIOgwKwHrg4T--` |
| dropbox | `fetchloom get "https://www.dropbox.com/s/3y96pnutxum3p4v/0569.pdf?dl=1"` |

for part of a dataset, pass `--select "**/*.csv"` or `--exclude "**/drafts/**"` to `add` or `get`. quote patterns so your shell passes them unchanged; they match listed files, not archive members. [scope](docs/scope.md#sources) lists accepted links and access limits; [settings](docs/formats.md#settings) lists credentials for private or gated data.

## how fast

the [benchmark method](docs/dev/benchmarks.md) pairs runs on one laptop, with linux in WSL 2 and windows with defender enabled. it covers a network lab with lan, home, far and poor profiles, plus live services whose timings describe that network and run date.

fetchloom's [comparison times](docs/dev/performance.md#against-other-tools) include hashing, pinning, flushing and cached placement; transfer-only timings do not include an equivalent project lock or shared cache.

<!-- bench: lab -->

<!-- bench: live -->

## how it works

- resolve your source reference and pin the selected files by SHA-256 and size in `data.lock`.
- keep one content-addressed cache shared by projects, with objects named by SHA-256.
- place files by reflink, then hardlink, then copy where the filesystem permits; recognized synced folders skip hardlinks.
- start new downloads as one stream; eligible files of at least 64 MiB can split into parallel range requests when that stream falls below 1 MB/s.
- use `--offline` to forbid network requests; unchanged placed files can suffice, while missing files need cached objects and archives can need unpack records.

see [architecture](docs/dev/architecture.md) for cache trust, resume conditions and placement rules.

## windows tips

Microsoft Defender scanned every staged file in a 10,000-file unpack on the measured laptop. [small-file measurements](docs/dev/performance.md#small-files-on-windows) record the cost.

a dev drive with defender's performance mode defers scans until after file open completes. [Microsoft's performance mode comparison](https://learn.microsoft.com/en-us/defender-endpoint/microsoft-defender-endpoint-antivirus-performance-mode#performance-mode-compared-to-real-time-protection) states the trade-off:

> Deferring scans until after file open completes inherently provides faster performance, but with less protection. However, enabling performance mode provides significantly better protection than other performance tuning methods, such as using folder exclusions, which block security scans altogether.

creating a dev drive needs an administrator; ask your IT team whether performance mode suits your machine. if the drive is `D:`, set the cache in your powershell session:

```powershell
$env:FETCHLOOM_CACHE = 'D:\fetchloom-cache'
```

keep your projects on the same drive to permit linking. [settings](docs/formats.md#settings) covers cache location.

## troubleshooting

| stderr message | fix |
|---|---|
| `error: <link> is missing` | pass a dataset reference after `get` or `add`; `fetchloom get --help` lists examples. |
| `error: offline, and 1 file the lock names is not in the cache` | run `fetchloom sync` online first: remove `--offline` and unset `FETCHLOOM_OFFLINE` if it is `1`. |
| `error: iris/iris.csv is not the file that was locked` | the source returned different bytes. if the change is intended, run `fetchloom lock --upgrade iris`, then `fetchloom sync`; otherwise ask the source owner for the pinned file. |
| `error: downloading huge.bin needs 2.0 GiB, and fetchloom cannot ask for confirmation here` | check the download size, then add `--yes` if you approve it. |
| `error: iris/iris.csv already exists with other contents` | move your file somewhere safe, or fetch into another folder with `fetchloom get zenodo:3608990 -o flowers`. |

use `-v` for error kinds and work counters. [agents](docs/agents.md#error-kinds-and-exit-codes) lists exit codes and retry decisions.

## scripts and agents

use `--json` and branch on fields and exit codes, not progress text:

```sh
fetchloom --json lock --check
fetchloom --json sync
fetchloom --json status
```

stop if `lock --check` fails. inspect `result.placement.kept` after sync; exit 0 can leave conflicts in place. status differences also exit 0. [agents](docs/agents.md) defines the JSON recipes and failure handling.

with the python helper installed:

```python
import fetchloom

fetchloom.sync(project=".")
iris = fetchloom.path("iris/iris.csv", project=".", offline=True)
```

both helpers return `pathlib.Path` and call the installed executable with `--json`. the [python helper](docs/agents.md#python-helper) documents approval options and exceptions.

## docs

### user docs in `docs/`

| doc | use it for |
|---|---|
| [cli](docs/cli.md) | commands, options and output |
| [scope](docs/scope.md) | sources, platforms and limits |
| [formats](docs/formats.md) | manifest, lock, settings and JSON fields |
| [agents](docs/agents.md) | scripts, offline use and the python helper |

### developer docs in `docs/dev/`

| doc | use it for |
|---|---|
| [architecture](docs/dev/architecture.md) | modules, data flow and trust assumptions |
| [engineering](docs/dev/engineering.md) | builds, tests, profiling and releases |
| [benchmarks](docs/dev/benchmarks.md) | comparison method and workloads |
| [performance](docs/dev/performance.md) | measurements and their conditions |
| [decisions](docs/dev/decisions.md) | recorded design choices |
| [plan](docs/dev/plan.md) | pending work |
| [style](docs/dev/style.md) | prose, code and commit conventions |
| [vision](docs/dev/vision.md) | project goals |

## contributing

start with [architecture](docs/dev/architecture.md) for the code map and [engineering](docs/dev/engineering.md) for building, testing and releases.

## license

apache-2.0. see [LICENSE](LICENSE) and [NOTICE](NOTICE).

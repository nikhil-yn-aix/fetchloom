# fetchloom

fetchloom is uv for research data: it puts the datasets your project needs in `data/` on windows and linux. `data.toml` is your shopping list, `data.lock` is the receipt that pins every file by SHA-256 and size, and the shared cache is your pantry. `fetchloom sync` fills `data/` from that receipt, reusing cached bytes across projects by reflink, then hardlink, then copy where the filesystem permits.

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

each method installs `fetchloom` and `fl`, the same program under two names. use `fetchloom` in powershell, where `fl` already means `Format-List`.

## quick start

outside a project, running `fetchloom` with no arguments prints this on stdout:

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

in your research project's folder, run:

```sh
fetchloom add zenodo:3608990
```

you now have `data/iris/iris.csv`, a `data.toml` entry called `iris`, and its exact file hash and size in `data.lock`. commit `data.toml` and `data.lock` to git. `add` puts the data folder in `.gitignore` when you are in a git repository.

anyone who clones the project runs:

```sh
fetchloom sync
```

to find your dataset from a script or notebook:

```sh
fetchloom path iris
```

this prints the absolute folder path. it syncs the project first if the dataset is not ready.

for a download you do not need to track in a project:

```sh
fetchloom get zenodo:3608990 -o flowers
```

`get` prints the output folder and writes no project files. a later `get` can accept changed data; use `add` when you need pinned inputs. recognized zip and tar archives unpack by default. add `--no-extract` to keep them packed.

## sources

use `get` to try a source, or replace it with `add` to keep that dataset in your project:

| source | example |
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

for part of a dataset, use `--select "**/*.csv"` or `--exclude "**/drafts/**"` with `add` or `get`. quote patterns so your shell passes them unchanged. they match listed files, not files inside an archive. [scope](docs/scope.md#sources) lists accepted links and access limits; [formats](docs/formats.md#settings) lists credentials for private or gated data.

## everyday commands

| when you want to | run |
|---|---|
| add a dataset and choose its name | `fetchloom add zenodo:3608990 --as flowers` |
| restore a cloned project's inputs | `fetchloom sync` |
| see missing, edited or extra files | `fetchloom status` |
| find a dataset or one of its files | `fetchloom path flowers/iris.csv` |
| update pins after editing `data.toml`, without placing files | `fetchloom lock` |
| check that the manifest and lock agree, without changing either | `fetchloom lock --check` |
| ask the source for a newer version | `fetchloom lock --upgrade flowers`, then `fetchloom sync` |
| remove a dataset and its unchanged managed files | `fetchloom remove flowers` |
| download once without a project | `fetchloom get zenodo:3608990 -o flowers` |
| see where the cache lives and how much it holds | `fetchloom cache` |
| remove cached files no registered project needs, once a day old | `fetchloom cache gc` |
| check cached bytes for damage | `fetchloom cache verify` |

files are read-only by default. for an analysis that writes into its inputs, set `copy = true` on that dataset in `data.toml`, then sync. keep your own results outside the managed inputs.

`sync` preserves hand edits and reports conflicts, even when it exits successfully. check `status` before using `sync --force`, which can replace edited and untracked files at locked paths. [cli](docs/cli.md) covers every command and option.

## how fast

on one laptop, in WSL with ubuntu 24.04, from a test server on the same machine, medians of 3 runs: a 1 GiB file took 4.0 s against 3.4 s for curl followed by sha256sum, 1.9 s for aria2c and 8.8 s for pooch, and a second project got it from the cache in 0.01 s. a zip of 10,000 small files took 0.87 s against 0.66 s for curl with bsdtar and 2.2 s for pooch. 1,000 files of 100 KiB at 100 Mbit/s took 9.5 s against 9.0 s for curl's parallel mode. on a lossy far link, splitting a slow download across streams fetched 100 MiB 65% faster than one stream. [performance](docs/dev/performance.md#against-other-tools) has the method, every tool, the misses and the windows results.

the first download depends on the source, your connection and your disk. the cache stores files by their content hash, so projects with identical files can reuse the same bytes. reflinks and hardlinks avoid copying the whole file, where supported. a project whose files are already in place can sync without network requests.

one-off `get` may still ask a source for metadata or check whether a URL changed. avoiding a file download does not always mean avoiding requests. [performance](docs/dev/performance.md) has measurements and their conditions.

## windows tips

Microsoft Defender scans every new file an unrecognized program writes. with many small files, those scans can set the pace: about 21 ms per file was measured for 4 KiB random files.

a dev drive with defender's performance mode scans after the file-open operation completes. [Microsoft's performance mode comparison](https://learn.microsoft.com/en-us/defender-endpoint/microsoft-defender-endpoint-antivirus-performance-mode#performance-mode-compared-to-real-time-protection) states the trade-off:

> Deferring scans until after file open completes inherently provides faster performance, but with less protection. However, enabling performance mode provides significantly better protection than other performance tuning methods, such as using folder exclusions, which block security scans altogether.

creating a dev drive needs an administrator, often your IT team. ask them whether a dev drive with performance mode is suitable for your machine. if yours is `D:`, set the cache in your powershell session:

```powershell
$env:FETCHLOOM_CACHE = 'D:\fetchloom-cache'
```

keep your projects on that same drive so fetchloom can link files instead of copying them. [formats](docs/formats.md#settings) covers cache settings.

## troubleshooting

| message or problem | what to do |
|---|---|
| `<link> is missing` | pass a dataset reference after `get` or `add`; use `fetchloom get --help` for examples |
| `offline, and 1 file the lock names is not in the cache` | run sync online first; remove `--offline`, and unset `FETCHLOOM_OFFLINE` if it is `1` |
| `iris/iris.csv is not the file that was locked` | the download differs from the pinned input. if you intend to use the new data, run `fetchloom lock --upgrade iris`, then sync; otherwise ask the source owner for the pinned file |
| `downloading huge.bin needs 2.0 GiB, and fetchloom cannot ask for confirmation here` | check the download size, then add `--yes` if you approve it |
| `iris/iris.csv already exists with other contents` | move your file somewhere safe, or give `get` another folder with `-o` |
| the host cannot be reached | check your connection and retry; if the host is down, wait for it to recover |
| access is refused | check whether you have access in a browser; use the supported credentials in [formats](docs/formats.md#settings), or ask the owner for a public copy |
| a permission error or a file held open on windows | close the program using the file, check its permissions and retry |
| the disk is full | free space on the affected disk and retry; `fetchloom cache gc` can remove eligible cached files |

use `-v` to see an error's kind and work counters. a failed download can leave reusable bytes in the cache. [agents](docs/agents.md#error-kinds-and-exit-codes) lists exit codes for scripts.

## scripts and agents

use `--json` for a machine-readable answer, including failures. [agents](docs/agents.md) has recipes, offline checks and the `fetchloom.path()` and `fetchloom.sync()` python helpers.

## docs

| doc | use it for |
|---|---|
| [cli](docs/cli.md) | commands, options and output |
| [scope](docs/scope.md) | sources, platforms and limits |
| [formats](docs/formats.md) | manifest, lock, settings and JSON fields |

## contributing

start with [architecture](docs/dev/architecture.md) for the code map and [engineering](docs/dev/engineering.md) for building, testing and releases.

## license

apache-2.0. see [LICENSE](LICENSE) and [NOTICE](NOTICE).

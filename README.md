# fetchloom

fetchloom fetches research data into a project folder. `data.toml` names the datasets; `data.lock` pins each selected file by SHA-256 and size.

a shared local cache reuses the bytes across projects. files land in `data/` by reflink, then hardlink, then copy. recognized zip and tar archives unpack by default.

## install

build from source with rust 1.99.1 and a C toolchain:

```sh
git clone https://github.com/nikhil-yn-aix/fetchloom.git
cd fetchloom
cargo install --path . --locked
fetchloom --version
```

```text
fetchloom 0.1.0
```

both `fetchloom` and `fl` are installed. use `fetchloom` in powershell, where `fl` already means `Format-List`. python wheels and standalone installers are not built yet.

## first use

in your research project:

```sh
fetchloom add zenodo:3608990
fetchloom path iris
```

this adds an `iris` entry to `data.toml`, writes its pin to `data.lock`, and places `iris.csv` in `data/iris/`. `path` prints the absolute dataset path.

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

files are read-only by default. `sync` preserves hand edits unless you use `--force`; it may still exit successfully with conflicts kept. use `status` to inspect them, or read the JSON result in scripts. [CLI](docs/cli.md) covers selection, writable inputs, archives, offline use and cache cleanup.

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
| [engineering](docs/engineering.md) | building and testing |
| [style](docs/style.md) | writing rules |

## license

apache-2.0. see [LICENSE](LICENSE) and [NOTICE](NOTICE).

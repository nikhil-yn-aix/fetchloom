# fetchloom

fetchloom is uv for research data on windows and linux, on x86_64 and arm64.

`data.toml` is your shopping list, `data.lock` is the receipt pinning each file by SHA-256 and size, and the shared cache is your pantry. `fetchloom sync` fills `data/` from that receipt by reflink, hardlink or copy.

```sh
fetchloom add zenodo:3608990
```

stderr excerpt when the filesystem supports links:

```text
     Locked iris · 1 file in data.lock
     Placed data/iris · 1 file linked from the cache
      Added iris, its files are in data/iris
```

commit `data.toml` and `data.lock`, not `data/`. another clone can restore the pinned inputs with `fetchloom sync`.

## install

on linux:

```sh
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh
```

on windows, in powershell:

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.ps1 | iex"
```

the installers choose x86_64 or arm64 and verify the release's `SHA256SUMS`. each installs one executable, `fetchloom`.

with python 3.9 or later:

```sh
pip install fetchloom
```

[getting started](docs/user/getting-started.md#install) covers install locations, pinned installer versions and building from source. macos is not supported.

## quick start

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

run `fetchloom` without arguments outside a project to see that welcome. inside a project, it shows status.

`add` writes the manifest and lock, fills the data folder and adds it to `.gitignore` in a git repository. files are read-only by default. [getting started](docs/user/getting-started.md) walks through a first project and a fresh clone.

sync preserves local edits and can exit 0 with conflicts left in place. use `status` before choosing `sync --force`, which can replace edited and untracked files at locked paths. [scripting](docs/user/scripting.md) shows conflict checks and the python helper.

## docs

| need | read |
|---|---|
| choose a user topic | [user guide](docs/user/README.md) |
| understand the shopping list, receipt and pantry | [concepts](docs/user/concepts.md) |
| look up a command or option | [commands](docs/user/commands/README.md) |
| use a supported host | [sources](docs/user/sources/README.md) |
| write scripts or notebooks | [scripting](docs/user/scripting.md) |
| choose a windows drive or cache location | [windows](docs/user/windows.md) |
| fix a failed download or local conflict | [troubleshooting](docs/user/troubleshooting.md) |
| work on the code | [developer guide](docs/dev/README.md) |
| understand code organization | [architecture](docs/dev/architecture/README.md) |
| build, test or release | [engineering](docs/dev/engineering/README.md) |
| follow project conventions | [style](docs/dev/style.md) |

## license

apache-2.0. see [LICENSE](LICENSE) and [NOTICE](NOTICE).

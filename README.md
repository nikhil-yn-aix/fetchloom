# fetchloom

fetchloom is uv for research data on windows, linux and macos, on x86_64 and arm64.

`data.toml` is your shopping list, `data.lock` is the receipt pinning each file by SHA-256 and size, and the shared cache is your pantry. `fetchloom sync` fills `data/` from that receipt by reflink, hardlink or copy.

```text
$ fetchloom add zenodo:3608990
   Resolved zenodo:3608990 · DatasetIris · 1 file · 4.1 KiB · cc-by-4.0
 Downloaded 1 file · 4.1 KiB in 251 ms
     Locked iris · 1 file in data.lock
     Placed data/iris · 1 file linked from the cache
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

commit `data.toml` and `data.lock`, not `data/`. on another machine, a clone runs `fetchloom sync` and gets the same bytes, checked against the lock.

## install

no release has been published yet. until then, build it from source with [rust](https://rustup.rs):

```sh
cargo install --locked --git https://github.com/nikhil-yn-aix/fetchloom
fetchloom --version
```

[getting started](docs/user/getting-started.md) walks through installing rust on windows, macos and linux, a first project and how to undo it.

after the first release, these will install a prebuilt `fetchloom` for x86_64 or arm64 and check it against the release's `SHA256SUMS`:

```sh
# linux and macos
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh
```

```powershell
# windows powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.ps1 | iex"
```

```sh
# python 3.9 or later
pip install fetchloom
```

## quick start

```sh
fetchloom add zenodo:3608990    # put iris.csv in data/iris and pin it
fetchloom path iris/iris.csv    # print where the file is
fetchloom sync                  # in a cloned project, restore data/
fetchloom search iris           # find datasets by name
fetchloom get zenodo:3608990    # download once, without a project
```

run `fetchloom` with no arguments for a welcome, or for the project's status inside a project. `fetchloom help concepts` explains the ideas and `fetchloom <command> --help` each command.

files in `data/` are read-only by default. `sync` keeps your local edits and reports them as conflicts; check `fetchloom status` before `fetchloom sync --force`, which puts the original files back. [scripting](docs/user/scripting.md) covers JSON output and the python helper.
## docs

| need | read |
|---|---|
| install and try it for the first time | [getting started](docs/user/getting-started.md) |
| choose a user topic | [user guide](docs/user/README.md) |
| understand the shopping list, receipt and pantry | [concepts](docs/user/concepts.md) |
| look up a command or option | [commands](docs/user/commands/README.md) |
| use a supported host | [sources](docs/user/sources/README.md) |
| write scripts or notebooks | [scripting](docs/user/scripting.md) |
| let a coding agent fetch data | [AI agents](docs/user/ai-agents.md) |
| choose a windows drive or cache location | [windows](docs/user/windows.md) |
| use icloud drive, apfs or the macos cache folder | [macos](docs/user/macos.md) |
| fix a failed download or local conflict | [troubleshooting](docs/user/troubleshooting.md) |
| work on the code | [developer guide](docs/dev/README.md) |
| understand code organization | [architecture](docs/dev/architecture/README.md) |
| build, test or release | [engineering](docs/dev/engineering/README.md) |
| follow project conventions | [style](docs/dev/style.md) |

## license

apache-2.0. see [LICENSE](LICENSE) and [NOTICE](NOTICE).

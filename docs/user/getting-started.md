# getting started

fetchloom puts the research data your project needs in `data/` and records how to restore it.

## install

no release or PyPI package has been published yet. install from source with [rust](https://rustup.rs) and a C toolchain:

```sh
cargo install --locked --git https://github.com/nikhil-yn-aix/fetchloom
fetchloom --version
```

fetchloom supports windows and linux on x86_64 and arm64. windows needs windows 10 or later; macos is not supported. [building and testing](../dev/engineering/building-and-testing.md) covers the pinned rust toolchain and platform setup.

for the executable together with the [python helper](scripting.md#python-helper), install the python package from source instead. a [shell alias](windows.md#short-name) can give the executable a shorter name.

## first project

```sh
mkdir iris-study
cd iris-study
fetchloom add zenodo:3608990
```

`add` downloads iris, writes its entry in `data.toml`, pins the files in `data.lock` and places them in `data/iris`. in a git repository it adds the data folder to `.gitignore`.

stderr excerpt when the filesystem supports links:

```text
     Locked iris · 1 file in data.lock
     Placed data/iris · 1 file linked from the cache
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

find the file without assuming where the data folder lives:

```sh
fetchloom path iris/iris.csv
```

## sharing a project

send `data.toml` and `data.lock` with your project. git is optional. to start tracking the project with git after the example above, initialize it and repeat `add`; the existing dataset is reused and `.gitignore` is updated.

| path | keep in git? | reason |
|---|---|---|
| `data.toml` | yes | it records which datasets the project needs |
| `data.lock` | yes | it pins each selected file by hash and size |
| `data/` | no | sync restores downloaded inputs |
| `.fetchloom/` | no | it holds machine-local state and ignores itself |
| `.gitignore` | yes | it keeps downloaded files out of commits |

```sh
git init
fetchloom add zenodo:3608990
git add data.toml data.lock .gitignore
git commit -m "pin iris inputs"
```

## restoring a project

enter a project folder containing `data.toml` and `data.lock`, whether you received the files directly or cloned them with git, then run:

```sh
fetchloom sync
```

sync fills `data/` from the pins. cached bytes need no download. if the manifest changed, sync updates the lock first while preserving unchanged entries. use `fetchloom sync --locked` when a job must require an up-to-date lock.

## working offline

follow [offline preparation](concepts.md#offline), then restore without requests:

```sh
fetchloom sync --offline
```

## next commands

| need | command or guide |
|---|---|
| find a dataset by name | [`fetchloom search`](commands/search.md) |
| see the datasets in a project | [`fetchloom list`](commands/list.md) |
| inspect missing, edited or extra files | [`fetchloom status`](commands/status.md) |
| try data without creating project files | [`fetchloom get`](commands/get.md) |
| use a different host | [sources](sources/README.md) |
| understand hashes, cache and placement | [concepts](concepts.md) |
| use writable inputs | [`copy = true`](concepts.md#placement) |
| handle an error | [troubleshooting](troubleshooting.md) |

# getting started

fetchloom installs as one executable and restores a project's research inputs from a committed lock.

## install

release binaries support windows and linux on x86_64 and arm64. windows needs windows 10 or later; the static linux binary works with any C library. macos is not supported.

on linux:

```sh
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh
```

on windows, in powershell:

```powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.ps1 | iex"
```

the installers select the architecture and check the download against the release's `SHA256SUMS`. installation uses `~/.local/bin` on linux or `%LOCALAPPDATA%\Programs\fetchloom\bin` on windows. the installer adds the folder to shell profiles or the user PATH when needed.

to choose a version, pass it as the first argument or set `FETCHLOOM_VERSION`:

```sh
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh -s -- 0.1.0
```

with pip, on python 3.9 or later:

```sh
pip install fetchloom
```

this also installs the [python helper](scripting.md#python-helper).

from source, with rust 1.99.1 and a C toolchain:

```sh
git clone https://github.com/nikhil-yn-aix/fetchloom.git
cd fetchloom
cargo install --path . --locked
fetchloom --version
```

each method installs one program, `fetchloom`. use a [shell alias](windows.md#short-name) if you want a shorter name.

## first project

```sh
mkdir iris-study
cd iris-study
git init
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

## what to commit

| path | keep in git? | reason |
|---|---|---|
| `data.toml` | yes | it records which datasets the project needs |
| `data.lock` | yes | it pins each selected file by hash and size |
| `data/` | no | sync restores downloaded inputs |
| `.fetchloom/` | no | it holds machine-local state and ignores itself |
| `.gitignore` | yes | it keeps downloaded files out of commits |

```sh
git add data.toml data.lock .gitignore
git commit -m "pin iris inputs"
```

## cloning a project

clone the project's git repository and enter its folder. if it commits both `data.toml` and `data.lock`, run:

```sh
fetchloom sync
```

sync fills `data/` from the pins. cached bytes need no download. if the manifest changed, sync updates the lock first while preserving unchanged entries. use `fetchloom lock --check` first when a job must require an up-to-date committed lock.

## working offline

prepare the project with an online sync before disconnecting. then run:

```sh
fetchloom sync --offline
```

unchanged files can suffice. restoring missing files needs cached objects and, for extracted archives, unpack records. keep the manifest and lock unchanged while offline.

## next commands

| need | command or guide |
|---|---|
| inspect missing, edited or extra files | [`fetchloom status`](commands/status.md) |
| try data without creating project files | [`fetchloom get`](commands/get.md) |
| use a different host | [sources](sources/README.md) |
| understand hashes, cache and placement | [concepts](concepts.md) |
| use writable inputs | [`copy = true`](concepts.md#placement) |
| handle an error | [troubleshooting](troubleshooting.md) |

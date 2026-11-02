# getting started

you will install fetchloom, download a small dataset into a project folder, find the file, restore it on another machine and undo it all.

## open a terminal

a terminal is a window where you type a command and press enter to run it.

- windows: open the start menu, type `powershell` and open windows powershell.
- macos: press cmd and space, type `terminal` and press enter.
- linux: press ctrl, alt and t, or open terminal from your applications.

type each command below on its own line and press enter after it.

## install

no release has been published yet, so the one-line installers and `pip install fetchloom` do not work. until the first release, build fetchloom from its source code. you do this once and it takes a few minutes.

### 1. install rust

rust is the language fetchloom is written in. its installer, rustup, also gives you `cargo`, the command that builds fetchloom.

windows: download `rustup-init.exe` from [rustup.rs](https://rustup.rs) and run it. when it offers to install visual studio's C++ build tools, accept; fetchloom needs them. then press enter to take the default rust install.

macos: install apple's command line tools, then rust:

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

linux: install a C compiler, then rust. on debian or ubuntu:

```sh
sudo apt install build-essential curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

on fedora, use `sudo dnf install gcc curl` for the first line.

close the terminal and open a new one, so it finds `cargo`.

### 2. build and install fetchloom

```sh
cargo install --locked --git https://github.com/nikhil-yn-aix/fetchloom
fetchloom --version
```

the second line prints `fetchloom` and its version. if `cargo install` says your rust is too old, run `rustup update` and try again.

fetchloom runs on windows 10 or later, macos 11 or later and linux, on x86_64 and arm64 machines. for the [python helper](scripting.md#python-helper) as well, install the python package instead. to finish the setup, see [shell completions](commands/completions.md).

after the first release, the [README](https://github.com/nikhil-yn-aix/fetchloom#install) lists one-line installers that need no rust.

## first project

a project is a folder with your work in it. make one and download the iris dataset, a 4.1 KiB table of flower measurements:

```sh
mkdir iris-study
cd iris-study
fetchloom add zenodo:3608990
```

`zenodo:3608990` is the dataset's id: record 3608990 on zenodo. the last lines it prints:

```text
     Locked iris · 1 file in data.lock
     Placed data/iris · 1 file linked from the cache
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

the folder now holds:

| path | what it is |
|---|---|
| `data.toml` | the datasets this project wants, one entry named `iris` |
| `data.lock` | the exact files, with their sizes and SHA-256 fingerprints |
| `data/iris/iris.csv` | the data itself, read-only so you cannot change it by accident |
| `.fetchloom/` | fetchloom's notes about this machine; leave it alone |

the downloaded bytes also live once in a shared cache, so another project that needs iris gets it without a second download. [concepts](concepts.md) explains the cache and how files are placed.

## find your files

open the folder in your file manager:

- windows: `explorer data\iris`
- macos: `open data/iris`
- linux: `xdg-open data/iris`

or print the full path of one file, for code that reads it:

```sh
fetchloom path iris/iris.csv
```

`fetchloom list` shows every dataset in the project with its size and folder.

## restore it somewhere else

copy `data.toml` and `data.lock` to another machine, or commit them to git. leave `data/` and `.fetchloom/` out. there, in the project folder, run:

```sh
fetchloom sync
```

`sync` downloads what is missing and checks every file against `data.lock`, so you get the same bytes.

with git, `add` already put `data/` in `.gitignore` when the folder was a git repository. if you start git later, run `git init` and then `fetchloom add zenodo:3608990` again; it reuses the dataset and writes `.gitignore`. then:

```sh
git add data.toml data.lock .gitignore
git commit -m "pin iris"
```

## undo

remove the dataset from the project:

```sh
fetchloom remove iris
```

this deletes its entry in `data.toml` and `data.lock` and its files in `data/iris`. a file you edited stays. add `--dry-run` first to see what it would delete.

the shared cache still holds the bytes. `fetchloom cache gc` frees bytes no project needs once they are a day old, in case you want them back. to free them now:

```sh
fetchloom cache gc --now
```

to remove the whole project, delete its folder. to uninstall fetchloom, run `cargo uninstall fetchloom`, then delete the cache folder listed in [cache folder](reference/cache-folder.md#location).

## next

| to | read |
|---|---|
| find a dataset by name | [`search`](commands/search.md) |
| try data without a project | [`get`](commands/get.md) |
| see missing or edited files | [`status`](commands/status.md) |
| use another site, such as kaggle or hugging face | [sources](sources/README.md) |
| work without internet | [offline](concepts.md#offline) |
| change files in `data/` | [writable copies](concepts.md#placement) |
| fix an error | [troubleshooting](troubleshooting.md) |

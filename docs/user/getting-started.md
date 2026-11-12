# getting started

you will install fetchloom, download a small dataset into a project folder, find the file, restore it on another machine and undo it all.

## open a terminal

a terminal is a window where you type a command and press enter to run it.

- windows: open the start menu, type `powershell` and open windows powershell.
- macos: press cmd and space, type `terminal` and press enter.
- linux: press ctrl, alt and t, or open terminal from your applications.

type each command below on its own line and press enter after it.

with a screen reader, run `export TERM=dumb` in that terminal first, or `$env:TERM = 'dumb'` in powershell. fetchloom then writes plain lines: no colors, no redrawn progress bars, and ` - ` in place of symbols such as `·`. it also asks no questions, so a download over 1 GiB needs `--yes`, and plain words such as `iris` list the matching datasets, to add by their id, instead of opening a list to move through. `-q` keeps only answers, warnings and errors, `NO_COLOR=1` turns off colors alone, and `--json` gives one document for a script.

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

cargo installs `fetchloom` alone, while the installers also make `fl`, a short name for it. on linux and macos, this line makes the same link in cargo's folder; skip it when `command -v fl` prints another program:

```sh
ln -s fetchloom ~/.cargo/bin/fl
```

in powershell, `fl` means `Format-List`; [windows](windows.md#short-name) shows how to get the short name there.

fetchloom runs on windows 10 or later, macos 11 or later and linux, on x86_64 and arm64 machines. for the [python helper](scripting.md#python-helper) as well, install the python package instead. to finish the setup, see [shell completions](commands/completions.md).

after the first release, the [README](https://github.com/nikhil-yn-aix/fetchloom#install) lists one-line installers that need no rust.

## first project

a project is a folder with your work in it. make one and add the iris dataset, a small table of flower measurements:

```sh
mkdir iris-study
cd iris-study
fetchloom add iris
```

fetchloom searches hugging face, kaggle and zenodo for iris and shows a list. move to `hf:datasets/scikit-learn/iris` with the arrow keys and press enter. to skip the list, give the dataset's link instead:

```sh
fetchloom add https://huggingface.co/datasets/scikit-learn/iris
```

either way, it ends with:

```text
   Resolved hf:datasets/scikit-learn/iris@0bda0ce · 4 files · 18.1 KiB · cc0-1.0
 Downloaded 4 files · 18.1 KiB in 287 ms
    Started a project in /home/ada/iris-study
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
  next: commit data.toml and data.lock; anyone who clones runs `fetchloom sync`
```

the folder now holds:

| path | what it is |
|---|---|
| `data.toml` | the datasets this project wants, one entry named `iris` |
| `data.lock` | the exact files, with their sizes and SHA-256 |
| `data/iris/Iris.csv` | the table, next to the other files the dataset ships, all read-only so you cannot change them by accident |
| `.fetchloom/` | fetchloom's notes about this machine; leave it alone |

the downloaded bytes also live once in a shared cache, so another project that needs iris gets it without a second download. [concepts](concepts.md) explains the cache and how files are placed.

## find your files

open the folder in your file manager:

- windows: `explorer data\iris`
- macos: `open data/iris`
- linux: `xdg-open data/iris`

or print the full path of one file, for code that reads it:

```sh
fetchloom path iris/Iris.csv
```

`fetchloom list` shows every dataset in the project with its size and folder.

## restore it somewhere else

copy `data.toml` and `data.lock` to another machine, or commit them to git. leave `data/` and `.fetchloom/` out. there, in the project folder, run:

```sh
fetchloom sync
```

`sync` downloads what is missing and checks every download against `data.lock`, so you get the same bytes. files it already verified are reused without hashing them again; `fetchloom cache verify` rehashes the cache when you want to be sure.

with git, `add` already put `data/` in `.gitignore` when the folder was a git repository, and its `next:` line then says `commit data.toml, data.lock and .gitignore`. if you start git later, run `git init` and then `fetchloom sync`, which writes `.gitignore`. then:

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

to remove the whole project, delete its folder. to remove every copy of a dataset from a machine, as a data-use agreement can require, follow [deleting a dataset from a machine](concepts.md#deleting-a-dataset-from-a-machine).

## uninstall

1. sign out of anything you signed in to with fetchloom, if you want those keys gone too: `fetchloom auth` lists them, and `fetchloom auth logout <account>` removes one. keys you saved with a source's own tools, such as `~/.kaggle` or the hugging face token, are shared with those tools.
2. run `fetchloom -q cache` to find the cache folder, then delete it if you no longer need its bytes. this respects `FETCHLOOM_CACHE` and, on linux, an absolute `XDG_CACHE_HOME`; the [cache locations](reference/cache-folder.md#location) list the defaults.
3. remove the program, the way you installed it:

| installed with | remove with |
|---|---|
| `cargo install` | `cargo uninstall fetchloom`, and `rm ~/.cargo/bin/fl` if you made that link |
| `pip install fetchloom` | `pip uninstall fetchloom` |
| `install.sh` | `rm ~/.local/bin/fetchloom ~/.local/bin/fl`, and the line marked `# added by the fetchloom installer` in your shell profiles. if the installer said it left `fl` alone, that `fl` is another program: remove only `fetchloom` |
| `install.ps1` | delete `%LOCALAPPDATA%\Programs\fetchloom`, which holds `fl.exe` too, and that folder's `bin` from your user PATH in the environment variables settings |

projects keep their `data.toml`, `data.lock` and `data/`; delete them with the project.

## next

| to | read |
|---|---|
| find a dataset by name | [`search`](commands/search.md) |
| try data without a project | [`get`](commands/get.md) |
| see missing or edited files | [`status`](commands/status.md) |
| use another site, such as kaggle or hugging face | [sources](sources/README.md) |
| move a download step from curl, gdown, kaggle or pooch | [other tools](other-tools.md) |
| work without internet | [offline](concepts.md#offline) |
| keep data between sessions of a hosted notebook or a container | [notebooks and containers](notebooks.md) |
| change files in `data/` | [writable copies](concepts.md#placement) |
| fix an error | [troubleshooting](troubleshooting.md) |

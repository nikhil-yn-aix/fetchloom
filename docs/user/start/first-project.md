# Your first project

install fetchloom, add a small dataset to a project, find its files, restore them on another machine and undo it all.

## first project

a project is a folder with your work in it. make one and add the iris dataset, a small table of flower measurements:

```sh
mkdir iris-study
cd iris-study
fetchloom add iris
```

fetchloom searches hugging face, kaggle, zenodo, GEO and openneuro for iris and shows a list. move to `hf:datasets/scikit-learn/iris` with the arrow keys and press enter. to skip the list, give the dataset's link instead:

```sh
fetchloom add https://huggingface.co/datasets/scikit-learn/iris
```

either way, it ends with:

```text
   Resolved hf:datasets/scikit-learn/iris@0bda0ce · 4 files · 18.1 KiB · cc0-1.0
 Downloaded 4 files · 18.1 KiB in 287 ms
    Started a project in ~/iris-study
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

the downloaded bytes also live once in a shared cache, so another project that needs iris gets them without a second download. [concepts](../concepts/README.md) explains the cache and how files are placed.

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

commit `data.toml` and `data.lock` to git, or copy them to another machine; leave `data/` and `.fetchloom/` out. in a git repository, `add` already put `data/` in `.gitignore`, and its `next:` line said `commit data.toml, data.lock and .gitignore`. if you start git later, `git init` and then `fetchloom sync` write it. then:

```sh
git add data.toml data.lock .gitignore
git commit -m "pin iris"
```

on the other machine, in the project folder, run:

```sh
fetchloom sync
```

`sync` downloads what is missing and checks every download against `data.lock`, so you get the same bytes. files it already checked are not hashed again; `fetchloom cache verify` rehashes the cache when you want to be sure.

## undo

remove the dataset from the project:

```sh
fetchloom remove iris
```

this deletes its entry in `data.toml` and `data.lock` and its files in `data/iris`, except a file you edited. with `--dry-run`, it only shows what it would delete.

the shared cache still holds the bytes. `fetchloom cache gc` frees bytes no project needs once they are a day old, in case you want them back. to free them now:

```sh
fetchloom cache gc --now
```

to remove the whole project, delete its folder. to remove every copy of a dataset from a machine, as a data-use agreement can require, follow [deleting a dataset from a machine](../guides/free-space.md#deleting-a-dataset-from-a-machine).

## next

| to | read |
|---|---|
| find a dataset by name | [`search`](../commands/search.md) |
| try data without a project | [`get`](../commands/get.md) |
| see missing or edited files | [`status`](../commands/status.md) |
| use another site, such as kaggle or hugging face | [sources](../sources/README.md) |
| get at private or gated data | [signing in](../guides/sign-in.md) |
| move a download step from curl, gdown, kaggle or pooch | [other tools](../guides/other-tools.md) |
| work without internet | [offline](../concepts/offline-and-recovery.md#offline) |
| keep data between sessions of a hosted notebook or a container | [notebooks and containers](../guides/notebooks-and-containers.md) |
| change files in `data/` | [writable copies](../concepts/cache-and-placement.md#placement) |
| fix an error | [troubleshooting](../troubleshooting/README.md) |

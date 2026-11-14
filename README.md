# fetchloom

[![ci](https://github.com/nikhil-yn-aix/fetchloom/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/nikhil-yn-aix/fetchloom/actions/workflows/ci.yml?query=branch%3Amain) [![license](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/nikhil-yn-aix/fetchloom/blob/main/LICENSE)

fetchloom is uv for research data. `fetchloom add` pins the files of a dataset by SHA-256 in your project, one shared cache keeps each file once, and `fetchloom sync` puts the same bytes in `data/` on every machine that clones the project. it runs on windows, linux and macos, on x86_64 and arm64.

```text
$ mkdir iris-study && cd iris-study
$ fetchloom add https://huggingface.co/datasets/scikit-learn/iris
   Resolved hf:datasets/scikit-learn/iris@0bda0ce · 4 files · 18.1 KiB · cc0-1.0
 Downloaded 4 files · 18.1 KiB in 287 ms
    Started a project in ~/iris-study
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
  next: commit data.toml and data.lock; anyone who clones runs `fetchloom sync`
```

commit `data.toml`, the datasets you want, and `data.lock`, their exact files with each one's SHA-256 and size. leave `data/` out; in a git repository, `add` and `sync` put it in `.gitignore`. a clone runs `fetchloom sync` and gets the same bytes, checked against the lock.

`fetchloom add iris` looks the name up on hugging face, kaggle, zenodo, GEO and openneuro, and `fetchloom get <link>` downloads once, without a project. fetchloom reads links and DOIs; zenodo, figshare, dataverse, dryad, mendeley data and OSF; hugging face and kaggle; ENA, GEO, DANDI and OpenNeuro; arXiv papers, and semantic scholar's papers and datasets; github and gitlab releases; google drive and dropbox; and S3, services that speak its API, google cloud storage and azure blob storage. [sources](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/sources/README.md) shows how to write each, and [signing in](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/guides/sign-in.md) covers private data.

## install

no release has been published yet. until then, build it from source with [rust](https://rustup.rs):

```sh
cargo install --locked --git https://github.com/nikhil-yn-aix/fetchloom
fetchloom --version
```

[getting started](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/start/README.md) walks through installing rust on windows, macos and linux, a first project and how to undo it.

after the first release, these installers will install a prebuilt `fetchloom` and check it against the release's `SHA256SUMS`:

```sh
# linux and macos
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh
```

```powershell
# windows powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.ps1 | iex"
```

```sh
# the python package, with the python helper; python 3.10 or later
pip install fetchloom
```

the installers add the program's folder to your PATH; set `FETCHLOOM_NO_MODIFY_PATH=1` to keep them out of your shell profiles. they and the python package also make `fl`, a short name for `fetchloom`, unless another program already answers to it. [getting started](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/start/install.md#uninstall) says how to uninstall.

## quick start

```text
start in a folder for your project:
  mkdir iris-study && cd iris-study
  fetchloom add iris              pick an iris dataset online and pin its files
  fetchloom add ../measurements   copy files from this machine, no network needed

in a project you cloned:
  fetchloom sync                  put the pinned files in data/

what fetchloom writes:
  data.toml   the datasets you asked for
  data.lock   each file's size and SHA-256, so every machine gets the same bytes
  the cache   one copy of each file, shared by all your projects

more: `fetchloom --help` lists every command
docs: https://nikhil-yn-aix.github.io/fetchloom
```

bare `fetchloom` prints this outside a project, below its name and version; inside a project it shows the project's status. [scripting](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/guides/scripts.md) covers JSON output and the [python helper](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/reference/python-helper.md).

## docs

| need | read |
|---|---|
| install, try it and uninstall | [getting started](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/start/README.md) |
| understand data.toml, data.lock, the cache and sync | [concepts](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/concepts/README.md) |
| look up a command or option | [commands](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/commands/README.md) |
| use a supported site, or sign in for private data | [sources](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/sources/README.md), [signing in](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/guides/sign-in.md) |
| fix a failed download or a local conflict | [troubleshooting](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/troubleshooting/README.md) |
| every other topic: scripts, CI, clusters, agents, windows, macos | [user guide](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/README.md) |
| see what changed in each release | [changelog](https://github.com/nikhil-yn-aix/fetchloom/blob/main/CHANGELOG.md) |
| cite it in a paper | [CITATION.cff](https://github.com/nikhil-yn-aix/fetchloom/blob/main/CITATION.cff) |
| work on the code | [developer guide](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/dev/README.md) |

## license

apache-2.0. see [LICENSE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/LICENSE) and [NOTICE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/NOTICE).

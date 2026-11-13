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

`fetchloom add iris` looks the name up on hugging face, kaggle, zenodo, GEO and openneuro, and `fetchloom get <link>` downloads once, without a project. fetchloom reads links and DOIs; zenodo, figshare, dataverse, dryad, mendeley data and OSF; hugging face and kaggle; ENA, GEO, DANDI and OpenNeuro; arXiv and semantic scholar papers; github and gitlab releases; google drive and dropbox; and S3, services that speak its API, google cloud storage and azure blob storage. [sources](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/sources/README.md) shows how to write each, and [signing in](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/signing-in.md) covers private data.

## install

no release has been published yet. until then, build it from source with [rust](https://rustup.rs):

```sh
cargo install --locked --git https://github.com/nikhil-yn-aix/fetchloom
fetchloom --version
```

[getting started](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/getting-started.md) walks through installing rust on windows, macos and linux, a first project and how to undo it.

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

the installers add the program's folder to your PATH; set `FETCHLOOM_NO_MODIFY_PATH=1` to keep them out of your shell profiles. they and the python package also make `fl`, a short name for `fetchloom`, unless another program already answers to it. [getting started](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/getting-started.md#uninstall) says how to uninstall.

## quick start

```text
fetchloom puts the exact research data a project needs into its data/ folder.

  data.toml  the datasets you want
  pinning    recording a dataset's exact files and their SHA-256 in data.lock
  data.lock  the list of pinned files, so every machine gets the same bytes
  the cache  one folder that keeps each file once, for all your projects

  fetchloom add iris    choose an iris dataset, put it in data/ and pin it
  fetchloom get iris    download it once, without a project
  fetchloom sync        fill data/ in a project you cloned

a name is looked up on hugging face, kaggle, zenodo and more. a link works too:
  fetchloom add https://zenodo.org/records/3608990

learn more: `fetchloom --help` lists the commands;
`fetchloom help <topic>` explains concepts, recipes, environment or exit-codes
```

bare `fetchloom` prints this outside a project, after its version line; inside a project it shows the project's status. [scripting](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/scripting.md) covers JSON output and the [python helper](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/scripting.md#python-helper).

## docs

| need | read |
|---|---|
| install, try it and uninstall | [getting started](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/getting-started.md) |
| understand data.toml, data.lock, the cache and sync | [concepts](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/concepts.md) |
| look up a command or option | [commands](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/commands/README.md) |
| use a supported site, or sign in for private data | [sources](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/sources/README.md), [signing in](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/signing-in.md) |
| fix a failed download or a local conflict | [troubleshooting](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/troubleshooting.md) |
| every other topic: scripts, CI, clusters, agents, windows, macos | [user guide](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/README.md) |
| see what changed in each release | [changelog](https://github.com/nikhil-yn-aix/fetchloom/blob/main/CHANGELOG.md) |
| cite it in a paper | [CITATION.cff](https://github.com/nikhil-yn-aix/fetchloom/blob/main/CITATION.cff) |
| work on the code | [developer guide](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/dev/README.md) |

## license

apache-2.0. see [LICENSE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/LICENSE) and [NOTICE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/NOTICE).

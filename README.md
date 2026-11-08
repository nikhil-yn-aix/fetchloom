# fetchloom

fetchloom is uv for research data on windows, linux and macos, on x86_64 and arm64.

```text
$ fetchloom add https://huggingface.co/datasets/scikit-learn/iris
   Resolved hf:datasets/scikit-learn/iris@0bda0ce · 4 files · 18.1 KiB · cc0-1.0
 Downloaded 4 files · 18.1 KiB in 287 ms
    Started a project in /home/ada/iris-study
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

`fetchloom add iris` does the same from a name: it searches hugging face, kaggle and zenodo and lets you pick. a link, a DOI or an id such as `kaggle:uciml/iris` works too, and `fetchloom get iris` just downloads without a project.

`data.toml` is your shopping list, `data.lock` is the receipt pinning each file by SHA-256 and size, and the shared cache is your pantry. `fetchloom sync` fills `data/` from that receipt by reflink, hardlink or copy.

commit `data.toml` and `data.lock`, not `data/`. on another machine, a clone runs `fetchloom sync` and gets the same bytes, checked against the lock.

## install

no release has been published yet. until then, build it from source with [rust](https://rustup.rs):

```sh
cargo install --locked --git https://github.com/nikhil-yn-aix/fetchloom
fetchloom --version
```

[getting started](docs/user/getting-started.md) walks through installing rust on windows, macos and linux, a first project and how to undo it.

after the first release, these shell installers will install a prebuilt `fetchloom` for x86_64 or arm64 and check it against the release's `SHA256SUMS`:

```sh
# linux and macos
curl -LsSf https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.sh | sh
```

```powershell
# windows powershell
powershell -ExecutionPolicy ByPass -c "irm https://github.com/nikhil-yn-aix/fetchloom/releases/latest/download/install.ps1 | iex"
```

the python package will install the executable and [python helper](docs/user/scripting.md#python-helper):

```sh
# python 3.9 or later
pip install fetchloom
```

the installers add the program's folder to your PATH; set `FETCHLOOM_NO_MODIFY_PATH=1` to keep them out of your shell profiles. [getting started](docs/user/getting-started.md#uninstall) says how to uninstall.

## quick start

```text
fetchloom puts the exact research data a project needs into its data/ folder.

  data.toml, the shopping list: which datasets you want
  data.lock, the receipt: the exact files and their fingerprints
  the cache, the pantry: shared by projects using it
  sync, restocking: fill data/ from the receipt

  fetchloom add iris    pick an iris dataset, put it in data/ and pin it
  fetchloom get iris    just download it, without a project
  fetchloom sync        fill data/ in a project you cloned

a name searches hugging face, kaggle and zenodo. a link works as well:
  fetchloom add https://huggingface.co/datasets/scikit-learn/iris

learn more with `fetchloom help concepts` and `fetchloom <command> --help`
```

that is the introductory part of the welcome outside a project. it follows a version line and, at a wide terminal, a wordmark. inside a project, bare `fetchloom` shows its status.

files in `data/` are read-only by default. `sync` keeps your local edits and reports them as conflicts; check `fetchloom status` before `fetchloom sync --force`, which puts the original files back. [scripting](docs/user/scripting.md) covers JSON output and the python helper.

## docs

| need | read |
|---|---|
| install and try it for the first time, or uninstall | [getting started](docs/user/getting-started.md) |
| choose a user topic | [user guide](docs/user/README.md) |
| understand the shopping list, receipt and pantry | [concepts](docs/user/concepts.md) |
| look up a command or option | [commands](docs/user/commands/README.md) |
| use a supported host | [sources](docs/user/sources/README.md) |
| check supported systems, terminals and versions | [compatibility](docs/user/reference/compatibility.md) |
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

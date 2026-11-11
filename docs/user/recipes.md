# recipes

a recipe makes a dataset from other datasets with a command of yours, such as a script that cleans a raw table. `data.toml` declares it, `fetchloom build` runs it and pins what it made in `data.lock`, and fetchloom tells you when the pin no longer matches the recipe. only `fetchloom build` runs your code.

## declare a recipe

a recipe is a dataset with `from` and `run` instead of `ref`:

```toml
[datasets.iris]
ref = "hf:datasets/scikit-learn/iris"
select = "Iris.csv"

[datasets.clean]
from = ["iris"]
run = ["python3", "clean.py", "{iris}", "{out}"]
code = ["clean.py"]
env = { SOURCE_DATE_EPOCH = "315532800" }
```

| key | meaning |
|---|---|
| `from` | the datasets it is made from, by their names in `data.toml`: fetched datasets or other recipes |
| `run` | the program and its arguments, one word each, never read by a shell. `{iris}` stands for the folder holding a copy of iris's files, `{out}` for the folder the step writes its result in, and `{{` and `}}` for a brace of their own |
| `code` | files and folders of the project whose bytes the result depends on, such as the script and the lock file of its environment |
| `env` | variables set for the step alone |

[data.toml](reference/data-toml.md#recipes) lists every rule. the ones you meet first:

- a dataset is fetched or made, never both: a recipe has no `ref`, `select`, `exclude` or `extract`, and choosing files belongs inside the step.
- `run` names the folder of every dataset in `from` and `{out}`, so the step can find what it reads and has somewhere to write.
- recipes cannot be made from each other: `datasets a and b are made from each other: a → b → a`.
- `code` stays inside the project and out of the data folder, `data.lock`, `data.lock.d` and repository metadata.
- `env` never sets fetchloom's own variables or anything named like a secret, such as `HF_TOKEN`, since `data.toml` and `data.lock` are shared. the step inherits the environment of whoever builds it, so keys stay there.

a step runs in the project folder, so `clean.py` in `run` is the project's own file.

## what fetchloom keys

a recipe's result is named by a key: the SHA-256 of what it is made from. the key holds

- the content of each dataset in `from`, as `data.lock` pins it: each file's path, size and SHA-256, and for an archive that unpacks, its path and the tree of what it unpacks to, never the archive's own bytes, so a source that zips the same files again keeps the content. a [members table](reference/data-toml.md#files-inside-archives) that keeps or leaves out files inside an archive changes it, since the step finds other files. links, versions and when it was pinned stay out, so a source that moves its files keeps the content. a recipe made from another recipe takes that recipe's result tree, so a rebuild that gives the same files changes nothing after it.
- `run` as written, before any folder is put in.
- the bytes of each file of `code`, a folder as the tree of its files. a text file, one with no NUL byte in its first 8,000 bytes as git decides, counts with each CRLF made LF, since git for windows checks files out with CRLF; a linux and a windows clone get one key. a folder leaves out `.git`, `.hg`, `.svn`, `__pycache__`, `.ipynb_checkpoints`, `.DS_Store` and `Thumbs.db`, and a link or a special file anywhere in `code` is refused.
- `env`.

the key holds nothing of the machine: no path, user, system, time or fetchloom version, and not the recipe's own name. a moved project, a renamed data folder or a renamed recipe keeps its key. your environment is in the key only as far as `code` names its lock files, such as `uv.lock`, `requirements.txt` or `environment.yml`.

## when a recipe needs building

`data.lock` pins a recipe's result with its key, once a build makes it. a recipe needs building when there is no pin yet, or when what its key holds moved since the pin: a dataset in `from` was pinned to other content, `run` or `env` changed, or a file of `code` changed or went missing.

`fetchloom status` lists such a recipe under `unlocked`, with what moved:

```text
   Unlocked clean · clean.py changed; env changed
      Found 1 difference from data.lock
  hint: `fetchloom build --dry-run` shows the commands that would build them
```

`fetchloom lock --check`, `fetchloom sync --locked` and a [`fetchloom push`](commands/push.md) of the recipe fail with `build.needed`, exit 60, naming what moved:

```text
error: data.lock is out of date: clean changed since it was built
  clean: clean.py changed
  help: run `fetchloom build clean`, then commit data.lock and data.lock.d
```

a fetched dataset that is out of date fails first with `lock.outdated`, exit 1, since `fetchloom lock` must pin it before a build can read it. the JSON `fix` of `build.needed` is always null: building runs the project's code, so a script or an agent asks a person first.

`fetchloom push --inputs <recipe>` copies a recipe's pinned result and every dataset it is made from into a folder [mirror](mirrors.md), so a reviewer whose sources are gone can rebuild it from there.

no command but `build` runs a step. `sync` places a pinned result from the cache like any dataset, also after its recipe changed, and never builds it; `sync` and `path` then warn what changed, naming the build that makes it again. a recipe not built yet gets the same warning from a sync of every dataset. a pinned result neither the cache nor a [mirror](mirrors.md) holds, as in a fresh clone, is left out of a sync of every dataset with a warning naming `fetchloom build` with the recipe, `status` lists it under `unbuilt` with that build as its hint, and `sync <recipe>` and `sync --locked` fail with `build.needed`, exit 60, as [sync](commands/sync.md#recipe-results) shows. `sync <recipe>` and `path <recipe>` work on that recipe alone: they read the pins of what it is made from to tell that one changed, and fetch nothing of them. `lock` keeps a recipe's pin while `data.toml` has the recipe, and `sync` and `lock` drop it with the recipe, as they drop a removed dataset. a dataset a recipe is made from cannot be removed while the recipe reads it. `fetchloom path` answers a built recipe's folder, and fails with `build.needed` for one never built. `fetchloom export` lists a result's files with their SHA-256, and `cache gc` keeps the files of every result a project's `data.lock` pins.

## build

`fetchloom build --dry-run` lists the steps in the order a build runs them, each with its command, what it reads and where its result goes, and runs nothing:

```text
  Would run clean · not built yet
            python3 clean.py <build>/in/iris <build>/out
            reads iris and clean.py; writes data/clean
         Ok dry run, nothing was run or changed
  hint: <build> is a folder a build makes in the cache for each step: the step finds a copy of each dataset it reads in <build>/in, and what it writes in <build>/out becomes its dataset
```

read the command before you build: a recipe in a repository you cloned runs as you, with your files and keys, and fetchloom has no sandbox. then `fetchloom build` runs each step that needs it, pins what it made and places it in `data/clean`:

```text
    Running clean · python3 clean.py <build>/in/iris <build>/out
            kept 2 of 3 rows
      Built clean · 1 file, 46 B in 31 ms
     Placed data/clean · 1 file linked to the cache
         Ok pinned clean's result in data.lock
```

the step reads a private copy of each dataset in `from`, made from the files `data.lock` pins, and writes its result into `{out}`, plain files whose names every system can store. the copy comes from the cache, fetched first when it lacks a file, and never from the data folder, so a dataset imported from a folder needs `fetchloom add <folder> --as <name>` on a machine whose cache lacks it. a step that fails leaves `data.lock` and the data folder as they were, and its output is in `.fetchloom/builds/clean.log`. commit `data.lock` and `data.lock.d` with your code, so a teammate's build of the same commit finds the result pinned. [build](commands/build.md) has the details, the errors and the JSON.

a step should make the same files from the same inputs. one that does not, as one using the time or an unseeded random number, fails with `build.differs` when it runs again for a pin whose files are not on the machine, and `data.lock` keeps the files it pins.

## keep only the result

raw data a recipe reads need not sit in the data folder too. `place = false` on it keeps it pinned in `data.lock` and its files in the cache for builds, and places nothing of it. it is the first-time researcher's setup: the raw table from a source, a cleaned one the project uses. data.toml after `fetchloom add <link> --as raw`:

```toml
[datasets.raw]
place = false
ref = "https://example.org/Iris.csv"

[datasets.clean]
from = ["raw"]
run = ["python3", "clean.py", "{raw}", "{out}"]
code = ["clean.py"]
```

the build places the result and retires what the add placed of raw, unchanged files only, an edited one staying with a warning:

```text
    Running clean · python3 clean.py <build>/in/raw <build>/out
            kept 4 of 6 rows
      Built clean · 1 file, 200 B in 32 ms
     Placed data/clean · 1 file linked to the cache
    Retired data/raw · 1 file, since raw has place = false
         Ok pinned clean's result in data.lock
```

`status` and `list` say raw is not placed, `fetchloom path raw` refuses it with exit 2, and `cache gc` keeps its bytes, since the next build reads them:

```text
 Not placed raw · place = false in data.toml
         Ok everything matches data.lock by size and time
```

a teammate's sync of the same commit places clean from their cache or a mirror and fetches nothing of raw. with neither holding clean, as in a fresh clone, the sync leaves it out, and `status` names the build, and raw, which it reads and the cache lacks too:

```text
 Not placed raw · place = false in data.toml
    Unbuilt clean · its pinned result is not on this machine or a mirror, nor is raw, which it is made from
      Found 1 difference from data.lock
  hint: `fetchloom build clean` makes it on this machine
```

their `fetchloom build clean` fetches raw into the cache, runs the step and places clean, still placing nothing of raw. the examples are stderr from one run against a server on this machine.

# Make a dataset with a recipe

a recipe makes a dataset from other datasets with a command of yours, such as a script that cleans a raw table. `data.toml` declares it, `fetchloom build` runs it and pins what it made in `data.lock`, and fetchloom tells you when the pin no longer matches the recipe. only `fetchloom build` runs your code.

## declare a recipe

a recipe is a dataset with `from` and `run` instead of `ref`, as `fetchloom help recipes` shows at the terminal:

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

[data.toml](../reference/recipes.md) lists every rule. the ones you meet first:

- a dataset is fetched or made, never both: a recipe has no `ref`, `select`, `exclude` or `extract`, and choosing files belongs inside the step.
- `run` names the folder of every dataset in `from` and `{out}`, so the step can find what it reads and has somewhere to write.
- recipes cannot be made from each other: `datasets a and b are made from each other: a → b → a`.
- `code` stays inside the project and out of the data folder, `data.lock`, `data.lock.d` and repository metadata.
- `env` never sets fetchloom's own variables or anything named like a secret, such as `HF_TOKEN`, since `data.toml` and `data.lock` are shared. the step inherits the environment of whoever builds it, so keys stay there.

a step runs in the project folder, so `clean.py` in `run` is the project's own file.

## build

`fetchloom build --dry-run` lists the steps in the order a build runs them, each with its command, what it reads and where its result goes, and runs nothing, as [dry run](../commands/build.md#dry-run) shows with every action. read the command before you build: a recipe in a repository you cloned runs as you, with your files and keys, and fetchloom has no sandbox. in a fresh clone the dry run says `Would run clean · its pinned result is not on this machine or a mirror`, or `Would fetch` where the data folder or a [mirror](mirrors.md) holds the result, so nothing runs. then `fetchloom build` runs each step that needs it, pins what it made and places it in `data/clean`, as [what happens](../commands/build.md#what-happens) shows.

the step reads a private copy of each dataset in `from`, made from the files `data.lock` pins, never from the data folder, where you may have changed a file, and writes its result into `{out}`: plain files whose names every system can store. a step that fails leaves `data.lock` and the data folder as they were, and its output is in `.fetchloom/builds/clean.log`. commit `data.lock` and `data.lock.d` with your code, so a teammate's build of the same commit finds the result pinned; a build warns of each file of `code` git does not have. [build](../commands/build.md) has the details, the errors and the JSON.

an edit of a placed file, which `copy = true` allows, never reaches a step, and `build` says so, naming the file, as [editing raw files](edit-input-files.md#editing-raw-files) shows. an edit of a result's own file, which `copy = true` on the recipe allows, stays too when a build pins another result, so the data folder keeps the earlier version, and the build says so (stderr):

```text
      Needs up to 51 B on the disk holding the cache, which has 322.2 GiB free
    Running mc · python3 c.py <build>/in/m <build>/out
            kept 2 of 4 rows
      Built mc · 1 file, 19 B in 32 ms
warning: data/mc/measure_clean.csv has changed, so it stays, but it is an edit of an earlier version of mc, not what data.lock pins now
  help: `fetchloom sync --force mc` replaces it
         Ok pinned mc's result in data.lock
```

[`fetchloom build --check`](../commands/build.md#check) runs every step again from the inputs `data.lock` pins and compares what each makes with `data.lock`, file by file, without changing it or the data folder. a step should make the same files from the same inputs. one that does not, as one using the time or an unseeded random number, or one run with another version of a program or library, fails with `build.differs` when a check or a rebuild runs it, and `data.lock` keeps the files it pins. `fetchloom status` lists the files the rerun made as a kept rebuild, as `` Kept emb · a rebuild that differs from data.lock; `fetchloom build --accept emb` pins it ``, with the hint `` `fetchloom status -v` lists where the files of each kept rebuild that differ from data.lock are ``. look at them before you choose them, then `fetchloom build --accept emb` pins and places them without running anything, as [a rebuild that differs](../commands/build.md#a-rebuild-that-differs) shows.

## keep only the result

raw data a recipe reads need not sit in the data folder too. `place = false` on it keeps it pinned in `data.lock` and its files in the cache for builds, and places nothing of it: the raw table comes from a source, and the project uses the cleaned one. data.toml after `fetchloom add <link> --as raw`:

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

to leave a bad sample of raw out of what clean reads, run [`fetchloom drop`](../commands/drop.md#raw-data-only-recipes-read) with the path the file would have in the data folder, as for any dataset. `status` and `list` say raw is not placed, as `Not placed raw · place = false in data.toml`, `fetchloom path raw` refuses it with exit 2, and `fetchloom cache` counts its bytes as read only by builds; `cache gc` keeps them, since the next build reads them.

a teammate's sync of the same commit places clean from their cache or a mirror and fetches nothing of raw. with neither holding clean, as in a fresh clone, the sync leaves it out, and `status` names the build, and raw, which it reads and the cache lacks too, as [status](../commands/status.md#what-happens) shows. their `fetchloom build clean` fetches raw into the cache, runs the step and places clean, still placing nothing of raw. a plain [`fetchloom push`](../commands/push.md) sends clean and leaves raw off the mirror, and `push --inputs clean` sends raw too, for a teammate who must rebuild. the examples are stderr from one run against a server on this machine.

raw stays in the cache, since a build that runs clean again reads it. to free the disk it takes, put it on a folder [mirror](mirrors.md), then free the cache's copy, naming that mirror:

```sh
fetchloom push --inputs clean --to /mnt/lab/fetchloom
fetchloom cache gc --inputs --mirror /mnt/lab/fetchloom
```

[`cache gc --inputs`](../commands/cache.md#raw-input-a-mirror-holds) reads each copy on the mirror, says what it would free and what that costs, and asks first; clean's result stays, and a build that runs clean again takes raw from the mirror, or from its source.

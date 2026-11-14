# build

`build` runs the [recipes](../guides/recipes.md) in `data.toml`: each step makes a dataset from other datasets, and `build` pins what it made in `data.lock` and places it in the data folder like any dataset. a step is your project's code, so no other command runs one; `--dry-run` shows the commands first, and `fetchloom help recipes` shows how to write a recipe.

```sh
fetchloom build --dry-run
fetchloom build
fetchloom build clean
fetchloom build --check
fetchloom build --json
```

## usage

```sh
fetchloom build [options] [name]...
```

`name` is a recipe in `data.toml`, a dataset with `from` and `run`. a build takes the named recipes and every recipe they are made from, directly or not; without names it takes every recipe. a name data.toml lacks is a usage error, exit 2, that suggests the closest name, and so is the name of a dataset fetched from a source:

```text
error: iris is fetched, not built; `fetchloom sync` places it
  help: name the recipes to build, the datasets data.toml makes with from and run
```

| option | effect |
|---|---|
| `--check` | run every step again from the inputs `data.lock` pins and compare what each makes with `data.lock`, file by file; `data.lock` and the data folder stay as they are. [check](#check) has the details |
| `--accept` | pin the files a build kept because they differ from what `data.lock` pins for the same inputs, command and code, and place them; run nothing. [a rebuild that differs](#a-rebuild-that-differs) has the details |
| `--prepare` | put in the cache everything the build, or with `--check` the check, needs, from a mirror first, and run nothing, as on a login node before a job builds without the network. [prepare](#prepare) has the details |
| `--locked` | fail with `lock.outdated`, exit 1, instead of changing `data.lock`, for CI: before anything is fetched or run when `data.lock` does not pin every dataset the steps need as `data.toml` asks, and when building would change a pin anyway |
| `-n`, `--dry-run` | say what the build would do with each step, and the command, inputs and result folder of each that would or may run; run nothing. [dry run](#dry-run) has the details |

[global options](global-options.md) apply.

## what happens

1. find the project and take its lock, which the build holds until it ends.
2. pin the fetched datasets the steps read, resolving those whose entry changed as `sync` would, without writing `data.lock` yet. with `--locked`, a fetched dataset whose entry changed, a recipe never built or changed since it was built, or a pin building would change, as one of a recipe `data.toml` no longer has, stops the build with `lock.outdated`, exit 1, before anything is fetched or run.
3. say how much room the steps that will run may take on the disk holding the cache, as `Needs up to 91 B on the disk holding the cache, which has 341.3 GiB free`: the files they read that the cache lacks, their results as `data.lock` last pinned them and, where the disk does not clone files, copies of what the largest step reads. a result never built is named instead, since its size is unknown. when that plus the `build_floor` [setting](settings.md) passes the free space, the build asks first, and with no one to ask stops with `unconfirmed`, exit 1, unless `--yes` is given. a disk already below `build_floor` stops the build with `local.io`, exit 50, even with `--yes`, as [the step](../reference/recipes.md#the-step) shows.
4. take each recipe in order, each after the datasets it is made from, ties by name:
    - `Up to date` when `data.lock` pins what building it now would make and the cache holds those files;
    - `Fetched` when `data.lock` pins what building it now would make and the data folder or a [mirror](../guides/mirrors.md) holds the files the cache lacks, which it copies into the cache;
    - `Reused` when an earlier build of the same key, here or in another project that shares the cache, made a result the cache holds whole;
    - otherwise put in the cache the files of the datasets it reads, from the data folder, a mirror or their source, then run its step, as [the step](../reference/recipes.md#the-step) describes, and take in what it wrote. an [imported](add.md#files-on-this-machine) dataset has no source, so where the cache and the data folder lack it the build stops with `reference.unresolved`, exit 10, naming `fetchloom add <folder> --as <name>`.
5. write `data.lock` and `data.lock.d` once, after the last step, with each result's key and the list of its files.
6. place the datasets the build took in the data folder, as `fetchloom sync` with their names would. a file you edited stays, and the warning names `fetchloom sync --force` with that recipe alone.
7. warn of each file of a recipe's `code` that git does not have, as `fetchloom status` does: a file never added, or one `.gitignore` leaves out, which a clone would lack. a build that ran no step warns too.

from a project where `clean` is made from `iris` with `python3 clean.py`:

```text
      Needs up to 72 B, besides what clean writes, on the disk holding the cache, which has 341.4 GiB free
    Running clean · python3 clean.py <build>/in/iris <build>/out
            kept 2 of 3 rows
      Built clean · 1 file, 63 B in 32 ms
     Placed data/clean · 1 file linked to the cache
         Ok pinned clean's result in data.lock
```

the dim line under `Running` is the step's own output. a build of what `data.lock` already pins runs nothing:

```text
 Up to date clean
         Ok every recipe is built as data.lock pins it
```

where neither the cache, the data folder nor a mirror holds the result `data.lock` pins, as in a fresh clone, the build runs the step again; a copy of the project that brought its data folder along takes the result from there, as [moving a project](cache.md#moving-a-project-to-another-machine) describes. a step that makes the same files leaves `data.lock` as it is and ends with `Ok clean's result matches data.lock`; one that makes other files fails, as [a rebuild that differs](#a-rebuild-that-differs) describes.

in a git repository, a build whose recipe has code git does not have says so before its last line, here for `made`, whose `step.py` was never added (stderr):

```text
     Placed data/made · 1 file linked to the cache
warning: git does not have step.py, code of made
  help: run `git add step.py`, then commit it, since a clone without it cannot build made from the same code
         Ok pinned made's result in data.lock
```

## check

`fetchloom build --check` asks whether each step still makes what `data.lock` pins. it runs every step the named recipes need, each from the inputs `data.lock` pins, so a step reads the pinned result of a recipe before it, not what the check made of it. it compares what each makes with the files `data.lock` lists, so it needs no pinned result on the machine, and reports every step, not only the first that differs. from a project with `clean`, which keeps rows of a table, and `emb`, whose step writes random bytes:

```text
      Needs up to 135 B on the disk holding the cache, which has 320.4 GiB free
    Running clean · python3 clean.py <build>/in/iris <build>/out
            kept 2 of 3 rows
    Checked clean · 1 of 1 file matches data.lock
    Running emb · python3 embed.py <build>/in/iris <build>/out
            embedded 3 rows
    Differs emb · 0 of 1 file match data.lock
error: emb differs from data.lock: 0 of 1 file match
  changed vectors.bin
  help: the step made other files from the same inputs, command and code. another version of a program or library it uses can do that, as can the time or an unseeded random number; data.lock and data/emb are as they were. the rebuild is kept for you to compare; for example, `fetchloom status -v` says where its files are. once you choose it, `fetchloom build --accept emb` pins it
```

a check changes neither `data.lock` nor the data folder. when every step matches, it ends with `Ok every step made the files data.lock pins`, exit 0, and the results are in the cache for `sync` to place, even on a fresh clone. a step that differs is kept as [a rebuild that differs](#a-rebuild-that-differs) describes, and the check ends with `build.differs`, exit 60, once every step ran. a step that fails stops the check. a step that reads a recipe whose check made other files, and whose pinned result is not on this machine or a mirror, is `Skipped`, and the error says so.

a check runs nothing when `data.lock` does not pin every dataset the steps need as `data.toml` asks, as when a fetched dataset's entry changed or a recipe changed since it was built: that is `lock.outdated`, exit 1, with the error [when a recipe needs building](../reference/recipes.md#when-a-recipe-needs-building) shows.

`--check` cannot be combined with `--accept` or `--dry-run`. with `--locked` it is the check CI runs, as [CI](../guides/ci.md#recipes) shows.

## prepare

`fetchloom build --prepare` fetches what a build of the named recipes needs and runs nothing, so a build on a machine without the network, such as a cluster's compute node, finds it all in the cache. it fetches what each step that would run reads, from the data folder, a [mirror](../guides/mirrors.md) or the source, and what placing the results needs. a step `data.lock` pins as building it now would make needs only its own result: `Up to date` when the cache holds it, and `Fetched` when the data folder or a mirror gave it. with `--check`, every step counts as one that runs, and nothing is placed. a fetched dataset whose entry changed in `data.toml` is pinned anew and written to `data.lock`, as `fetchloom lock` would, unless `--locked` makes that `lock.outdated`. from a project whose `clean` reads `iris`, with a cache that did not hold it yet:

```text
 Downloaded 1 file · 72 B in 28 ms
   Prepared clean · what it reads is on this machine
         Ok a build of these steps needs only what this machine holds now; nothing ran
```

at a terminal, a last `next:` line names that build, with the recipes named and `--check` or `--locked` as given, as `` next: run `fetchloom --offline build --check made` where the steps should run `` after `fetchloom build --prepare --check made` with a folder mirror. when every step is up to date, or fetched from a mirror, no step would run, and the line says `` next: run `fetchloom --offline build`, which places the results without running a step ``.

`fetchloom --offline build` then runs the step with no request. [HPC](../guides/clusters.md#recipes-in-jobs) shows the two halves on a cluster. `--prepare` cannot be combined with `--accept` or `--dry-run`.

## a rebuild that differs

a step that runs again for a key `data.lock` pins, and makes other files than the ones it pins, stops the build with `build.differs`, exit 60. `data.lock`, `data.lock.d` and the data folder stay as they were, and the error names the files that differ, as [check](#check) shows for `emb`, whose step writes random bytes. look at the program versions and the files before you choose: `--accept` pins these files for everyone who takes the commit.

the files it made stay in the cache as a kept rebuild, which `fetchloom status` lists and `cache gc` keeps, but no build reuses. `fetchloom status -v` names each of its files that differs from `data.lock` with the cache file that holds the rebuild's bytes, so you can compare it with the pinned file in the data folder; `status --json` gives the cache file of both. this run, after a `build --check`, had the cache in its usual place on linux, with the home folder shown as `/home/ada`. its first lines say [what the run spends](../guides/speed-and-memory.md#see-what-a-run-uses), as every `-v` run of `status` does:

```text
$ fetchloom status -v
  Resources default: 8 downloads at once, 4 to one host, 24 local workers, 4.0 GiB for buffers, 2 hash lanes
            buffers: 4.0 GiB at most, less than a quarter of the 166.4 GiB available
       Kept emb · a rebuild that differs from data.lock; `fetchloom build --accept emb` pins it
            changed vectors.bin: /home/ada/.cache/fetchloom/objects/93/d6625459034dd0efdba5d8894e35f9274cc778ef1f93b8a7449b36f8881d4e
         Ok everything matches data.lock by size and time
            6 ms in all
$ cmp data/emb/vectors.bin /home/ada/.cache/fetchloom/objects/93/d6625459034dd0efdba5d8894e35f9274cc778ef1f93b8a7449b36f8881d4e
data/emb/vectors.bin /home/ada/.cache/fetchloom/objects/93/d6625459034dd0efdba5d8894e35f9274cc778ef1f93b8a7449b36f8881d4e differ: byte 1, line 1
```

a cache file is read-only and others may share it, so copy one before you change it. once you have looked at them, `fetchloom build --accept emb` pins them in `data.lock` and places them in the data folder, without running anything:

```text
   Accepted emb · 1 file, 24 B in place of the files data.lock pinned
     Placed data/emb · 1 file linked to the cache
         Ok pinned emb's kept rebuild in data.lock
```

without names, `--accept` pins every kept rebuild. it refuses with exit 2, and changes nothing, when a recipe has no kept rebuild, when the recipe or `data.lock` changed since the rebuild was kept, as after a checkout of another commit, or when the cache no longer holds its files. a recipe made from an accepted one needs building again, which a hint names. a later build that pins other files for the recipe drops its kept rebuild, and so does dropping the recipe from `data.toml`; `cache gc` then frees its files. `--accept` cannot be combined with `--dry-run` or `--locked`.

## dry run

a dry run says what a build would do with each step, in the order a build runs them, and shows the command of each step that would or may run, so you can read it before it runs as you. it reads `data.toml`, `data.lock`, each recipe's code, the cache, the data folder and each folder [mirror](../guides/mirrors.md), and changes nothing: no file is written, no step runs, nothing is downloaded and no web mirror is asked. from a project where `clean` is made from `iris` and `tidy` from `clean`, neither built yet:

```text
      Would run clean · not built yet
            python3 clean.py <build>/in/iris <build>/out
            reads iris and clean.py; writes data/clean
    May run tidy · not built yet; clean is not built yet
            python3 tidy.py <build>/in/clean --out=<build>/out
            reads clean and tidy.py, with SEED=1; writes data/tidy
         Ok dry run, nothing was run or changed
  hint: <build> is a folder a build makes in the cache for each step: the step finds a copy of each dataset it reads in <build>/in, and what it writes in <build>/out becomes its dataset
```

the second line of a step is its command, with each folder in place and a word with spaces quoted. the third line names the datasets and code the step reads, the variables `env` sets for it, and the folder its result goes in. each step has one of five actions:

| action | when, as the line says | the build then |
|---|---|---|
| `Would run` | its step runs: something it is made from changed, a dataset it reads, its `run`, a file of its `code` or its `env`, as `Would run clean · clean.py changed`, and no earlier build left that result; or the result data.lock pins is neither in the cache nor on a mirror, as in a fresh clone, where every step says `its pinned result is not on this machine or a mirror` | `ran` |
| `Would fetch` | data.lock pins its result as building it now would make, and the data folder or a folder mirror holds the files the cache lacks, which the build copies in, as `Would fetch clean · 1 file, 46 B as data.lock pins it, from ../mirror`, or `from data` | `fetched` |
| `Would reuse` | an earlier build of the same key, here or in another project that shares the cache, left a result the cache holds whole | `reused` |
| `May run` | what the build does with it waits on what the build learns first: the result of a step before it that runs, as `May run tidy · runs again if clean's result changes`, a dataset whose entry in `data.toml` changed, which the build pins again first, or whether a web mirror holds its pinned result | any of the others |
| `Up to date` | data.lock pins its result as building it now would make, and the cache holds it | `up_to_date` |

with a mirror holding every result, as `fetchloom --mirror ../mirror build --dry-run` in a fresh clone, nothing runs, and the last line says `Ok no step would run; dry run, nothing was run or changed`. when every step is up to date, it says `Ok every recipe is built as data.lock pins it; dry run, nothing was run or changed`. a `May run` step after a changed recipe runs again only if that recipe makes other files than before, so after a comment was added to clean.py, the build ran clean, which made the same file, and found tidy up to date.

## exit codes

| exit | kind | when |
|---|---|---|
| 0 | | every step is built and placed, a dry run completed, or a ctrl-c or SIGTERM came once the steps ran and the build was writing `data.lock`, as `--accept` and `--prepare` do too: it says `Finishing what it started (ctrl-c again to quit at once)`, or `(a second SIGTERM quits at once)`, pins and places the results and ends with its usual last line |
| 1 | `lock.outdated` | with `--locked` or `--check`, `data.lock` does not pin what the steps need as `data.toml` asks |
| 1 | `unconfirmed` | the build is short of room and cannot ask |
| 2 | `usage` | a name that is not a recipe, or an `--accept` that finds nothing to pin |
| 50 | `local.io` | the disk has less free than `build_floor`, before a step runs or while it does, or code cannot be read |
| 50 | `project.invalid` | code is missing, a link or a special file |
| 60 | `build.failed` | a step cannot start, fails or writes what a result cannot hold |
| 60 | `build.differs` | a step makes other files than `data.lock` pins for its key |
| 130 | `interrupted` | ctrl-c, or SIGTERM on linux and macos, while the steps run |

fetching what a step reads uses the [shared exit codes](../reference/exit-codes.md) for downloads.

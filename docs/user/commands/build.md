# build

`build` runs the [recipes](../recipes.md) in `data.toml`: each step makes a dataset from other datasets, and `build` pins what it made in `data.lock` and places it in the data folder like any dataset. a step is your project's code, so no other command runs one; `--dry-run` shows each command first and runs nothing.

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
| `-n`, `--dry-run` | say what each step would run, what it reads and where its result goes; run nothing |

[global options](README.md#global-options) apply.

## what happens

1. find the project and take its lock, which the build holds until it ends.
2. pin the fetched datasets the steps read, resolving those whose entry changed as `sync` would, without writing `data.lock` yet. with `--locked`, a fetched dataset whose entry changed, a recipe never built or changed since it was built, or a pin building would change, as one of a recipe `data.toml` no longer has, stops the build with `lock.outdated`, exit 1, before anything is fetched or run.
3. say how much room the steps that will run may take on the disk holding the cache, as `Needs up to 91 B on the disk holding the cache, which has 341.3 GiB free`: the files they read that the cache lacks, their results as `data.lock` last pinned them, besides results never built before, which the line names, and, where the disk does not clone files, the copies of what the largest step reads. when that and the `build_floor` [setting](settings.md) are more than the disk has free, it asks first; `--yes` answers yes, and with no one to ask the build stops with `unconfirmed`, exit 1, before anything runs.
4. take each recipe in order, each after the datasets it is made from, ties by name:
   - `Up to date` when `data.lock` pins what building it now would make and the cache holds those files;
   - `Fetched` when `data.lock` pins what building it now would make and the data folder or a [mirror](../mirrors.md) holds the files the cache lacks, which it copies into the cache;
   - `Reused` when an earlier build of the same key, here or in another project that shares the cache, made a result the cache holds whole;
   - otherwise put in the cache the files of the datasets it reads, from the data folder, a mirror or their source when it lacks them, then run its step, as [the step](#the-step) describes, and take in what it wrote. a dataset imported from a folder has no source to fetch from, so on a machine whose cache and data folder lack it the build stops with `reference.unresolved`, exit 10, naming `fetchloom add <folder> --as <name>`.
5. write `data.lock` and `data.lock.d` once, after the last step, with each result's key and the list of its files.
6. place the datasets the build took in the data folder, as `fetchloom sync` with their names would.

this output is from a project where `clean` is made from `iris` with `python3 clean.py`:

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

on a machine whose cache lacks the result `data.lock` pins, as in a fresh clone with no mirror holding it, the build runs the step again. a copy of the project that brought its data folder along takes the result from there instead, as [moving a project](cache.md#moving-a-project-to-another-machine) describes. when the step makes the same files, `data.lock` stays as it is and the last line says so, here after the cache lost clean's result:

```text
      Needs up to 100 B on the disk holding the cache, which has 331.1 GiB free
    Running clean · python3 clean.py <build>/in/iris <build>/out
            kept 2 of 3 rows
      Built clean · 1 file, 43 B in 32 ms
         Ok clean's result matches data.lock
```

when it makes other files, the build fails instead, as [a rebuild that differs](#a-rebuild-that-differs) describes.

## the step

a step runs its `run` words as the program and its arguments, without a shell, so a folder with spaces stays one argument. the program is found as your shell finds it: in the folders of `PATH`, in order, and a name with a folder in it, such as `tools/prep`, from the project folder. on windows each folder is tried with the extensions of `PATHEXT` that windows can start, `.com`, `.exe`, `.bat` and `.cmd`, so `uv`, `python` and a `.cmd` shim such as `npm` all start; [windows](../windows.md#recipe-steps) has the details. fetchloom makes a folder for the step in the cache, which `<build>` stands for in the output:

- `<build>/in/<name>` holds a copy of each dataset it reads, made from the files `data.lock` pins in the cache, never from the data folder, where you may have changed a file. a file the cache took from the data folder came in only once its bytes there hashed to their pin. the copies are reflinks where the disk clones files and plain copies elsewhere, never hardlinks, and read-only, so a step that writes one anyway changes only its own copy.
- `<build>/out` is empty, and what the step writes there becomes the dataset.

the step runs in the project folder, so a script in `run` is the project's own, with nothing on its input and the environment you run fetchloom in, plus the variables of `env` and `FETCHLOOM_BUILDING`, the project folder. a fetchloom the step runs on its own project stops at once with exit 2 rather than wait for the build that holds the project. the step and everything it starts run in a group of their own, a process group on linux and macos and a job object on windows: when the step ends, anything it started and left running is ended too, and ctrl-c ends the whole group. on windows the group also ends when fetchloom does, even when task manager or a cancelled CI job ends it outright. SIGTERM, which a scheduler such as slurm sends a job it ends, stops the build as ctrl-c does and ends the step's group at once; a second one quits as a second ctrl-c does.

while the step runs, fetchloom looks at the free space on the disk holding the cache every two seconds, and stops the step, with all it started, once less is free than the `build_floor` [setting](settings.md), by default the larger of 1 GiB and 1% of that disk. the build then fails with `local.io`, exit 50, and `data.lock` and the data folder stay as they were. this output is from `FETCHLOOM_BUILD_FLOOR=100T fetchloom build --yes clean` on a disk with 329 GiB free, so `--yes` went on past the question and the step stopped at its first look; the line under the error, left out here, says how much was free and where the cache is:

```text
      Needs up to 135 B on the disk holding the cache, which has 329.1 GiB free
    Running clean · python3 clean.py <build>/in/iris <build>/out
error: stopped clean: the disk holding the cache has less than 100.0 TiB free; the step had written 0 B
  help: free space on that disk, or lower build_floor in the settings file or FETCHLOOM_BUILD_FLOOR; data.lock and data/clean are as they were
```

the step's output goes to `.fetchloom/builds/<name>.log`, made anew for each run, which only you may read on linux and macos and which keeps the latest 16 MiB. it also shows under the step, except with `-q` or `--json`.

## what a result may hold

once the step exits with 0, fetchloom takes in what it wrote into `<build>/out`, without following links, and refuses the result with `build.failed` when it holds:

- nothing at all;
- a link, a device, a fifo or a socket, since a result holds plain files only;
- a name some disk fetchloom supports cannot store, such as `a:b.csv` on windows or a name longer than 255 bytes, or one that names repository metadata, such as `.git`;
- two names a disk takes for one, as `Data.csv` and `data.csv` on a disk that ignores case, or the same name with an accent written two ways;
- more files than the `most_files` [setting](settings.md) allows;
- a file that changed while fetchloom read it, as when something the step started still writes.

a step that changed a file of its own `code` while it ran fails too, since its result would answer code that is no longer there. fetchloom hashes each file, then moves it into the cache when it has no other name and is yours, and copies it otherwise and on windows, so a file the step linked from elsewhere, such as one of your own, stays as it was.

## what a failure leaves

a step that fails, is stopped or writes what a result cannot hold leaves `data.lock`, `data.lock.d` and the data folder byte for byte as they were. the error names the exit code and shows the last 20 lines of output:

```text
error: clean failed: python3 exited with code 1 after 31 ms
  kept 2 of 3 rows
  stop
  help: the whole output is in .fetchloom/builds/clean.log, and data.lock and data/clean are as they were
```

the results of the steps that finished before it stay in the cache by their key, so a build run again reuses them. a second build of a key another build is running, as in a second clone of the project, waits for it, says `Waiting for another fetchloom building clean`, and reuses what it made.

a recipe whose key `data.lock` already pins, but whose files the cache lacks, as in a fresh clone, comes from the data folder or a mirror that holds them, and runs again when none does. a step that makes the same files leaves the pin as it is; one that makes other files is not reproducible, as [a rebuild that differs](#a-rebuild-that-differs) describes.

## check

`fetchloom build --check` asks whether each step still makes what `data.lock` pins. it runs every step the named recipes need, each from the inputs `data.lock` pins, a recipe it reads as its pinned result rather than as its own check made it, so each step is checked on its own. it compares what each makes with the files `data.lock` and `data.lock.d` list, so it needs no pinned result on the machine, and reports every step, not only the first that differs. this output is from a project with `clean`, which keeps rows of a table, and `emb`, whose step writes random bytes:

```text
      Needs up to 159 B on the disk holding the cache, which has 329.1 GiB free
    Running clean · python3 clean.py <build>/in/iris <build>/out
            kept 2 of 3 rows
    Checked clean · 1 of 1 files match data.lock
    Running emb · python3 embed.py <build>/in/iris <build>/out
            embedded 3 rows
    Differs emb · 0 of 1 files match data.lock
error: emb differs from data.lock: 0 of 1 files match
  changed vectors.bin
  help: the step is not reproducible, as one that uses the time or an unseeded random number is; data.lock and data/emb are as they were. the rebuild is kept: `fetchloom build --accept emb` pins it
```

a check changes neither `data.lock` nor the data folder. when every step matches, it ends with `Ok every step made the files data.lock pins`, exit 0. the files of a step that matches enter the cache as its pinned result, so a check on a fresh clone leaves the results there for `sync` to place. one that differs is kept as [a rebuild that differs](#a-rebuild-that-differs) describes, and the check ends with `build.differs`, exit 60, once every step ran. a step that fails stops the check, as it stops a build. a step that reads a recipe whose check made other files, and whose pinned result neither the cache, the data folder nor a mirror holds, cannot be checked: it is `Skipped`, and the error says so.

a check compares with `data.lock` as it is, so it runs nothing when `data.lock` does not pin every dataset the steps need as `data.toml` asks: a fetched dataset whose entry changed, or a recipe never built or changed since it was built, is `lock.outdated`, exit 1:

```text
error: data.lock is out of date: clean changed since it was built
  clean: clean.py changed
  help: run `fetchloom build clean`, then commit data.lock and data.lock.d
```

`--check` cannot be combined with `--accept` or `--dry-run`. with `--locked` it is the check CI runs, [CI](../ci.md#recipes) shows.

## prepare

`fetchloom build --prepare` fetches what a build of the named recipes needs and runs nothing, so a build on a machine without the network, such as a cluster's compute node, finds it all in the cache. for each step the build would run, it puts in the cache the files of the datasets the step reads, taking them from the data folder or asking a [mirror](../mirrors.md) before their source as a build does, and the pinned result of each recipe the step reads. a step `data.lock` pins as building it now would make needs only its own result: it is `Up to date` when the cache holds it, and `Fetched` when the data folder or a mirror gave the files the cache lacked. a build places its datasets once its steps are done, so `--prepare` also fetches what placing them needs, such as a dataset read only by steps that will not run. with `--check`, every step counts as one that runs and nothing is placed. a fetched dataset whose entry changed in `data.toml` is pinned anew and written to `data.lock`, as `fetchloom lock` would, so the later build needs no network for it either; with `--locked`, it is `lock.outdated` instead. this output is from a project whose `clean` reads `iris`, with a cache that did not hold it yet:

```text
 Downloaded 1 file · 72 B in 28 ms
   Prepared clean · what it reads is on this machine
         Ok a build of these steps needs only what this machine holds now; nothing ran
```

`fetchloom --offline build` then runs the step with no request. [HPC](../hpc.md#recipes-in-jobs) shows the two halves on a cluster. `--prepare` cannot be combined with `--accept` or `--dry-run`.

## a rebuild that differs

a step that runs again for a key `data.lock` pins, and makes other files than the ones it pins, stops the build with `build.differs`, exit 60. `data.lock`, `data.lock.d` and the data folder stay as they were, and the error says which files differ. this output is from a project where `emb` writes random bytes with `python3 embed.py`, built again after its file left the cache:

```text
      Needs up to 96 B on the disk holding the cache, which has 329.1 GiB free
 Up to date clean
    Running emb · python3 embed.py <build>/in/iris <build>/out
            embedded 3 rows
    Differs emb · 0 of 1 files match data.lock
error: emb differs from data.lock: 0 of 1 files match
  changed vectors.bin
  help: the step is not reproducible, as one that uses the time or an unseeded random number is; data.lock and data/emb are as they were. the rebuild is kept: `fetchloom build --accept emb` pins it
```

the files it made stay in the cache as a kept rebuild, which `fetchloom status` lists and `cache gc` keeps, but no build reuses: only you choose them over the pinned files. once you have looked at them, `fetchloom build --accept emb` pins them in `data.lock` and places them in the data folder, without running anything:

```text
   Accepted emb · 1 file, 24 B in place of the files data.lock pinned
     Placed data/emb · 1 file linked to the cache
         Ok pinned emb's kept rebuild in data.lock
```

without names, `--accept` pins every kept rebuild. it refuses with exit 2, and changes nothing, when a recipe has no kept rebuild, as after accepting it once; when the recipe or `data.lock` changed since it was kept, so it no longer answers them; or when the cache no longer holds its files. a recipe made from an accepted one needs building again, which a hint names. a later build that pins other files for the recipe drops its kept rebuild, and so does a sync or `fetchloom remove` once `data.toml` no longer has the recipe, as they drop its pin; `cache gc` then frees its files. `--accept` cannot be combined with `--dry-run` or `--locked`.

## dry run

steps come in the order a build runs them: each after the datasets it is made from, ties by name. a dry run reads `data.toml`, `data.lock` and each recipe's code, and changes nothing: no file is written, no step runs and nothing is downloaded. this output is from a project where `clean` is made from `iris` and `tidy` from `clean`, neither built yet:

```text
  Would run clean · not built yet
            python3 clean.py <build>/in/iris <build>/out
            reads iris and clean.py; writes data/clean
  Would run tidy · not built yet; clean is not built yet
            python3 tidy.py <build>/in/clean --out=<build>/out
            reads clean, with SEED=1; writes data/tidy
         Ok dry run, nothing was run or changed
  hint: <build> is a folder a build makes in the cache for each step: the step finds a copy of each dataset it reads in <build>/in, and what it writes in <build>/out becomes its dataset
```

the second line of a step is its command, the program and its arguments with each folder in place. no shell reads it, so a word with spaces stays one argument; the dry run quotes such a word. the third line names the datasets and code the step reads, the variables `env` sets for it, and the folder its result goes in.

each step has one of three actions:

| action | when |
|---|---|
| `Would run` | data.lock pins no result for it, or something it is made from moved since: a dataset it reads, its `run`, a file of its `code` or its `env`. the line says what |
| `May run` | data.lock pins it as building it now would make, but it reads the result of a step that would run. it runs again only if that result changes |
| `Up to date` | data.lock pins its result as building it now would make |

when every step is up to date, the last line says so:

```text
 Up to date clean
         Ok every recipe is built as data.lock pins it; dry run, nothing was run or changed
```

## JSON

`--json` returns `result.project`; `result.selected`, the recipes named or every recipe when none was; `result.datasets`, the datasets whose pins changed; `result.lock_changed`; `result.placement`, as [sync](sync.md) reports it; and `result.steps`, one record a step in build order:

| field | value |
|---|---|
| `name` | the recipe |
| `action` | `ran`, `reused`, `fetched`, `up_to_date`, with `--accept` `accepted`, with `--check` `ran` or `skipped`, and with `--prepare` `prepared`, `fetched` or `up_to_date` |
| `key` | the key of its result, `sha256:` and 64 hex digits; with `--prepare`, null while a recipe it reads would run first |
| `tree` | the tree of the files its result holds, `sha256:` and 64 hex digits; for a step that differs, the tree `data.lock` keeps; with `--prepare`, null while `data.lock` pins no result for its key |
| `files`, `bytes` | how many files its result holds, and their size |
| `check` | for a step checked, or one that ran again for a key `data.lock` pins and made other files, `matching`, the count of files that match, and `changed`, `missing` and `extra`, every path of each; else null |
| `candidate` | the tree of the files a step that differs made, kept for `--accept`; else null |
| `log` | `.fetchloom/builds/<name>.log` when it ran, else null |
| `seconds` | how long it ran, else null |

a failed build keeps the steps that finished before it in `result.steps`. a check reports every step it ran there, with `lock_changed` false and `placement` null. with `--dry-run`, `result.dry_run` is `true` and each step record has:

| field | value |
|---|---|
| `name` | the recipe |
| `action` | `would_run`, `may_run` or `up_to_date` |
| `key` | the key of the result building it now would make, `sha256:` and 64 hex digits, or null until data.lock pins every dataset it reads and all its code is there |
| `changes` | what moved since data.lock's result of it was built, in the words of the human output; empty when it is up to date |
| `run` | the program and its arguments, each folder in place, `<build>` standing for the step's folder |
| `env` | the variables set for the step alone |
| `inputs` | each dataset it reads, as `name` and `folder` |
| `code` | the files and folders of code whose bytes enter its key |
| `output` | the folder in the project its result goes in |

the JSON `fix` of a build's error is always null, since building runs the project's code.

## exit codes

0 means every step is built and placed, or for a dry run, that it completed. a name that is not a recipe is a usage error, exit 2, and so is an `--accept` that finds nothing to pin. `--locked` and `--check` fail with `lock.outdated`, exit 1, when `data.lock` does not pin what the steps need as `data.toml` asks, and a build short of room that cannot ask fails with `unconfirmed`, exit 1. a step the `build_floor` setting stopped is `local.io`, exit 50. a step that cannot start, fails or writes what a result cannot hold is `build.failed`, and one that makes other files than `data.lock` pins for its key is `build.differs`, both exit 60. code that is missing, a link or a special file is `project.invalid`, exit 50, and code that cannot be read is `local.io`, exit 50. ctrl-c, or SIGTERM on linux and macos, stops the build with `interrupted`, exit 130. fetching what a step reads uses the [shared exit codes](../reference/exit-codes.md) for downloads.

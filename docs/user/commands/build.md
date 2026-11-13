# build

`build` runs the [recipes](../recipes.md) in `data.toml`: each step makes a dataset from other datasets, and `build` pins what it made in `data.lock` and places it in the data folder like any dataset. a step is your project's code, so no other command runs one, and `--dry-run` shows the commands first. `fetchloom help recipes` shows how to write a recipe.

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

[global options](README.md#global-options) apply.

## what happens

1. find the project and take its lock, which the build holds until it ends.
2. pin the fetched datasets the steps read, resolving those whose entry changed as `sync` would, without writing `data.lock` yet. with `--locked`, a fetched dataset whose entry changed, a recipe never built or changed since it was built, or a pin building would change, as one of a recipe `data.toml` no longer has, stops the build with `lock.outdated`, exit 1, before anything is fetched or run.
3. say how much room the steps that will run may take on the disk holding the cache, as `Needs up to 91 B on the disk holding the cache, which has 341.3 GiB free`: the files they read that the cache lacks, their results as `data.lock` last pinned them, and, where the disk does not clone files, the copies of what the largest step reads. a result never built before is named instead, since its size is unknown. when that and the `build_floor` [setting](settings.md) are more than the disk has free, the build asks first; with no one to ask, it stops with `unconfirmed`, exit 1, unless `--yes` is given. when the disk has less free than `build_floor` already, the build stops with `local.io`, exit 50, `--yes` or not, as [the step](#the-step) shows.
4. take each recipe in order, each after the datasets it is made from, ties by name:
   - `Up to date` when `data.lock` pins what building it now would make and the cache holds those files;
   - `Fetched` when `data.lock` pins what building it now would make and the data folder or a [mirror](../mirrors.md) holds the files the cache lacks, which it copies into the cache;
   - `Reused` when an earlier build of the same key, here or in another project that shares the cache, made a result the cache holds whole;
   - otherwise put in the cache the files of the datasets it reads, from the data folder, a mirror or their source, then run its step, as [the step](#the-step) describes, and take in what it wrote. an [imported](add.md#files-on-this-machine) dataset has no source, so where the cache and the data folder lack it the build stops with `reference.unresolved`, exit 10, naming `fetchloom add <folder> --as <name>`.
5. write `data.lock` and `data.lock.d` once, after the last step, with each result's key and the list of its files.
6. place the datasets the build took in the data folder, as `fetchloom sync` with their names would. a file you edited stays, and the warning names `fetchloom sync --force` with that recipe alone.
7. warn of each file of a recipe's `code` that git does not have, as `fetchloom status` does: a file never added, or one `.gitignore` leaves out, which a clone would lack. a build that ran no step warns too.

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

where neither the cache, the data folder nor a mirror holds the result `data.lock` pins, as in a fresh clone, the build runs the step again; a copy of the project that brought its data folder along takes the result from there, as [moving a project](cache.md#moving-a-project-to-another-machine) describes. when the step makes the same files, `data.lock` stays as it is, here after the cache lost clean's result:

```text
      Needs up to 100 B on the disk holding the cache, which has 331.1 GiB free
    Running clean · python3 clean.py <build>/in/iris <build>/out
            kept 2 of 3 rows
      Built clean · 1 file, 43 B in 32 ms
         Ok clean's result matches data.lock
```

when it makes other files, the build fails instead, as [a rebuild that differs](#a-rebuild-that-differs) describes.

in a git repository, a build whose recipe has code git does not have says so before its last line, here for `made`, whose `step.py` was never added (stderr):

```text
     Placed data/made · 1 file linked to the cache
warning: git does not have step.py, code of made
  help: run `git add step.py`, then commit it, since a clone without it cannot build made from the same code
         Ok pinned made's result in data.lock
```

## the step

a step runs its `run` words as the program and its arguments, without a shell, so a folder with spaces stays one argument. the program is found as your shell finds it: in the folders of `PATH`, or for a name with a folder in it, such as `tools/prep`, from the project folder. on windows, `.com`, `.exe`, `.bat` and `.cmd` programs all start, as [windows](../windows.md#recipe-steps) explains. fetchloom makes a folder for the step in the cache, which `<build>` stands for in the output:

- `<build>/in/<name>` holds a copy of each dataset it reads, made from the files `data.lock` pins in the cache, never from the data folder, where you may have changed a file. the copies are reflinks where the disk clones files and plain copies elsewhere, never hardlinks, and read-only, so a step that writes one anyway changes only its own copy.
- `<build>/out` is empty, and what the step writes there becomes the dataset.

a program that is not there stops the build with `build.failed`, exit 60, and the error says where it looked. a name with a folder in it, as `.venv/bin/python`, is missing from a clone until the project's environment is set up there, so the help asks for that rather than a full path in `run`, which would work on one machine only and change the recipe's key for everyone (stderr):

```text
    Running made · .venv/bin/python step.py <build>/in/raw <build>/out
error: made cannot start: the project folder has no .venv/bin/python
  help: a program named with a folder in it runs from the project folder, not from PATH: set up the project's environment so that .venv/bin/python is there, then run `fetchloom build` again
```

for a bare name, such as `python3.99`, the help says to install it or add its folder to `PATH`.

the step runs in the project folder, so a script in `run` is the project's own, with nothing on its input and the environment you run fetchloom in, plus the variables of `env` and `FETCHLOOM_BUILDING`, the project folder. a fetchloom the step runs on its own project stops at once with exit 2 rather than wait for the build that holds the project. the step and everything it starts run in a group of their own, a process group on linux and macos and a job object on windows: when the step ends, anything it started and left running is ended too, and ctrl-c ends the whole group. on windows the group also ends when fetchloom does, even when task manager or a cancelled CI job ends it outright. SIGTERM, which a scheduler such as slurm sends a job it ends, stops the build as ctrl-c does and ends the step's group at once, with `Stopping (a second SIGTERM quits at once)` in the job's log and a last line that names it, `Stopped by a SIGTERM, so data.lock and the data folder are as they were`; a second one quits as a second ctrl-c does, with one `Quit at once` line.

while the step runs, fetchloom looks at the free space on the disk holding the cache every two seconds, and stops the step, with all it started, once less is free than the `build_floor` [setting](settings.md), by default the larger of 1 GiB and 1% of that disk. the build then fails with `local.io`, exit 50, and `data.lock` and the data folder stay as they were. a disk with less free than the floor before any step runs stops the build there, `--yes` or not. this output is from `FETCHLOOM_BUILD_FLOOR=100T fetchloom build --yes made` on a disk with 321 GiB free; the line under the error, left out here, says where the cache is:

```text
      Needs up to 122 B on the disk holding the cache, which has 321.1 GiB free
error: the disk holding the cache has 321.1 GiB free, less than the 100.0 TiB build_floor keeps free, so no step can run
  help: free space on that disk, or set FETCHLOOM_CACHE to a folder on a bigger one. to let steps leave less free, lower build_floor in the settings file or FETCHLOOM_BUILD_FLOOR
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

a step that changed a file of its own `code` while it ran fails too, since its result would answer code that is no longer there. fetchloom moves each file into the cache, or copies it when it has another name, so a file the step linked from elsewhere, such as one of your own, stays as it was.

## what a failure leaves

a step that fails, is stopped or writes what a result cannot hold leaves `data.lock`, `data.lock.d` and the data folder byte for byte as they were. the step's output shows as it runs, so the error names the exit code and where the whole output is (stderr):

```text
      Needs up to 27 B, besides what clean writes, on the disk holding the cache, which has 323.3 GiB free
    Running clean · python3 clean.py <build>/in/raw <build>/out
            kept 2 of 3 rows
            stop
error: clean failed: python3 exited with code 1 after 32 ms
  help: the whole output is in .fetchloom/builds/clean.log, and data.lock and data/clean are as they were
```

with `-q`, which hides the output as it runs, the error shows its last 20 lines instead, and so does `errors[0].details` with `--json`:

```text
error: clean failed: python3 exited with code 1 after 32 ms
  kept 2 of 3 rows
  stop
  help: the whole output is in .fetchloom/builds/clean.log, and data.lock and data/clean are as they were
```

the results of the steps that finished before it stay in the cache by their key, so a build run again reuses them. a second build of a key another build is running, as in a second clone of the project, says `Waiting for another fetchloom building clean` and reuses what that build made.

code a recipe names that the project lacks, as in a clone of a commit that left it out, stops the build with `project.invalid`, exit 50, before its step runs. the help asks for the code back, since taking it out of `code` would change the recipe's key for everyone (stderr):

```text
error: clean cannot be built: its code clean.py is not in the project
  help: bring clean.py back: `git restore clean.py` if git has it, else ask whoever built clean to commit it; take it out of code in data.toml only once the step no longer reads it
```

## check

`fetchloom build --check` asks whether each step still makes what `data.lock` pins. it runs every step the named recipes need, each from the inputs `data.lock` pins, so a step reads the pinned result of a recipe before it, not what the check made of it. it compares what each makes with the files `data.lock` lists, so it needs no pinned result on the machine, and reports every step, not only the first that differs. this output is from a project with `clean`, which keeps rows of a table, and `emb`, whose step writes random bytes:

```text
      Needs up to 135 B on the disk holding the cache, which has 320.4 GiB free
    Running clean · python3 clean.py <build>/in/iris <build>/out
            kept 2 of 3 rows
    Checked clean · 1 of 1 files match data.lock
    Running emb · python3 embed.py <build>/in/iris <build>/out
            embedded 3 rows
    Differs emb · 0 of 1 files match data.lock
error: emb differs from data.lock: 0 of 1 files match
  changed vectors.bin
  help: the step made other files from the same inputs, command and code. another version of a program or library it uses can do that, as can the time or an unseeded random number; data.lock and data/emb are as they were. the rebuild is kept for you to compare; for example, `fetchloom status -v` says where its files are. once you choose it, `fetchloom build --accept emb` pins it
```

a check changes neither `data.lock` nor the data folder. when every step matches, it ends with `Ok every step made the files data.lock pins`, exit 0, and the results are in the cache for `sync` to place, even on a fresh clone. a step that differs is kept as [a rebuild that differs](#a-rebuild-that-differs) describes, and the check ends with `build.differs`, exit 60, once every step ran. a step that fails stops the check. a step that reads a recipe whose check made other files, and whose pinned result is not on this machine or a mirror, is `Skipped`, and the error says so.

a check runs nothing when `data.lock` does not pin every dataset the steps need as `data.toml` asks, as when a fetched dataset's entry changed or a recipe changed since it was built: that is `lock.outdated`, exit 1:

```text
error: data.lock is out of date: clean changed since it was built
  clean: clean.py changed
  help: run `fetchloom build clean`, then commit data.lock and data.lock.d
```

`--check` cannot be combined with `--accept` or `--dry-run`. with `--locked` it is the check CI runs, [CI](../ci.md#recipes) shows.

## prepare

`fetchloom build --prepare` fetches what a build of the named recipes needs and runs nothing, so a build on a machine without the network, such as a cluster's compute node, finds it all in the cache. it fetches what each step that would run reads, from the data folder, a [mirror](../mirrors.md) or the source, and what placing the results needs. a step `data.lock` pins as building it now would make needs only its own result: `Up to date` when the cache holds it, and `Fetched` when the data folder or a mirror gave it. with `--check`, every step counts as one that runs, and nothing is placed. a fetched dataset whose entry changed in `data.toml` is pinned anew and written to `data.lock`, as `fetchloom lock` would, unless `--locked` makes that `lock.outdated`. this output is from a project whose `clean` reads `iris`, with a cache that did not hold it yet:

```text
 Downloaded 1 file · 72 B in 28 ms
   Prepared clean · what it reads is on this machine
         Ok a build of these steps needs only what this machine holds now; nothing ran
```

at a terminal, the last line names that build, with the recipes named and `--check` or `--locked` as given. here it is the end of `fetchloom build --prepare --check made` with a folder mirror:

```text
   Prepared made · what it reads is on this machine
         Ok a build of these steps needs only what this machine holds now; nothing ran
  next: run `fetchloom --offline build --check made` where the steps should run
```

when every step is up to date, or fetched from a mirror, no step would run, and the line says so:

```text
 Up to date made
         Ok a build of these steps needs only what this machine holds now; nothing ran
  next: run `fetchloom --offline build`, which places the results without running a step
```

`fetchloom --offline build` then runs the step with no request. [HPC](../hpc.md#recipes-in-jobs) shows the two halves on a cluster. `--prepare` cannot be combined with `--accept` or `--dry-run`.

## a rebuild that differs

a step that runs again for a key `data.lock` pins, and makes other files than the ones it pins, stops the build with `build.differs`, exit 60. `data.lock`, `data.lock.d` and the data folder stay as they were, and the error says which files differ. this output is from a project where `emb` writes random bytes with `python3 embed.py`, built again after its file left the cache:

```text
      Needs up to 85 B on the disk holding the cache, which has 320.4 GiB free
 Up to date clean
    Running emb · python3 embed.py <build>/in/iris <build>/out
            embedded 3 rows
    Differs emb · 0 of 1 files match data.lock
error: emb differs from data.lock: 0 of 1 files match
  changed vectors.bin
  help: the step made other files from the same inputs, command and code. another version of a program or library it uses can do that, as can the time or an unseeded random number; data.lock and data/emb are as they were. the rebuild is kept for you to compare; for example, `fetchloom status -v` says where its files are. once you choose it, `fetchloom build --accept emb` pins it
```

look at the program versions and the files before you choose: `--accept` pins these files for everyone who takes the commit.

the files it made stay in the cache as a kept rebuild, which `fetchloom status` lists and `cache gc` keeps, but no build reuses. `fetchloom status -v` names each of its files that differs from `data.lock` with the cache file that holds the rebuild's bytes, so you can compare it with the pinned file in the data folder; `status --json` gives the cache file of both. this run, after a `build --check`, had the cache in its usual place on linux, with the home folder shown as `/home/ada`. its first lines say [what the run spends](../resources.md#see-what-a-run-uses), as every `-v` run of `status` does:

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

a dry run says what a build would do with each step, in the order a build runs them, and shows the command of each step that would or may run, so you can read it before it runs as you. it reads `data.toml`, `data.lock`, each recipe's code, the cache, the data folder and each folder [mirror](../mirrors.md), and changes nothing: no file is written, no step runs, nothing is downloaded and no web mirror is asked. this output is from a project where `clean` is made from `iris` and `tidy` from `clean`, neither built yet:

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

the second line of a step is its command, with each folder in place and a word with spaces quoted. the third line names the datasets and code the step reads, the variables `env` sets for it, and the folder its result goes in.

each step has one of five actions:

| action | when | the build then |
|---|---|---|
| `Would run` | its step runs: something it is made from changed, a dataset it reads, its `run`, a file of its `code` or its `env`, and no earlier build left that result; or the result data.lock pins is neither in the cache nor on a mirror, as in a fresh clone. the line says which | `ran` |
| `Would fetch` | data.lock pins its result as building it now would make, and the data folder or a folder mirror holds the files the cache lacks, which the build copies in. the line says where from | `fetched` |
| `Would reuse` | an earlier build of the same key, here or in another project that shares the cache, left a result the cache holds whole | `reused` |
| `May run` | what the build does with it waits on what the build learns first: the result of a step before it that runs, a dataset whose entry in `data.toml` changed, which the build pins again first, or whether a web mirror holds its pinned result. the line says what | any of the others |
| `Up to date` | data.lock pins its result as building it now would make, and the cache holds it | `up_to_date` |

in a fresh clone, whose cache holds none of the results data.lock pins, every step runs, and the dry run says so:

```text
      Would run clean · its pinned result is not on this machine or a mirror
            python3 clean.py <build>/in/iris <build>/out
            reads iris and clean.py; writes data/clean
      Would run tidy · its pinned result is not on this machine or a mirror
            python3 tidy.py <build>/in/clean --out=<build>/out
            reads clean and tidy.py, with SEED=1; writes data/tidy
         Ok dry run, nothing was run or changed
  hint: <build> is a folder a build makes in the cache for each step: the step finds a copy of each dataset it reads in <build>/in, and what it writes in <build>/out becomes its dataset
```

with a folder mirror that holds the results, as `fetchloom --mirror ../mirror build --dry-run` there, nothing runs. a cache that lost results the data folder still holds says `from data` instead, as the build copies them from there:

```text
      Would fetch clean · 1 file, 46 B as data.lock pins it, from ../mirror
      Would fetch tidy · 1 file, 46 B as data.lock pins it, from ../mirror
         Ok no step would run; dry run, nothing was run or changed
```

a step after one that runs for a changed recipe may run. here a comment was added to clean.py, so tidy runs again only if clean makes other files than before; the build that followed ran clean, which made the same file, and found tidy up to date:

```text
      Would run clean · clean.py changed
            python3 clean.py <build>/in/iris <build>/out
            reads iris and clean.py; writes data/clean
    May run tidy · runs again if clean's result changes
            python3 tidy.py <build>/in/clean --out=<build>/out
            reads clean and tidy.py, with SEED=1; writes data/tidy
         Ok dry run, nothing was run or changed
  hint: <build> is a folder a build makes in the cache for each step: the step finds a copy of each dataset it reads in <build>/in, and what it writes in <build>/out becomes its dataset
```

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
| `action` | `would_run`, `would_fetch`, `would_reuse`, `may_run` or `up_to_date`; each but `may_run` is the action the build after it reports: `ran`, `fetched`, `reused` or `up_to_date` |
| `key` | the key of the result building it now would make, `sha256:` and 64 hex digits, or null until data.lock pins every dataset it reads and all its code is there, and while it waits on a step before it |
| `changes` | why it would or may run, in the words of the human output: what moved since data.lock's result of it was built, or that its pinned result is not on this machine or a mirror; empty when it is up to date |
| `run` | the program and its arguments, each folder in place, `<build>` standing for the step's folder |
| `env` | the variables set for the step alone |
| `inputs` | each dataset it reads, as `name` and `folder` |
| `code` | the files and folders of code whose bytes enter its key |
| `output` | the folder in the project its result goes in |

the JSON `fix` of a build's error is always null, since building runs the project's code.

## exit codes

0 means every step is built and placed, or for a dry run, that it completed. a name that is not a recipe is a usage error, exit 2, and so is an `--accept` that finds nothing to pin. `--locked` and `--check` fail with `lock.outdated`, exit 1, when `data.lock` does not pin what the steps need as `data.toml` asks, and a build short of room that cannot ask fails with `unconfirmed`, exit 1. a disk with less free than the `build_floor` setting, before a step runs or while it does, is `local.io`, exit 50. a step that cannot start, fails or writes what a result cannot hold is `build.failed`, and one that makes other files than `data.lock` pins for its key is `build.differs`, both exit 60. code that is missing, a link or a special file is `project.invalid`, exit 50, and code that cannot be read is `local.io`, exit 50. ctrl-c, or SIGTERM on linux and macos, stops the build with `interrupted`, exit 130, while its steps run. once they ran and the build starts writing `data.lock`, as `--accept` and `--prepare` do too, it no longer stops: it says `Finishing what it started (ctrl-c again to quit at once)`, or `(a second SIGTERM quits at once)` after a SIGTERM, pins and places the results, and ends with its usual last line, exit 0. fetching what a step reads uses the [shared exit codes](../reference/exit-codes.md) for downloads.

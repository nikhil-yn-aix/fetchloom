# Recipes

a recipe makes a dataset from others with a command of yours; [recipes](../guides/recipes.md) says how fetchloom keys and checks them, and data.toml takes these rules.

```toml
[datasets.clean]
from = ["iris"]
run = ["python3", "clean.py", "{iris}", "{out}"]
code = ["clean.py", "lib/"]
env = { SOURCE_DATE_EPOCH = "315532800" }
```

| key | meaning | rules |
|---|---|---|
| `from` | the datasets it is made from, by name | required; a list of names data.toml has, each once, not empty. `out` cannot be one, since `{out}` names the result's folder. recipes cannot be made from each other, directly or through others |
| `run` | the program and its arguments | required; a list of words, since no shell reads it. `{name}` stands for the folder of a dataset in `from`, `{out}` for the result's folder and `{{` and `}}` for one brace, anywhere inside a word. it names every dataset in `from` and `{out}`, and no other name. a brace that is not part of these, a control character or an empty program is refused |
| `code` | project files and folders whose bytes enter the key | optional list. each path uses `/`, may end in `/`, and stays inside the project: no absolute path, drive, backslash, `.` or `..`, no repository metadata such as `.git`, not `data.lock` or `data.lock.d`, and neither inside the data folder nor holding it. a link or a special file on the way is refused when the code is read |
| `env` | variables set for the step alone | optional table of strings. names use letters, digits and `_` and do not start with a digit. names starting with `FETCHLOOM_` are refused, and so are names that look like a secret: a word ending in `TOKEN`, a word holding `SECRET`, `PASSWORD`, `PASSWD` or `CREDENTIAL`, or the words `API_KEY`, `ACCESS_KEY` or `PRIVATE_KEY`, ignoring case. `MAX_TOKENS` and `TOKENIZERS_PARALLELISM` stay usable. a value cannot hold a NUL character |

a recipe takes `copy`, `path` and `place` as a fetched dataset does. `ref`, `select`, `exclude` and `extract` belong to fetched datasets: beside `from` they are refused, and choosing files belongs inside the step. every mistake is `project.invalid`, exit 50, pointing at its line:

for a `clean` whose `run`, on line 7 of data.toml, leaves out `{out}`:

```text
error: run in datasets.clean never names {out}, so the step has nowhere to write
  data.toml:7   run = ["python3", "clean.py", "{iris}"]
  help: put {out} where the step takes the folder to write the result in
```

a message about `env` cites its line by number only, as one about `ref` does, since either may hold a secret.

## what fetchloom keys

a recipe's result is named by a key: the SHA-256 of what it is made from. the key holds

- the content of each dataset in `from`, as `data.lock` pins it: each file's path, size and SHA-256, and for an archive, the files it unpacks to, less those a [members table](data-toml.md#files-inside-archives) leaves out. links, versions and when a dataset was pinned stay out, so a source that moves or zips again the same files keeps the key. a recipe made from another recipe takes that recipe's result, so a rebuild that gives the same files changes nothing after it.
- `run` as written, before any folder is put in.
- the bytes of each file of `code`, a folder as the tree of its files. a text file counts with each CRLF made LF, so a linux and a windows clone get one key. a folder leaves out `.git`, `.hg`, `.svn`, `__pycache__`, `.ipynb_checkpoints`, `.DS_Store` and `Thumbs.db`, and a link or a special file anywhere in `code` is refused.
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

`fetchloom lock --check`, `fetchloom sync --locked` and a [`fetchloom push`](../commands/push.md) of the recipe fail with `build.needed`, exit 60, naming what moved:

```text
error: data.lock is out of date: clean changed since it was built
  clean: clean.py changed
  help: run `fetchloom build clean`, then commit data.lock and data.lock.d
```

a fetched dataset that is out of date fails first with `lock.outdated`, exit 1, since `fetchloom lock` must pin it before a build can read it. the JSON `fix` of `build.needed` is always null: building runs the project's code, so a script or an agent asks a person first.

no command but `build` runs a step. `sync` places a pinned result like any dataset, also after its recipe changed, and warns what changed; a result no cache or [mirror](../guides/mirrors.md) here holds, as in a fresh clone, is left out with a warning naming the build, as [sync](../commands/sync.md#recipe-results) shows. `status` lists it under `unbuilt`, `fetchloom path` answers a built recipe's folder and fails with `build.needed` for one never built, and a dataset a recipe is made from cannot be removed while the recipe reads it. `cache gc` keeps the files of every result a project's `data.lock` pins.

`fetchloom push --inputs <recipe>` copies a recipe's pinned result and every dataset it is made from into a folder [mirror](../guides/mirrors.md), so a reviewer whose sources are gone can rebuild it from there.

## the step

a step runs its `run` words as the program and its arguments, without a shell, so a folder with spaces stays one argument. the program is found as your shell finds it: in the folders of `PATH`, or for a name with a folder in it, such as `tools/prep`, from the project folder. on windows, `.com`, `.exe`, `.bat` and `.cmd` programs all start, as [windows](../guides/windows.md#recipe-steps) explains. fetchloom makes a folder for the step in the cache, which `<build>` stands for in the output:

- `<build>/in/<name>` holds a copy of each dataset it reads, made from the files `data.lock` pins in the cache, never from the data folder, where you may have changed a file. the copies are reflinks where the disk clones files and plain copies elsewhere, never hardlinks, and read-only, so a step that writes one anyway changes only its own copy.
- `<build>/out` is empty, and what the step writes there becomes the dataset.

a program that is not there stops the build with `build.failed`, exit 60, and the error says where it looked. a name with a folder in it, as `.venv/bin/python`, is missing from a clone until the project's environment is set up there, so the help asks for that rather than a full path in `run`, which would work on one machine only and change the recipe's key for everyone (stderr):

```text
    Running made · .venv/bin/python step.py <build>/in/raw <build>/out
error: made cannot start: the project folder has no .venv/bin/python
  help: a program named with a folder in it runs from the project folder, not from PATH: set up the project's environment so that .venv/bin/python is there, then run `fetchloom build` again
```

for a bare name, such as `python3.99`, the help says to install it or add its folder to `PATH`.

the step runs in the project folder, so a script `run` names is the project's own, with nothing on its input and the environment you run fetchloom in, plus the variables of `env` and `FETCHLOOM_BUILDING`, the project folder. a fetchloom the step runs on its own project stops at once with exit 2 rather than wait for the build that holds the project. when the step ends, anything it started and left running is ended too, and ctrl-c ends them all; on windows they also end when fetchloom does, even when task manager or a cancelled CI job ends it outright. SIGTERM, which a scheduler such as slurm sends a job it ends, stops the build as ctrl-c does, ends the step and all it started at once, and ends with `Stopped by a SIGTERM, so data.lock and the data folder are as they were`; [interruptions](../concepts/questions-and-interruptions.md#interruptions) has the rest.

while the step runs, fetchloom watches the free space on the disk holding the cache and stops the step, with all it started, once less is free than the `build_floor` [setting](../commands/settings.md), by default the larger of 1 GiB and 1% of that disk. the build then fails with `local.io`, exit 50, and `data.lock` and the data folder stay as they were. from `FETCHLOOM_BUILD_FLOOR=100T fetchloom build --yes made` on a disk with 321 GiB free, which stops before any step runs, without the line under the error that says where the cache is:

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
- more files than the `most_files` [setting](../commands/settings.md) allows;
- a file that changed while fetchloom read it, as when something the step started still writes.

a step that changed a file of its own `code` while it ran fails too, since its result would answer code that is no longer there. a file the step linked into `<build>/out` from elsewhere, such as one of your own, is copied into the cache and stays as it was.

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

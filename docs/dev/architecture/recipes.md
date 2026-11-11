# recipes

a recipe makes a dataset from other datasets with a command a person writes in `data.toml`. this page covers what the code reads, how a result is keyed, how a stale pin is found, and how `fetchloom build` runs a step, takes in what it wrote, pins it and places it.

## where it lives

| file | owns |
|---|---|
| [project/manifest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/manifest.rs) | `Entry.origin`: `Origin::Source(Source)` with `ref`, `select`, `exclude` and `extract`, or `Origin::Derived(Recipe)` with `from`, `run`, `code` and `env`; every check of those keys |
| [recipe.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/recipe.rs) | `Template` parse and expand, `order` and `closure`, `content`, `key`, `Step` and `drift` |
| [recipe/code.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/recipe/code.rs) | hashing a recipe's code: the CRLF rule, folder trees and the refusal of links |
| [project/lock/derived.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/derived.rs) | `Derived`, a result as `data.lock` pins it, with `Input` and `Code` |
| [cli/build.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/build.rs) | the command: selection, the order of steps, what each does or would do, the commit and the placing |
| [build.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/build.rs) | `Staging`, the folder a step runs in |
| [build/step.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/build/step.rs) | running a step: the process and its group, the log, the tail, a stop |
| [build/inputs.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/build/inputs.rs) | the private copy of each dataset a step reads |
| [build/capture.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/build/capture.rs) | taking in what a step wrote: the walk, the names, the hashes, the publication |
| [cache/results.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cache/results.rs) | `builds/<key>/<tree>` records, key locks, and moving a captured file into `objects/` |
| [examples/recipe_step.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/examples/recipe_step.rs) | the step program the cli tests put in recipes |

a dataset is fetched or made, so the manifest hands every caller an `Origin` to match on. code that only makes sense for a source, such as resolving a reference, drafting a pin or the spec hash, takes a `Source`, so the compiler finds each place a recipe must be left out. relocking asks nothing for a recipe, keeps its `Derived` pin while `data.toml` has the recipe and drops it with the recipe.

## reading data.toml

`Reader::recipe` refuses a recipe with `ref`, `select`, `exclude` or `extract`, one without `from` or `run`, and a `run` that is not a list of words: no shell ever reads it. each word goes through `Template::parse`, which splits it into text and `{name}` folders, reading `{{` and `}}` as braces of their own; the reader then checks that the words name every input and `{out}`, and nothing else. once every dataset is read, `recipe::order` runs Kahn's algorithm with a name-ordered queue over the `from` edges; a cycle comes back as its path, `a → b → a`, for the error. `code` paths get the same syntax checks as `path`, plus the files fetchloom writes, and a check against `data_dir` once it is known. `env` names must be portable and neither fetchloom's own nor named like a secret, and a message about `env` cites its line by number only, as one about `ref` does.

## the key

a result is named by its key, the SHA-256 of one line of JSON:

```text
{"scheme":"fetchloom-recipe-1","from":[[name, content], ...],"run":[word, ...],"code":[[path, "sha256:…" or "tree:sha256:…"], ...],"env":[[name, value], ...]}
```

`from` is sorted by name, `code` by path and `env` by name; `run` keeps the words as written. strings are escaped as `serde_json` and python's `json.dumps(obj, separators=(",", ":"), ensure_ascii=False)` escape them, and `a_known_key_matches_python` pins one key to the hash python gives, so a change to the scheme fails a test before it changes anyone's keys. a change to what enters a key, or how, takes a new scheme name.

`content` of a fetched input is the SHA-256 of one line a pinned file, sorted by path: `path \t size \t sha256`, or `path \t sha256:<tree>` for an archive that unpacks, whose own size and hash stay out, since the step finds its files and not it, so a source that packs the same files again keeps every key. the lines decide which files the step finds and where, since the layout of unpacked members follows from them, and they leave out links, MD5s, versions and times. the dataset's members tables, which choose the files inside its archives, add a line for each pattern after the files, `member \t <archive path> \t select or exclude \t <pattern>`, sorted by archive, then select before exclude, then pattern, each once, so a dataset without them keeps its content and how the patterns are written does not change it. `content` of a recipe's input is that recipe's pinned `tree`, so an upstream rebuild that gives the same files keeps every key after it, the early cutoff of build systems à la carte.

code is hashed by `code::hash`, which walks the path from the project root with `symlink_metadata` and refuses a link or a special file anywhere on the way. a file with no NUL in its first 8,000 bytes, git's text test, is hashed with each CRLF made LF while it streams, a CR held back until the next byte shows whether an LF follows; any other file is hashed as it is. a folder is the tree hash of its files, in the encoding of the cache's records of unpacked archives, with NFC paths and normalized sizes, leaving out version control folders, `__pycache__`, `.ipynb_checkpoints`, `.DS_Store` and `Thumbs.db`.

## finding a stale pin

`Step::read` takes a recipe as it stands: each input's pinned content, `run` as written, each code path's hash on disk and `env`. `Step::changes` compares it with the `Derived` pin and names what moved: no pin (`Unbuilt`), an input pinned to other content or not pinned yet, `run`, a code file changed or missing, `env`, or a key that disagrees though everything it records agrees. nothing moved means data.lock pins what building the recipe now would make.

`lock --check` and `sync --locked` turn changes into `build.needed`, exit 60, after any `lock.outdated` finding, since a build reads what `fetchloom lock` pins. `status` lists the recipe under `unlocked` with what moved. the JSON `fix` of a build is always null: `output::fix` skips any command `cli::runs_code` says would run the project's code, so no script or agent runs a build unasked.

## trust

`data.toml` comes with whatever repository a person clones, so a recipe's command is untrusted until that person builds it. only `cli/build.rs` reaches `build::step::run`, and `no_other_command_runs_a_step` keeps every other command from running one: it gives a recipe a step that would leave a file behind and runs each command against it. `build --dry-run` shows the command before anything runs it, with `<build>` standing for the private folder a build makes in the cache. there is no sandbox: a step runs as the person who builds, with their files and keys.

## deciding a step

a build takes the project's hold for its whole run, relocks the sources of the closure of the named recipes, `Scope::Only` of it, as a sync of them would, and keeps the new lock in memory. each recipe then, in closure order, gets its key from `Step::read` against that lock, so a result built earlier in the same run counts as its input's content:

| data.lock pins | the cache holds | action |
|---|---|---|
| this key | every file of its pin | `Up to date` |
| another key or none | a whole record of this key, on unix written by this user and writable by no one else | `Reused`: the newest record is pinned |
| this key | not every file | run; the same tree keeps the pin, another is `build.differs` and nothing enters the cache |
| another key or none | no whole record | run, and pin what it made |

before a run or a reuse the build takes `Cache::hold_key`, an OS lock on `builds/<key>/lock`, and looks for a record again under it, so a second build of one key, in another clone sharing the cache, waits with `Waiting for another fetchloom building <name>` and reuses what the first made. a disk without locks lets them overlap, which costs only the second run.

## the step

`Loop::ready` first puts in the cache every file of the fetched datasets the step reads, whatever the data folder holds: mirrors first, then sources, an archive unpacked again when a file it holds is missing. `Staging` makes `tmp/<16 hex>.build` in the cache, 0700 on unix, and holds its `lock` as an archive's staging does, so `Cache::sweep_staging` clears a folder a killed build left once that lock is free, and dropping it clears the folder. `inputs::make` places each dataset's files in `in/<name>` with `Placer::without_hardlinks`: reflinks where the disk clones, else copies, read-only, never flushed, since they are scratch. a fetched dataset's files are laid out by `plan::expand`, archives by what they unpacked to; a result's are the members of its pin.

`step::run` starts the words of `run`, each folder in place, with no shell, in the project folder, stdin null, the person's environment plus `env` and `FETCHLOOM_BUILDING`. a fetchloom inside the step that would take that project's hold fails at once in `hold_until` with exit 2, since the build holds it. both pipes are read on threads into `.fetchloom/builds/<name>.log`, made anew with mode 0600 on unix, which drops its older half when it would pass 16 MiB, and into a 64 KiB tail whose last 20 lines go into a failure's details. the thread that watches the step hands each line to the terminal, dim, unless `--json` asks for none, and looks at the child often at first, then every 200 ms, ending it when a stop is asked.

on unix the step runs in a process group of its own, `process_group(0)`, so the terminal's ctrl-c reaches only fetchloom. the build ends the whole group with SIGKILL when a stop is asked and once the step exits, so nothing it left running writes while its result is taken in. a second ctrl-c calls `build::step::end_running`, which ends the group of every step still running, before fetchloom exits with 130. a process that calls `setsid` leaves the group and is not ended; its open output is waited for 2 s at most. windows ends the step's own process with `Child::kill`, and the console's ctrl-c reaches the step as it reaches fetchloom; the `group` module is the seam where a job object, through process-wrap, will end everything a step started.

## taking in a result

once the step exits with 0, the build hashes its code again and fails when a file changed, since the result would answer code that is no longer there. `capture::read` walks `out/` with each entry's own type, never following a link: a link, a device, a fifo or a socket is refused by its path, and so is a name that is not UTF-8. each path is made NFC, the form `data.lock` records, and refused when `name::escapes`, `name::metadata` or `name::unportable` refuses it; `name::unportable` applies windows' rules and the 255-byte name limit on every system, so a result made on linux places on windows. `name::fold` keys find two names one disk takes for one, by case or composition, and a file where another needs a folder. `most_files` bounds the count. each file is hashed on the placement threads, with its size, time, inode and ctime looked at before and after, and a file that moved is refused.

`capture::publish` then follows the cache's flush rule: up to 64 files are flushed one by one, more with one `Batch` flush where the kernel reports write-back errors. a file with one name that the user owns is sealed where it is, made read-only and dated by its birth, then hardlinked or renamed into `objects/` by the cache's own publication, and a new object must be the very inode that was hashed. any other file, and every file on windows, is copied into the cache with `Cache::copy_in`, which checks the bytes against the hash, so a step that linked a person's file into its result never makes it the cache's. the result's record, `builds/<key>/<tree>`, is written last, durably, in the tree encoding of the records of unpacked archives.

## committing

the build writes `data.lock` and its parts once, after its last step, with `Loop::write_rendered`, only when the lock it made differs from the one it read; a pin keeps its `locked` time while its key and tree stay. a step that fails, is stopped or writes what a result cannot hold returns before that, so `data.lock`, `data.lock.d` and the data folder stay byte for byte as they were, while the results of the steps before it stay in the cache by key and a build run again reuses them. the errors are `build.failed`, with the exit code or signal and the tail, and `build.differs`, both exit 60, and the JSON `result` of the steps that finished stays beside them.

## placing results

`plan` places a pinned result like a fetched dataset: its targets hold `Holder::Result` instead of a `LockedFile`, so `Placing::missing` never makes a download of one, while `Placing::wanted` asks the mirrors for its objects as for any file. after the mirrors, `Loop::settle_unbuilt` takes the recipes whose results the placing needs bytes of that the cache lacks and no fetched file of the same run brings. a run of every dataset leaves them out with a warning: `Placing::leave_out` drops their targets that need bytes, so their files already in place stay and what an older pin placed at the others retires as an unwanted file, and the run records an incomplete sync and lists them in `Placement::unbuilt`. a run that names them, or `sync --locked` through `Loop::stopping_on_unbuilt`, refuses with `build.needed` before anything is placed. a dry run does the same when no mirror is set. the build places its closure with `sync_all` once `data.lock` is written, `path` answers a pinned result's folder as for any dataset, `export` lists its files, and `cache gc` counts every pinned result's files as needed.

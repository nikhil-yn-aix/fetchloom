# recipes

a recipe makes a dataset from other datasets with a command a person writes in `data.toml`. this page covers what the code reads, how a result is keyed and how a stale pin is found; running a step, capturing its result and placing it belong to `fetchloom build`, which so far only previews.

## where it lives

| file | owns |
|---|---|
| [project/manifest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/manifest.rs) | `Entry.origin`: `Origin::Source(Source)` with `ref`, `select`, `exclude` and `extract`, or `Origin::Derived(Recipe)` with `from`, `run`, `code` and `env`; every check of those keys |
| [recipe.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/recipe.rs) | `Template` parse and expand, `order` and `closure`, `content`, `key`, `Step` and `drift` |
| [recipe/code.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/recipe/code.rs) | hashing a recipe's code: the CRLF rule, folder trees and the refusal of links |
| [project/lock/derived.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/derived.rs) | `Derived`, a result as `data.lock` pins it, with `Input` and `Code` |
| [cli/build.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/build.rs) | `build --dry-run`: selection, the order of steps, what each would do |

a dataset is fetched or made, so the manifest hands every caller an `Origin` to match on. code that only makes sense for a source, such as resolving a reference, drafting a pin or the spec hash, takes a `Source`, so the compiler finds each place a recipe must be left out. relocking asks nothing for a recipe, keeps its `Derived` pin while `data.toml` has the recipe and drops it with the recipe, and `plan` builds targets from fetched datasets alone, so no command downloads anything for a result.

## reading data.toml

`Reader::recipe` refuses a recipe with `ref`, `select`, `exclude` or `extract`, one without `from` or `run`, and a `run` that is not a list of words: no shell ever reads it. each word goes through `Template::parse`, which splits it into text and `{name}` folders, reading `{{` and `}}` as braces of their own; the reader then checks that the words name every input and `{out}`, and nothing else. once every dataset is read, `recipe::order` runs Kahn's algorithm with a name-ordered queue over the `from` edges; a cycle comes back as its path, `a → b → a`, for the error. `code` paths get the same syntax checks as `path`, plus the files fetchloom writes, and a check against `data_dir` once it is known. `env` names must be portable and neither fetchloom's own nor named like a secret, and a message about `env` cites its line by number only, as one about `ref` does.

## the key

a result is named by its key, the SHA-256 of one line of JSON:

```text
{"scheme":"fetchloom-recipe-1","from":[[name, content], ...],"run":[word, ...],"code":[[path, "sha256:…" or "tree:sha256:…"], ...],"env":[[name, value], ...]}
```

`from` is sorted by name, `code` by path and `env` by name; `run` keeps the words as written. strings are escaped as `serde_json` and python's `json.dumps(obj, separators=(",", ":"), ensure_ascii=False)` escape them, and `a_known_key_matches_python` pins one key to the hash python gives, so a change to the scheme fails a test before it changes anyone's keys. a change to what enters a key, or how, takes a new scheme name.

`content` of a fetched input is the SHA-256 of one line a pinned file, `path \t size \t sha256`, with `\t sha256:<tree>` for an archive that unpacks, sorted by path. the lines decide which files the step finds and where, since the layout of unpacked members follows from them, and they leave out links, MD5s, versions and times. member filters, once `data.toml` has them, add lines of their own after the files, so a dataset without one keeps its content. `content` of a recipe's input is that recipe's pinned `tree`, so an upstream rebuild that gives the same files keeps every key after it, the early cutoff of build systems à la carte.

code is hashed by `code::hash`, which walks the path from the project root with `symlink_metadata` and refuses a link or a special file anywhere on the way. a file with no NUL in its first 8,000 bytes, git's text test, is hashed with each CRLF made LF while it streams, a CR held back until the next byte shows whether an LF follows; any other file is hashed as it is. a folder is the tree hash of its files, in the encoding of the cache's records of unpacked archives, with NFC paths and normalized sizes, leaving out version control folders, `__pycache__`, `.ipynb_checkpoints`, `.DS_Store` and `Thumbs.db`.

## finding a stale pin

`Step::read` takes a recipe as it stands: each input's pinned content, `run` as written, each code path's hash on disk and `env`. `Step::changes` compares it with the `Derived` pin and names what moved: no pin (`Unbuilt`), an input pinned to other content or not pinned yet, `run`, a code file changed or missing, `env`, or a key that disagrees though everything it records agrees. nothing moved means data.lock pins what building the recipe now would make.

`lock --check` and `sync --locked` turn changes into `build.needed`, exit 60, after any `lock.outdated` finding, since a build reads what `fetchloom lock` pins. `status` lists the recipe under `unlocked` with what moved. the JSON `fix` of a build is always null: `output::fix` skips any command `cli::runs_code` says would run the project's code, so no script or agent runs a build unasked.

## trust

`data.toml` comes with whatever repository a person clones, so a recipe's command is untrusted until that person builds it. no command runs a step yet, and `no_other_command_runs_a_step` keeps every command but `build` from ever running one: it gives a recipe a step that would leave a file behind and runs each command against it. `build --dry-run` shows the command before anything could run it, with `<build>` standing for the private folder a build makes in the cache.

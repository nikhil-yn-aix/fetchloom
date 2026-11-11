# build

`build` makes datasets from other datasets with the [recipes](../recipes.md) in `data.toml`. this version shows what a build would run and runs nothing: `--dry-run` is the only way to use it, and running steps comes in the next version.

```sh
fetchloom build --dry-run
fetchloom build --dry-run clean
fetchloom build -n --json
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
| `-n`, `--dry-run` | say what each step would run, what it reads and where its result goes; run nothing |

[global options](README.md#global-options) apply. without `--dry-run`, `build` stops with exit 2 and says to add it.

## what a dry run shows

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

`--json` returns `result.project`, `result.selected`, the recipes named or every recipe when none was, `result.dry_run`, always `true`, and `result.steps`, one record a step in build order:

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

## exit codes

0 means the dry run completed, whatever it found. a name that is not a recipe, or a build without `--dry-run`, is a usage error, exit 2. code that is a link or a special file is `project.invalid`, exit 50, and code that cannot be read is `local.io`, exit 50. the [shared exit codes](../reference/exit-codes.md) cover the rest.

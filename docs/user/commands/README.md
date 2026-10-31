# commands

fetchloom adds, pins, restores and checks research data from one executable.

| command | what it does | when to use it |
|---|---|---|
| [`add`](add.md) | add a dataset, lock its files and sync the project | start using a dataset in a project |
| [`sync`](sync.md) | place locked files, updating a stale lock first | restore data after cloning or changing the manifest |
| [`lock`](lock.md) | update or check pins without placing files | review an input change before syncing |
| [`list`](list.md) | show dataset references, locked sizes and folders | inspect the project manifest and lock |
| [`search`](search.md) | find datasets on public catalogs | choose a reference from plain words |
| [`status`](status.md) | list missing, edited and extra files | check local inputs without downloads |
| [`path`](path.md) | return an absolute dataset or file path | locate an input from a script or notebook |
| [`remove`](remove.md) | remove an entry and unchanged managed files | stop using a dataset |
| [`get`](get.md) | fetch once without project files | inspect a source before adding it |
| [`cache`](cache.md) | report cache location, sizes and projects | find where shared bytes live |
| [`cache gc`](cache.md#gc) | remove unneeded cached bytes after a day, or with `--now` | clean up the cache |
| [`cache verify`](cache.md#verify) | rehash cache objects and mark changed bytes | investigate suspected cache damage |
| `help concepts` | explain the shopping list, receipt and pantry | learn the [project model](../concepts.md) |
| `help <command>` | show a command's help | check syntax at the terminal |
| bare `fetchloom` | show project status or the welcome | find your next command |

## global options

every command takes `-q`, `-v`, `--json`, `--offline`, `--yes`, `--color` and `-C <dir>`. `add` and `get` also take `--select <glob>`, `--exclude <glob>` and `--no-extract`. `add` takes `--as <name>` to name the dataset, and `get` takes `-o <dir>` to choose its folder.

| option | effect |
|---|---|
| `-q`, `--quiet` | keep stdout answers and errors; hide other normal output; questions still apply |
| `-v`, `--verbose` | add details, error kinds and work counters |
| `-vv` | also enable debug logs |
| `--json` | print one result document on stdout, including failures; never prompt |
| `--offline` | prevent HTTP and DNS requests by fetchloom |
| `-y`, `--yes` | approve confirmations, but never choose a dataset for you |
| `--color <when>` | choose `auto` (default), `always` or `never` |
| `-C <dir>` | change the working directory before running |
| `-h`, `--help` | print help, including after a command |
| `-V`, `--version` | print the version; root-level option |

all except `--version` apply before or after a subcommand. quiet conflicts with verbosity. help stays text with `--json`. [environment](../reference/environment.md) lists persistent shell settings and color precedence. fetchloom has no config file.

```sh
fetchloom help concepts
fetchloom help add
fetchloom help cache gc
```

`help <command>` matches `<command> --help`. a shell alias can provide a [shorter name](../windows.md#short-name).

## project discovery

project commands find the nearest `data.toml` at or above the working directory. `-C` chooses where that search starts. `add` creates a manifest in the current folder when none is found. bare `fetchloom` shows status inside a project and a welcome outside one.

## output

stdout carries paths from `get` and `path`, the `list` table, version, welcome, help or JSON. `cache` and `search` print their bare path or canonical references on stdout when stdout is redirected or quiet mode hides the story; a terminal story already shows them. stderr carries progress, warnings, summaries and errors.

a ready `path` has no closing summary. successful `lock --check` is silent unless verbose or JSON output was requested. quiet mode keeps stdout answers and errors. JSON mode returns one document and never prompts; explicit debug logging can still write stderr.

[terminal output](../../dev/architecture/output.md) defines the palette and terminal fallbacks. [search selection](search.md#choosing-a-dataset) explains the picker and noninteractive refusal to guess.

[scripting](../scripting.md) covers machine-readable answers. [exit codes](../reference/exit-codes.md) distinguishes usage, network, integrity and local failures.

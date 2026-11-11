# commands

fetchloom adds, pins, restores and checks research data from one executable.

| command | what it does | when to use it |
|---|---|---|
| [`add`](add.md) | add a dataset, lock its files and sync the project | start using a dataset in a project |
| [`sync`](sync.md) | place locked files, updating a stale lock first, for every dataset or only those named | restore data after cloning or changing the manifest |
| [`lock`](lock.md) | update or check pins without placing files | review an input change before syncing |
| [`list`](list.md) | show dataset references, locked sizes and folders | inspect the project manifest and lock |
| [`export`](export.md) | write the SHA-256 of placed files, or the links of downloaded ones | check files with `sha256sum`, or download them with another tool |
| [`search`](search.md) | find datasets on public catalogs | choose a reference from plain words |
| [`status`](status.md) | list missing, edited and extra files | check local inputs without downloads |
| [`path`](path.md) | return an absolute dataset or file path | locate an input from a script or notebook |
| [`remove`](remove.md) | remove an entry and unchanged managed files | stop using a dataset |
| [`get`](get.md) | fetch once without project files: one link, a list, or one file at a path you name | inspect a source before adding it, download a list of links, or put one file where a script reads it |
| [`cache`](cache.md) | report cache location, sizes and projects | find where shared bytes live |
| [`cache gc`](cache.md#gc) | remove unneeded cached bytes after a day, or with `--now` | clean up the cache |
| [`cache verify`](cache.md#verify) | check cached files, so a changed one is downloaded again | investigate suspected cache damage |
| [`settings`](settings.md) | show each setting, its value and where it came from | check what a run will use, and why |
| [`auth`](auth.md) | sign in once, with the key shared with each source's own tools | download private, restricted or gated data |
| [`completions`](completions.md) | print a shell completion script | let your shell complete commands and options |
| `help concepts` | explain data.toml, data.lock, the cache and sync, with an analogy | learn the [project model](../concepts.md) |
| `help environment`, `help exit-codes` | summarize settings, or explain what each exit code means | the [environment](../reference/environment.md) and [exit code](../reference/exit-codes.md) pages have the details |
| `help <command>` | show a command's help | check syntax at the terminal |
| bare `fetchloom` | show project status or the welcome | find your next command |

## global options

every command takes `-q`, `-v`, `--json`, `--offline`, `--yes`, `--color`, `-C <dir>` and `--mirror <url>`. `add`, `remove`, `sync`, `lock` and `cache gc` also take `-n`, `--dry-run`. `add` and `get` also take `--select <glob>`, `--exclude <glob>` and `--no-extract`. `add` takes `--as <name>` to name the dataset, and `get` takes `-o <file>` to write one file, `--force` to replace one already there, `-d <folder>` to choose its folder, `-i <file>` to read links from a list, and `--sha256 <hex>` to check a single file.

| option | effect |
|---|---|
| `-q`, `--quiet` | keep stdout answers, warnings and errors; hide other normal output; questions still apply |
| `-v`, `--verbose` | add details, error kinds and work counters |
| `-vv` | also enable debug logs |
| `--json` | print one result document on stdout, including failures; never prompt |
| `--offline` | prevent HTTP and DNS requests by fetchloom |
| `-y`, `--yes` | answer yes to questions, but never pick a dataset for you |
| `--color <when>` | choose `auto` (default), `always` or `never` |
| `-C <dir>` | change the working directory before running; help, version and completions ignore it |
| `--mirror <url>` | ask the [mirror](../mirrors.md) at this web address or folder for pinned files before their sources; repeat it for more. these come before the mirrors the settings name |
| `-n`, `--dry-run` | with `add`, `remove`, `sync`, `lock` and `cache gc`, plan project and cache changes without applying them; sources may still be asked what a link holds. explicit `FETCHLOOM_TRACE` output is still written |
| `-h`, `--help` | print help, including after a command |
| `-V`, `--version` | print the version; root-level option |

all except `--version` and `--dry-run` apply before or after a subcommand; `--dry-run` goes after its command, as in `fetchloom sync --dry-run`. quiet conflicts with verbosity. [environment](../reference/environment.md) lists persistent shell settings and color precedence. fetchloom has no config file.

```sh
fetchloom help concepts
fetchloom help add
fetchloom help cache gc
fetchloom help --json
```

`help <command>` matches `<command> --help`. help is text for people. `fetchloom help --json` prints one JSON document describing every command, option and example instead; `<command> --help --json` describes only that command. a shell alias can provide a [shorter name](../windows.md#short-name).

## project discovery

project commands find the nearest `data.toml` at or above the working directory, inside the git repository the working directory is in. `-C` chooses where that search starts, and is how you use a project above the repository. `add` creates a manifest in the current folder when none is found. bare `fetchloom` shows status inside a project and a welcome outside one. [finding the project](../reference/project-folder.md#finding-the-project) has the exact rule.

## output

stdout carries paths from `get` and `path`, the `list` table, version, welcome, help or JSON. `cache` and `search` print their bare path or canonical references on stdout when stdout is redirected or quiet mode hides the story; a terminal story already shows them. stderr carries progress, warnings, summaries and errors.

a ready `path` has no closing summary. successful `lock --check` is silent unless verbose or JSON output was requested. quiet mode keeps stdout answers, warnings and errors. JSON mode returns one document and never prompts; explicit debug logging can still write stderr.

[terminal output](../../dev/architecture/output.md) defines the palette and terminal fallbacks. [search selection](search.md#choosing-a-dataset) explains the picker and noninteractive refusal to guess.

[scripting](../scripting.md) covers machine-readable answers. [exit codes](../reference/exit-codes.md) distinguishes usage, network, integrity and local failures.

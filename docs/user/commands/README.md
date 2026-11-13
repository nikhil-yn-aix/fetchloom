# commands

fetchloom adds, pins, restores and checks research data from one executable.

| command | what it does | when to use it |
|---|---|---|
| [`add`](add.md) | add a dataset, lock its files and sync the project | start using a dataset in a project |
| [`sync`](sync.md) | place locked files, updating a stale lock first, for every dataset or only those named | restore data after cloning or changing the manifest |
| [`lock`](lock.md) | update or check pins without placing files | review an input change before syncing |
| [`build`](build.md) | run the recipes in data.toml, pin what they made and place it; `--dry-run` shows the commands first, `--check` runs them again to compare with data.lock, and `--prepare` fetches what they read | make a dataset from others with a [recipe](../recipes.md) of yours |
| [`list`](list.md) | show dataset references, locked sizes and folders | inspect the project manifest and lock |
| [`export`](export.md) | write the SHA-256 of placed files, or the links of downloaded ones | check files with `sha256sum`, or download them with another tool |
| [`push`](push.md) | copy the pinned files from the cache into a folder mirror | share a project's data through a lab disk or a usb drive |
| [`search`](search.md) | find datasets on hugging face, kaggle, zenodo and other sites | choose a reference from plain words |
| [`status`](status.md) | list missing, edited and extra files; `--check` fails while the data is not ready | check local inputs without downloads |
| [`path`](path.md) | return an absolute dataset or file path | locate an input from a script or notebook |
| [`remove`](remove.md) | remove an entry and unchanged managed files | stop using a dataset |
| [`drop`](drop.md) | leave files out of their dataset for good, as patterns in data.toml, and delete them from data/ | take bad samples, or a part you do not use, out of a dataset |
| [`get`](get.md) | fetch once without project files: one link, a list, or one file at a path you name | inspect a source before adding it, download a list of links, or put one file where a script reads it |
| [`cache`](cache.md) | report cache location, sizes and projects | find where shared bytes live |
| [`cache gc`](cache.md#gc) | remove unneeded cached bytes after a day, or with `--now`; with `--inputs`, also raw data only builds read that a folder mirror holds | clean up the cache |
| [`cache verify`](cache.md#verify) | check cached files, so a changed one is downloaded again | investigate suspected cache damage |
| [`settings`](settings.md) | show each setting, its value and where it came from | check what a run will use, and why |
| [`auth`](auth.md) | sign in once, with the key shared with each source's own tools | download private, restricted or gated data |
| [`completions`](completions.md) | print a shell completion script | let your shell complete commands, options and dataset names |
| `help concepts` | explain data.toml, data.lock, the cache and sync, with an analogy, then recipes, `place = false`, mirrors and how to edit a dataset's files | learn the [project model](../concepts.md) |
| `help recipes` | show how to write a recipe in data.toml, with an example to copy | write your first [recipe](../recipes.md) at the terminal |
| `help environment`, `help exit-codes` | summarize settings, or explain what each exit code means | the [environment](../reference/environment.md) and [exit code](../reference/exit-codes.md) pages have the details |
| `help <command>` | show a command's help | check syntax at the terminal |
| bare `fetchloom` | show project status or the welcome | find your next command |

from the first release, the archives and debian packages also carry a manual page with the global options and a line for each command: `man fetchloom` once the debian package is installed, or `man -l fetchloom.1` in an unpacked archive.

## global options

every command takes `-q`, `-v`, `--json`, `--progress`, `--offline`, `--yes`, `--color`, `-C <dir>`, `--mirror <url>` and `--no-config`. `add`, `get`, `remove`, `drop`, `sync`, `lock`, `build`, `push` and `cache gc` also take `-n`, `--dry-run`. `add` and `get` also take `--select <glob>`, `--exclude <glob>`, `--no-extract` and `-i <file>`, which reads links from a list. `add` takes `--as <name>` to name the dataset, and `get` takes `-o <file>` to write one file, `--force` to replace one already there, `-d <folder>` to choose its folder and `--sha256 <hex>` to check a single file. `push` takes `--to <folder>` to name its mirror, and `--inputs` to add what its recipes are made from; `cache gc` takes `--inputs` to free the raw data only builds read where a folder mirror holds it. the table below says what each global option does.

| option | effect |
|---|---|
| `-q`, `--quiet` | keep stdout answers, warnings and errors; hide other normal output; questions still apply |
| `-v`, `--verbose` | add details, error kinds and work counters, and start a command that downloads, hashes or places files with [what it spends](../resources.md#see-what-a-run-uses) |
| `-vv` | also enable debug logs, which name local paths, dataset references and the queries sent to sources; read them before you share them, as [what leaves your machine](../concepts.md#what-leaves-your-machine) says |
| `--json` | print one result document on stdout, including failures; never prompt |
| `--progress` | with `--json`, keep the plain progress lines a long run prints on stderr where no bar shows: what it does, at once for a download or a lookup and after 2 s for other work, then how far it got every 10 s. stdout still holds the one document; `-q` hides the lines |
| `--offline` | prevent HTTP and DNS requests by fetchloom |
| `-y`, `--yes` | answer yes to questions, but never pick a dataset for you |
| `--color <when>` | choose `auto` (default), `always` or `never` |
| `-C <dir>` | change the working directory before running; help, version and completions ignore it |
| `--mirror <url>` | ask the [mirror](../mirrors.md) at this web address or folder for pinned files before their sources; repeat it for more. these come before the mirrors the settings name |
| `--no-config` | leave the [settings file](../reference/environment.md#settings-file) unread for this run, also one that would stop it, so its values give way to the variables and defaults; `settings` says the file was bypassed. `FETCHLOOM_NO_CONFIG=1` does the same. data.toml, data.lock and keys are read as always |
| `-n`, `--dry-run` | with `add`, `remove`, `drop`, `sync`, `lock` and `cache gc`, plan project and cache changes without applying them; sources may still be asked what a link holds. with `get`, tell what would be fetched and where it would land, and write nothing. with `build`, show what each step would run, and run nothing. with `push`, say what would be copied to the mirror, and copy nothing. explicit `FETCHLOOM_TRACE` output is still written |
| `-h` | print the short help, including after a command: the commands and options used most, or a command's usage, examples and own options, then where the rest is |
| `--help` | print the whole help, including after a command. the first of `-h` and `--help` on a line decides |
| `-V`, `--version` | print the version; with `-v`, also the OS, the architecture, the git commit the build came from and the cache folder; root-level option |

all but `--version` and `--dry-run` go before or after the command; `--dry-run` goes after it, as in `fetchloom sync --dry-run`. quiet conflicts with verbosity. [environment](../reference/environment.md) lists the variables and color precedence, a [settings file](../reference/environment.md#settings-file), `settings.toml`, keeps settings for every run, and [`fetchloom settings`](settings.md) shows each setting and where it came from.

```sh
fetchloom help concepts
fetchloom help recipes
fetchloom help add
fetchloom help cache gc
fetchloom help --json
```

`help <command>` is `<command> --help`, the whole help, as text for people. `fetchloom help --json` prints one JSON document describing every command, option and example, and `<command> --help --json` only that command. help and messages always say `fetchloom`, also when you typed `fl`, its short name.

## project discovery

project commands find the nearest `data.toml` at or above the working directory, inside its git repository; [finding the project](../reference/project-folder.md#finding-the-project) has the exact rule. `-C` chooses where that search starts, and is how you use a project above the repository. after a run with `-C`, a hint, `next:` line, help or error that names a command for the project, or a `get` into that folder, names the folder after `-C` as a full path, as `` hint: `fetchloom -C /home/ada/first sync` places missing or updated files and reports any conflicts it keeps``, so it works as typed where you ran it. a command for the whole machine, such as `auth`, `cache`, `search` or `settings`, and a line about what other clones run keep the plain command. `add` starts a project in the current folder when none is found, and bare `fetchloom` shows status inside a project and a welcome outside one.

## typos

a command, option, dataset name, path in a dataset or help topic that fetchloom does not know is a usage error, exit 2. when a known one is close, the help suggests your whole command line with only that word changed, so `fetchloom remove --dry-run irs` suggests `fetchloom remove --dry-run iris`, still a dry run. close means at most one change for every three letters, and at least one: a letter added, dropped, changed or swapped with its neighbour. commands match in any case, so `fetchloom Serach iris` suggests `fetchloom search iris`, and other tools' names for three of them count too: `fetchloom rm iris` suggests `fetchloom remove iris`, `fetchloom ls` suggests `fetchloom list` and `fetchloom version` suggests `fetchloom --version`. an unknown help topic lists the topics: concepts, recipes, environment and exit-codes, or a command. a line holding a link with a query, a fragment or a login is never repeated, since any of them may carry a secret under a name fetchloom does not know, such as dropbox's `rlkey`; the suggestion then names the command and the corrected word alone.

## output

stdout carries paths from `get` and `path`, the `list` table, version, welcome, help or JSON; `cache` and `search` print their bare path or references there when stdout is redirected or `-q` hides the story, which already shows them at a terminal. stderr carries progress, warnings, summaries and errors.

at a terminal, a line on stderr wider than the terminal wraps between words, never inside backticks or quotes. help there puts an example's comment below it when both do not fit, and continues a command too long for a line after `\`, or a backtick on windows, so it still runs when pasted. a pipe, a file or a coding agent gets each line whole.

a ready `path` has no closing summary, and a passing `lock --check` ends with `Ok data.lock matches data.toml`. [terminal output](../../dev/architecture/output.md) defines the palette and terminal fallbacks, [search selection](search.md#choosing-a-dataset) the picker and why it never guesses without a terminal, [scripting](../scripting.md) the machine-readable answers and [exit codes](../reference/exit-codes.md) the failure kinds.

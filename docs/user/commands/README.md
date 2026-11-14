# commands

fetchloom adds, pins, restores and checks research data from one executable.

| command | what it does | when to use it |
|---|---|---|
| [`add`](add.md) | add a dataset, lock its files and sync the project | start using a dataset in a project |
| [`sync`](sync.md) | place locked files, updating a stale lock first, for every dataset or only those named | restore data after cloning or changing the manifest |
| [`lock`](lock.md) | update or check pins without placing files | review an input change before syncing |
| [`build`](build.md) | run the recipes in data.toml, pin what they made and place it; `--dry-run` shows the commands first, `--check` runs them again to compare with data.lock, and `--prepare` fetches what they read | make a dataset from others with a [recipe](../guides/recipes.md) of yours |
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
| bare `fetchloom` | show project status or the welcome | find your next command |

from the first release, the archives and debian packages also carry a manual page with the global options and a line for each command: `man fetchloom` once the debian package is installed, or `man -l fetchloom.1` in an unpacked archive.

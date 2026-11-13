# exit codes

exit codes group failures, while error kinds identify the cause.

| Exit | Error kinds |
|---|---|
| 0 | success |
| 1 | `unconfirmed`, `lock.outdated`, `data.not_ready` |
| 2 | `usage` |
| 10 | `reference.invalid`, `reference.unresolved`, `reference.ambiguous`, `source.invalid` |
| 20 | `network.unreachable`, `network.status`, `network.rate_limited`, `network.refused`, `offline.missing` |
| 30 | `integrity.mismatch`, including size mismatches and changed bytes found by cache verification |
| 40 | `auth.required` |
| 50 | `local.io`, `local.not_empty`, `project.invalid`, `project.changed` |
| 60 | `build.needed`, `build.failed`, `build.differs` |
| 130 | `interrupted` |

a cancel you choose at a question keeps its exit code: q at the dataset list exits 2, no exits 1 and ctrl-c exits 130. a person sees one `Cancelled` line instead of `error:`, and `--json`, which never asks, reports errors as before. a stop you ask for with ctrl-c during other work is no error either: it exits 130, a person sees a `Stopped` line instead of `error:`, and `--json` reports it as `interrupted`. on linux and macos a SIGTERM, as a scheduler sends a job it ends, stops a run the same way.

`--json` returns the kind in `errors[0].kind`; verbose output also shows it. help text is a suggestion for a person to check, not a command for a script to run unchecked.

## choosing a next step

| Kind | Next step |
|---|---|
| `unconfirmed` | check the download size, or the room a build says its steps need, and pass `--yes` only if you approve; after a git merge, choose a side for each dataset named with `fetchloom lock --ours <name>` or `--theirs <name>`, which `--yes` never does. for `cache gc --inputs`, check `result.inputs` and the folder mirrors it relies on, and pass `--yes` only once those mirrors are to stay. for `auth logout`, ask whether the tools that read the same key file, which the message names, may be signed out too, and pass `--yes` only then |
| `lock.outdated` | run `fetchloom lock` deliberately and commit the updated lock. from a `--locked` run, `errors[0].fix` is null, as it is for every error such a run ends with whose next step would change data.lock. from `fetchloom build --check` or `--locked`, a recipe data.lock does not pin as building it now would make is outdated too: build it deliberately, then commit data.lock and data.lock.d |
| `data.not_ready` | `sync --strict` or `status --check` found the data asked for not ready to use. `errors[0].details` names each file or dataset with what makes it ready, as ``data/iris/iris.csv changed since fetchloom placed it: save what you need, then `fetchloom sync --force` replaces it``, and the run replaced nothing. `errors[0].fix` is `fetchloom sync` only when a plain sync puts everything in place; otherwise it is null, since replacing an edit, pinning or building is the user's call |
| `usage` | check arguments, such as a path given to `add` that names no file or folder, and [environment settings](environment.md) |
| `reference.invalid`, `reference.unresolved`, `source.invalid` | check the reference and the source's supported access rules. for `source.invalid`, `help` names who can change what was refused: the dataset's owner, for a file name fetchloom refuses or a file kept where fetchloom cannot pin it, or fetchloom's [issues](https://github.com/nikhil-yn-aix/fetchloom/issues), for an answer fetchloom cannot read |
| `reference.ambiguous` | choose a returned canonical reference and rerun; `--yes` never chooses |
| `network.unreachable`, `network.status` | check connectivity and the link before retrying |
| `network.rate_limited`, `network.refused` | respect the source's wait or access requirement |
| `offline.missing` | prepare the missing data online |
| `integrity.mismatch` | investigate changed bytes. when a source no longer serves the bytes data.lock pins, keep them from a folder mirror that holds them: someone who has them runs `fetchloom push --to <folder>`, then `fetchloom sync --mirror <folder>`. run `fetchloom lock --upgrade <name>` only if the new bytes are intended; `errors[0].fix` is null for it, since only a person may choose other bytes than data.lock pins |
| `auth.required` | obtain access and set supported credentials |
| `local.io` | check free space, permissions and files held open; after `add <folder> --as <name>`, check that the folder holds the files data.lock pins. when `push` says something replaced a file it wrote, stop whatever else writes into the mirror's folder, such as a sync client, a backup tool or antivirus, then push again. a build whose step the `build_floor` setting stopped needs free space on the cache's disk, or a lower floor |
| `local.not_empty` | preserve existing files and choose another `get --dir` folder or `get -o` name; `get -o --force` replaces a file you no longer need. for `push`, name an empty folder or a mirror's top folder, and check that its disk is mounted |
| `project.invalid` | correct or restore the manifest, lock or project folders |
| `project.changed` | check the change made to `data.toml` or `data.lock` while the command ran, then run it again |
| `build.needed` | a recipe changed, or was never built, since data.lock pinned it: read what moved in `errors[0].details`, then build it deliberately, since building runs the project's code; `errors[0].fix` is always null for it |
| `build.failed` | a recipe's step could not start, failed or wrote what a result cannot hold: read the last lines of its output in `errors[0].details`, or above the error when it showed as the step ran, and the whole of it in `.fetchloom/builds/<name>.log`; data.lock and the data folder are as they were |
| `build.differs` | a step built again from the same inputs, command and code made other files than data.lock pins, as another version of a program or library it uses, the time or an unseeded random number can make it do; data.lock and the data folder are as they were. `errors[0].details` names the files that differ, and the rebuild is kept: once a person has looked at it, as `fetchloom status -v` shows where its files are, `fetchloom build --accept <name>` pins it. `errors[0].fix` is always null for it |
| `interrupted` | retry when ready; resumable downloads may be reused |

a cached archive that cannot be read because the filesystem failed uses `local.io`, exit 50. archive bytes that are malformed use `source.invalid`, exit 10.

## success with differences

`status` exits 0 when it finds differences. `sync` exits 0 when it preserves conflicts. inspect status arrays and `result.placement.kept` before using the data, or ask for `status --check` and `sync --strict`, which exit 1 with `data.not_ready` while the data is not ready to use: a file with other bytes than data.lock pins, one not in place yet, a recipe's result not on this machine, or a dataset data.lock does not pin as data.toml asks. a cache registration warning also leaves sync successful.

`cache verify` returns 30 when it finds changed bytes, or 50 for unreadable paths when no bytes changed. `cache gc` returns 50 if any cleanup fails. both retain their partial results in [JSON output](json-output.md#command-results).

batch download failures use the first failed file in job order for the overall kind and exit code. each failure remains in `errors[0].failures`. interruption or another whole-batch failure returns immediately.

`get` with several links works the same way with links in place of files: the first failed link in command line and list order decides the kind and exit code, and each failed link remains in `errors[0].failures` and in `result.items`. a `--sha256` mismatch is `integrity.mismatch`, exit 30.

the [python helper](python-helper.md) also raises errors of its own, with no process exit code, so their `code` is `None`: `usage` when `get()` is given both `path` and `folder`, or `force=True` without `path`.

[troubleshooting](../troubleshooting/README.md) pairs recorded error text with fixes.

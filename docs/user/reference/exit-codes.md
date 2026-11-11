# exit codes

exit codes group failures, while error kinds identify the cause.

| exit | error kinds |
|---|---|
| 0 | success |
| 1 | `unconfirmed`, `lock.outdated` |
| 2 | `usage` |
| 10 | `reference.invalid`, `reference.unresolved`, `reference.ambiguous`, `source.invalid` |
| 20 | `network.unreachable`, `network.status`, `network.rate_limited`, `network.refused`, `offline.missing` |
| 30 | `integrity.mismatch`, including size mismatches and changed bytes found by cache verification |
| 40 | `auth.required` |
| 50 | `local.io`, `local.not_empty`, `project.invalid`, `project.changed` |
| 60 | `build.needed`, `build.failed`, `build.differs` |
| 130 | `interrupted` |

a cancel you choose at a question keeps its exit code: q at the dataset list exits 2, no exits 1 and ctrl-c exits 130. a person sees one `Cancelled` line instead of `error:`, and `--json`, which never asks, reports errors as before. a stop you ask for with ctrl-c during other work is no error either: it exits 130, a person sees a `Stopped` line instead of `error:`, and `--json` reports it as `interrupted`.

`--json` returns the kind in `errors[0].kind`; verbose output also shows it. help text is a suggestion for a person to check, not a command for a script to run unchecked.

## choosing a next step

| kind | next step |
|---|---|
| `unconfirmed` | check the download size and pass `--yes` only if you approve; after a git merge, choose a side for each dataset named with `fetchloom lock --ours <name>` or `--theirs <name>`, which `--yes` never does |
| `lock.outdated` | run `fetchloom lock` deliberately and commit the updated lock |
| `usage` | check arguments and [environment settings](environment.md) |
| `reference.invalid`, `reference.unresolved`, `source.invalid` | check the reference and the source's supported access rules |
| `reference.ambiguous` | choose a returned canonical reference and rerun; `--yes` never chooses |
| `network.unreachable`, `network.status` | check connectivity and the link before retrying |
| `network.rate_limited`, `network.refused` | respect the source's wait or access requirement |
| `offline.missing` | prepare the missing data online |
| `integrity.mismatch` | investigate changed bytes; upgrade pins only if intended |
| `auth.required` | obtain access and set supported credentials |
| `local.io` | check free space, permissions and files held open; after `add <folder> --as <name>`, check that the folder holds the files data.lock pins |
| `local.not_empty` | preserve existing files and choose another `get --dir` folder or `get -o` name; `get -o --force` replaces a file you no longer need |
| `project.invalid` | correct or restore the manifest, lock or project folders |
| `project.changed` | check the change made to `data.toml` or `data.lock` while the command ran, then run it again |
| `build.needed` | a recipe changed, or was never built, since data.lock pinned it: read what moved in `errors[0].details`, then build it deliberately, since building runs the project's code; `errors[0].fix` is always null for it |
| `build.failed` | a recipe's step could not start, failed or wrote what a result cannot hold: read the last lines of its output in `errors[0].details` and the whole of it in `.fetchloom/builds/<name>.log`; data.lock and the data folder are as they were |
| `build.differs` | a step built again from the same inputs, command and code made other files than data.lock pins: the step is not reproducible, as one that uses the time or an unseeded random number is; data.lock and the data folder are as they were |
| `interrupted` | retry when ready; resumable downloads may be reused |

a cached archive that cannot be read because the filesystem failed uses `local.io`, exit 50. archive bytes that are malformed use `source.invalid`, exit 10.

## success with differences

the python helper also has a helper-only `conflict.kept` error with no process exit code; see [strict sync](../scripting.md#python-helper).

`status` exits 0 when it finds differences. `sync` exits 0 when it preserves conflicts. inspect status arrays and `result.placement.kept` before using the data. a cache registration warning also leaves sync successful.

`cache verify` returns 30 when it finds changed bytes, or 50 for unreadable paths when no bytes changed. `cache gc` returns 50 if any cleanup fails. both retain their partial results in [JSON output](json-output.md#command-results).

batch download failures use the first failed file in job order for the overall kind and exit code. each failure remains in `errors[0].failures`. interruption or another whole-batch failure returns immediately.

`get` with several links works the same way with links in place of files: the first failed link in command line and list order decides the kind and exit code, and each failed link remains in `errors[0].failures` and in `result.items`. a `--sha256` mismatch is `integrity.mismatch`, exit 30.

[troubleshooting](../troubleshooting.md) pairs recorded error text with fixes.

# exit codes

exit codes group failures, while error kinds identify the cause.

| exit | error kinds |
|---|---|
| 0 | success |
| 1 | `unconfirmed`, `lock.outdated` |
| 2 | `usage` |
| 10 | `reference.invalid`, `reference.unresolved`, `source.invalid` |
| 20 | `network.unreachable`, `network.status`, `network.rate_limited`, `network.refused`, `offline.missing` |
| 30 | `integrity.mismatch`, including size mismatches and changed bytes found by cache verification |
| 40 | `auth.required` |
| 50 | `local.io`, `local.not_empty`, `project.invalid` |
| 130 | `interrupted` |

`--json` returns the kind in `errors[0].kind`; verbose output also shows it. help text is a suggestion for a person to check, not a command for a script to run unchecked.

## choosing a next step

| kind | next step |
|---|---|
| `unconfirmed` | check the download size and pass `--yes` only if you approve |
| `lock.outdated` | run `fetchloom lock` deliberately and commit the updated lock |
| `usage` | check arguments and [environment settings](environment.md) |
| `reference.invalid`, `reference.unresolved`, `source.invalid` | check the reference and the source's supported access rules |
| `network.unreachable`, `network.status` | check connectivity and the link before retrying |
| `network.rate_limited`, `network.refused` | respect the source's wait or access requirement |
| `offline.missing` | prepare the missing data online |
| `integrity.mismatch` | investigate changed bytes; upgrade pins only if intended |
| `auth.required` | obtain access and set supported credentials |
| `local.io` | check free space, permissions and files held open |
| `local.not_empty` | preserve existing files and choose another `get -o` folder |
| `project.invalid` | correct or restore the manifest, lock or project folders |
| `interrupted` | retry when ready; resumable downloads may be reused |

## success with differences

`status` exits 0 when it finds differences. `sync` exits 0 when it preserves conflicts. inspect status arrays and `result.placement.kept` before using the data. a cache registration warning also leaves sync successful.

`cache verify` returns 30 when it finds changed bytes, or 50 for unreadable paths when no bytes changed. `cache gc` returns 50 if any cleanup fails. both retain their partial results in [JSON output](json-output.md#command-results).

batch download failures use the first failed file in job order for the overall kind and exit code. each failure remains in `errors[0].failures`. interruption or another whole-batch failure returns immediately.

[troubleshooting](../troubleshooting.md) pairs recorded error text with fixes.

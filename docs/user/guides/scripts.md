# Use fetchloom in scripts

`--json` gives scripts a versioned result, error kinds and work counters without terminal progress.

```sh
fetchloom --json lock --check
fetchloom --json --yes sync --locked --strict
fetchloom --json status --check
```

stop when the lock check fails. `--strict` and `--check` fail with `data.not_ready`, exit 1, while the data is not ready to use, as when sync kept an edited file. without them, status differences and kept conflicts exit 0, so inspect them before using the data.

## reading an answer

| field | use |
|---|---|
| `schema` | require the supported integer version, currently `1` |
| `command` | identify the command, or null for welcome, version or an unrecognized prefix |
| `status` | require `ok` for successful completion |
| `result` | read the command-specific fields |
| `warnings` | inspect warnings even in quiet mode: branch on each one's `kind`, listed in [warning kinds](../reference/json-output.md#warning-kinds), never on its `message` |
| `errors` | read `kind`, `message`, `details`, nullable `help` and nullable `fix`, a command to run next; download failures also include `failures` |
| `work` | inspect request, byte, retry, resume, cache and placement counters |

JSON appears on stdout on success and failure, including usage errors and help: `fetchloom help --json` describes every command and option. a killed process may leave no valid JSON, so treat missing or invalid JSON as failure.

a long job that a person watches can still show how it goes: `--progress` keeps plain progress lines on stderr: a download or a lookup says what it does at once, other work after 2 s, then each says how far it got every 10 s, while stdout holds the one document. the lines are for a person, so read stdout and stderr apart and never parse them.

```sh
fetchloom --json --progress get https://example.org/big.bin
```

while the download runs, stderr shows:

```text
Downloading big.bin · 1.9 MiB
Downloading 1.4 MiB of 1.9 MiB · 10 s
```

[JSON output](../reference/json-output.md) defines every result and counter.

## checking and syncing

```python
import json
import subprocess

completed = subprocess.run(
    ["fetchloom", "--json", "--yes", "sync", "--locked"],
    stdin=subprocess.DEVNULL,
    capture_output=True,
    encoding="utf-8",
    check=False,
)
answer = json.loads(completed.stdout)
if answer["schema"] != 1:
    raise RuntimeError("unsupported fetchloom JSON schema")
if completed.returncode != 0 or answer["status"] != "ok":
    error = answer["errors"][0]
    raise RuntimeError(f"{error['kind']}: {error['message']}")
if answer["result"]["placement"]["kept"]:
    raise RuntimeError(answer["result"]["placement"]["kept"])
```

pass `--yes` only when you approve the download: JSON mode cannot answer a question, so an unapproved large download fails with `unconfirmed` and exit 1. never add `--force` by itself, since it overwrites a person's files.

`kept` lists the files sync left as they were, each with its reason, and does not include other extra files. for those, run `fetchloom --json status` and check its arrays, which [status](../commands/status.md#categories) explains: every array empty, but `unplaced`, means status found no differences. it compares sizes and times, not every byte, unless `--verify` asks it to hash every placed file.

a job that needs one dataset names it, as in `fetchloom --json --yes sync --locked small`: only small is downloaded, placed and checked against data.lock, and `result.selected` echoes `["small"]`.

## adding and fetching

```sh
fetchloom --json add https://huggingface.co/datasets/scikit-learn/iris
fetchloom --json get https://zenodo.org/records/3608990 --dir flowers
fetchloom --json get https://example.org/iris.csv -o iris.csv
```

`add` returns the added names in `result.datasets`, empty when the entry already existed. `get` returns `result.folder` and `result.files`, each file with its path in that folder, size and SHA-256, and with `-o`, `result.file`, the file's absolute path. `fetchloom --json get -i links.txt` returns one record for each link in `result.items`, with its own `status`, `folder`, `files` and `error`; a run where some links failed still exits nonzero, so read `result.items` to see which arrived. [JSON output](../reference/json-output.md#command-results) lists the fields. `get` pins nothing, so another run can take changed bytes.

## finding paths

```sh
fetchloom --json path iris/Iris.csv
fetchloom -q path iris
fetchloom --json path iris --locked
```

JSON `result.path` is the absolute path, and the quiet form prints only the path on stdout. either syncs the dataset alone when it is not ready, which does not prove an edited file was replaced. `--locked` keeps that sync from changing data.lock: it fails with `lock.outdated`, exit 1, before any request. `-C` chooses the folder where the search for the project starts.

## offline jobs

follow [offline preparation](../concepts/offline-and-recovery.md#offline) before disconnecting. a prepared job can require its lock without allowing requests:

```sh
fetchloom --json --offline sync --locked
fetchloom --json --offline path iris
```

## CI cache

[CI](ci.md) keeps the cache between jobs, keyed on `data.lock`, with recipes for github actions and gitlab ci.

## errors and exit codes

branch on `errors[0].kind`, never on messages. [exit codes](../reference/exit-codes.md) lists every kind and a next step. `help` is advice for a person to check, and `fix` a command to run only once it fits what you meant. download errors list each failed file under `failures`, and the first failed file in job order sets the overall kind and exit code.

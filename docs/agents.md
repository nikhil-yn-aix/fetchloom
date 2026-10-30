# agents

a script or an AI agent drives fetchloom with the same commands a person types. this page lists what a program can rely on. [cli](cli.md) explains each command, and [formats](formats.md#the-json-document) defines every field.

## without a terminal

- no command waits for input when stdin is not a terminal. a download above 1 GiB needs `--yes`; without it the run stops with kind `unconfirmed` and exit 1.
- stdout carries only the answer: a path, the version or the JSON document. stderr carries the story: progress, warnings and errors.
- progress bars and `next:` lines need a terminal, and so does color unless `--color always` asks for it.
- `-q` keeps the stdout answer and errors, and hides the rest of the story.
- `--json` prints one document on stdout instead of the story. it never asks a question.
- `--offline` sends no request. a file missing from the cache fails with kind `offline.missing`.
- `-C <dir>` runs as if fetchloom started in `<dir>`.

## the JSON document

with `--json`, every run prints one line of JSON on stdout, on success and on failure, usage errors included. `--help` and `fetchloom help` stay text.

| field | value |
|---|---|
| `schema` | `1` |
| `command` | the command that ran, such as `add` or `cache gc`; null for the welcome, `--version` and an unreadable command line |
| `status` | `ok` or `error` |
| `result` | what the command did, or null on failure |
| `warnings` | strings, collected with `-q` too |
| `errors` | empty, or one object with `kind`, `message`, `details` and `help` |
| `work` | counters: `requests`, `bytes_down`, `retries`, `resumes`, `objects_written`, `cache_hits`, `reflinks`, `hardlinks`, `copies`, `elapsed_ms` |

branch on `status`, the exit code and `errors[0].kind`, never on message text. `errors[0].help` names the command or flag that fixes the error. [command results](formats.md#command-results) lists each command's `result` fields.

## error kinds and exit codes

| exit | kinds |
|---|---|
| 0 | success |
| 1 | `unconfirmed`, `lock.outdated` |
| 2 | `usage` |
| 10 | `reference.invalid`, `reference.unresolved`, `source.invalid` |
| 20 | `network.unreachable`, `network.status`, `network.rate_limited`, `network.refused`, `offline.missing` |
| 30 | `integrity.mismatch` |
| 40 | `auth.required` |
| 50 | `local.io`, `local.not_empty`, `project.invalid` |
| 130 | `interrupted` |

`status` finding differences and `sync` keeping files edited by hand still exit 0. read `result` to see them.

## recipes

each output below is from a real run on windows, shortened. `<project>` stands for the project's absolute path, `<folder>` for the folder `get` ran in, and `…` for fields left out.

add a dataset to the project in the current folder, creating it if needed:

```sh
fetchloom --json add zenodo:3608990
```

```json
{"schema":1,"command":"add","status":"ok","result":{"project":"<project>","datasets":["iris"],"lock_changed":true,"placement":{"placed":1,"replaced":0,"removed":0,"unchanged":0,"kept":[],"restored":[]}},"warnings":[],"errors":[],"work":{"requests":2,"bytes_down":7666,…}}
```

make `data/` match `data.lock`, as in a fresh clone. a large download needs `--yes`:

```sh
fetchloom --json --yes sync
```

```json
{"schema":1,"command":"sync","status":"ok","result":{"project":"<project>","datasets":[],"lock_changed":false,"placement":{"placed":0,"replaced":0,"removed":0,"unchanged":1,"kept":[],"restored":[]}},"warnings":[],"errors":[],"work":{"requests":0,…}}
```

check for missing, edited or extra files. every array empty means `data/` matches:

```sh
fetchloom --json status
```

```json
{"schema":1,"command":"status","status":"ok","result":{"project":"<project>","unlocked":[],"missing":[],"edited":[],"untracked":[],"outdated":[],"edited_elsewhere":[],"extra":[],"not_unpacked":[]},"warnings":[],"errors":[],"work":{…}}
```

find a dataset's folder. without `--json`, `-q` prints only the path:

```sh
fetchloom -q path iris
fetchloom --json path iris
```

```json
{"schema":1,"command":"path","status":"ok","result":"<project>\\data\\iris","warnings":[],"errors":[],"work":{…}}
```

fetch once, with no project. `folder` is where the files are:

```sh
fetchloom --json get zenodo:3608990
```

```json
{"schema":1,"command":"get","status":"ok","result":{"folder":"<folder>\\iris","files":[{"path":"iris.csv","size":4177,"sha256":"b1f0b46c3e27350cbf0f75fb3b7d36adcd997c443e764bf09255126feeb11f31","url":"https://zenodo.org/api/records/3608990/files/iris.csv/content","origin":"cache","method":"hardlink","unpacked_from":null}]},"warnings":[],"errors":[],"work":{…}}
```

a failure keeps the same shape. here a dataset name is misspelled, so the run exits 2:

```sh
fetchloom --json path irsi
```

```json
{"schema":1,"command":"path","status":"error","result":null,"warnings":[],"errors":[{"kind":"usage","message":"data.toml has no dataset called irsi","details":[],"help":"did you mean `fetchloom path iris`?"}],"work":{…}}
```

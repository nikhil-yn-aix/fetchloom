# JSON output

`--json` returns one versioned document on stdout for scripts to read.

```sh
fetchloom --json lock --check
fetchloom --json --yes sync
fetchloom --json status
```

`--json` prints one document on stdout, including usage errors, welcome and version. `--help` and `fetchloom help` still print text; an unknown help topic is a usage error with a null command. explicit debug logging may also write stderr. JSON mode never prompts; use `--yes` when approval is needed.

| field | value |
|---|---|
| `schema` | integer `1` |
| `command` | `add`, `remove`, `sync`, `lock`, `status`, `path`, `get`, `cache`, `cache gc`, `cache verify`, or null |
| `status` | `ok` or `error` |
| `result` | command result on success; also retained for cache verification findings and gc removal failures; null for other failures |
| `warnings` | array of strings, including in quiet mode |
| `errors` | empty on success; one error object on failure |
| `errors[].kind` | one of the names in [exit codes](exit-codes.md) |
| `errors[].message` | summary string |
| `errors[].details` | array of supporting strings |
| `errors[].help` | string or null |
| `errors[].failures` | present for file download failures: every failed file, in job order, with `file`, `kind`, `message`, `details` and `help` |
| `work` | counters below, present on success and failure |

batch download errors keep every file in `failures`, even though the human summary names at most ten. each file keeps its own kind, message, details and help. the overall kind and exit code come from the first failed file in job order, not completion order or severity. interruption and other failures that stop the whole batch return immediately instead.

SHA-256 mismatch details contain only the first 12 hex digits, even with `--json` or `-v`. parse `kind` and result fields rather than human messages.

bare `fetchloom` inside a project reports command `status`. outside a project, and for `--version`, command is null and result is `{"version":"0.1.0"}`. on a usage error, command is the prefix clap recognized, or null. `--json` is recognized anywhere before `--` on that error path; other display flags depend on what clap read.

## command results

| command | result fields |
|---|---|
| `add`, `remove`, `sync`, `lock` | `project`: absolute path; `datasets`: name array; `lock_changed`: boolean; `placement`: object below or null |
| `get` | `folder`: absolute path; `files`: file records below |
| `path` | absolute path string, for a dataset, file or subfolder |
| `status` | `project`: absolute path; nine arrays below |
| `cache` | `path`: configured cache root; `objects`: count; `bytes`: payload bytes; `needed`: cached payload bytes needed by registered locks; `projects`: root-path array |
| `cache gc` | `objects`: deleted count; `bytes`: deleted object payload bytes; `kept_objects`: unneeded objects retained because they are less than a day old; `kept_bytes`: their payload bytes; `leftovers`: deleted partial or temporary file count; `not_removed`: paths that could not be removed or inspected for cleanup |
| `cache verify` | `objects`: examined count; `bytes`: examined payload bytes; `changed`: hash array; `stuck`: hashes whose change marks could not be written; `unreadable`: paths whose contents, metadata or directory entries could not be read |

project `datasets` names what was added or removed, or existing datasets whose resolved reference or files changed during `lock`. ordinary `sync` and duplicate `add` return an empty array. `remove` always reports `lock_changed = true`, including when the removed dataset had no lock entry. `lock` has null placement. successful `lock --check` also has empty datasets and `lock_changed = false`.

placement has integer counts `placed`, `replaced`, `removed` and `unchanged`; `kept`, an array of `{path, reason}`; and `restored`, an array of replaced paths whose bytes another project changed through a shared hardlink. paths are project-relative. `kept` reasons are `changed` for edited managed files, `foreign` for other bytes at a target, and `unneeded` for edited managed files no longer wanted. adopted identical files count as unchanged. unrelated extra files are not included in `kept`.

`get.files` lists placement targets, so an unpacked archive is replaced by its members. each record has `path`, `size`, `sha256`, `url`, `origin`, `method`, `unpacked_from`. the path is relative to `folder`. origin is `network`, `not_modified` or `cache`; method is `reflink`, `hardlink`, `copy`, or null when already present. `unpacked_from` is the selected archive path or null. unpacked members inherit the archive's URL and origin.

status arrays:

| field | entries |
|---|---|
| `unlocked` | dataset names missing from the lock or with a changed spec |
| `removed` | dataset names in data.lock but no longer in data.toml |
| `missing` | absent target paths |
| `edited` | managed paths whose size or modification time changed |
| `untracked` | existing target paths without state records |
| `outdated` | unchanged managed paths that `sync` replaces: pinned to other hashes now, or read-only where the dataset asks for `copy = true` |
| `edited_elsewhere` | managed hardlinks showing their recorded size and modification time, whose object the cache marked as changed after fetchloom last knew them, and whose bytes hashing found changed; `sync` puts them back |
| `extra` | scanned paths outside the known targets |
| `not_unpacked` | archive paths whose member records are unavailable on this machine |

status lists all differences in JSON. human output limits each file category to 20 paths; unlocked or removed datasets and not-unpacked archives are not truncated. without an archive record, files already placed from it can also appear as extra.

`status` finding differences and `sync` keeping conflicts still return `ok` and exit 0. inspect the arrays. `cache verify` finding changed bytes returns `integrity.mismatch` and exit 30; when no bytes changed but an object or directory could not be read, it returns `local.io` and exit 50. both errors retain the verification counts and findings in `result`, alongside `errors`. unreadable object and directory paths are also named in `warnings`; unreadable objects are neither marked nor removed. counts include only objects whose metadata could be read, so an unreadable directory can hide objects from the totals. successful `changed`, `stuck` and `unreadable` arrays are empty.

`cache gc` keeps objects no project needs until they are a day old by modification time. `kept_objects` and `kept_bytes` count those young objects, not objects retained for registered projects.

`cache gc` uses `local.io` and exit 50 when a removal fails, including after other removals succeeded. this existing kind covers a partial cleanup failure. `result` retains the successful removal counts and `not_removed` paths; the array length is the failed-path count. human output and JSON `warnings` name those paths. a path already absent is not a failed removal.

successful sync still exits 0 if its cache registration cannot be written. human output and JSON `warnings` explain that gc may not keep the project's files and ask you to check cache permissions and run the command again.

## work counters

all counts and byte quantities are unsigned integers. timestamps and large counters can exceed javascript's exact integer range.

| field | counts |
|---|---|
| `requests` | explicit metadata or file request attempts, including retries; redirect hops are not counted separately |
| `bytes_down` | body bytes read, including metadata and unsuccessful attempts; excludes headers and the saved resume prefix |
| `retries` | scheduled retry waits |
| `resumes` | accepted nonzero range starts |
| `objects_written` | new objects counted after successful downloads and archive unpacking |
| `cache_hits` | download-layer hash or MD5 shortcuts, not every object used by placement |
| `reflinks`, `hardlinks`, `copies` | new placements by each method, including replacements |
| `elapsed_ms` | elapsed whole milliseconds before rendering the final report |

a warm project can place cached files with zero cache hits. there are no phase timings or bytes-written counters. cache verification reports directory and metadata read failures in `result.unreadable` rather than claiming those paths were verified.

a killed process may leave no valid JSON. treat a missing or invalid document as a failure. [scripting](../scripting.md) shows checks for exit codes, schema versions and preserved conflicts.

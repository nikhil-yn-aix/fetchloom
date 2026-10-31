# troubleshooting

error text, exit codes and local status distinguish missing inputs from changed bytes and access problems.

```sh
fetchloom status
fetchloom -v sync
```

verbose output adds error kinds and work counters. use `--json` for [structured failures](reference/json-output.md), including every failed file in a download batch.

## errors

| symptom on stderr | cause | fix |
|---|---|---|
| `error: <link> is missing` | `add` or `get` needs a reference | pass a dataset link; use `fetchloom get --help` for syntax |
| `error: offline, and 1 file the lock names is not in the cache` | an offline project needs uncached bytes | sync online first; remove `--offline` and unset `FETCHLOOM_OFFLINE` if it is `1` |
| `error: iris/iris.csv is not the file that was locked` | downloaded bytes differ from the pin | investigate the source; upgrade only if the change is intended |
| `error: downloading huge.bin needs 2.0 GiB, and fetchloom cannot ask for confirmation here` | the download needs approval without an interactive terminal | check the size, then pass `--yes` if approved |
| `error: iris/iris.csv already exists with other contents` | `get` found conflicting local bytes | move the file somewhere safe or choose another folder with `-o` |
| `error: dataset has no file or folder called not-a-file.csv` | the requested member is absent | use one of the dataset paths named in the error details |

for a size mismatch, the full stderr answer can be:

```text
error: iris/iris.csv is not the file that was locked
  expected 32 bytes, got 15
  help: if the new version is intended, run `fetchloom lock --upgrade iris`
```

if the new input is intended, run the suggested upgrade, sync and commit the lock. otherwise preserve the pin and ask the source owner for the original bytes.

## warnings and unexpected results

| symptom | cause | fix |
|---|---|---|
| sync succeeds but a file is still edited | conflicts are preserved by default | inspect `status` and JSON `placement.kept`; save edits before choosing `--force` |
| status exits 0 with differences | differences are a successful result | inspect all nine status arrays |
| an input is read-only | default placement protects shared bytes | set `copy = true` on the dataset and sync |
| a glob matches no files | it differs from listed paths or targets archive members | quote the glob and check [pattern rules](reference/data-toml.md#patterns) |
| adding the same reference rejects different filters | the existing entry already owns that reference and settings | edit `data.toml` or pass an unused `--as` name |
| an archive remains packed | the extension or bytes are unsupported, or extraction is disabled | check [archive rules](concepts.md#archives) |
| an archive has no dataset folder | it contains no regular files | use `extract = false` and sync to keep the packed archive |
| a source returns an access refusal | access is unavailable or unsupported | check the source's [credentials and limits](sources/README.md) |
| cache registration warns after sync | the cache could not record the project root | fix cache permissions and sync again before relying on gc retention |
| `cache gc` exits 50 after partial cleanup | some paths could not be removed or inspected | inspect `result.not_removed`, close open handles and fix permissions before retrying |
| `cache verify` exits 30 | cached bytes changed | allow a later online sync to replace damaged objects, then sync affected projects |
| `cache verify` exits 50 without changed bytes | objects, metadata or directory entries could not be read | inspect `result.unreadable`, fix access and verify again |
| a windows file cannot be replaced or deleted | a program holds it open, or permissions block access | close the program, check permissions and retry |
| a long path fails in another program | that program may have a shorter path limit | use shorter project and dataset folders |

copied placement inside onedrive can report:

```text
warning: iris is inside onedrive, which breaks links, so the file was copied. a folder outside onedrive lets fetchloom try links
```

move the project outside synced storage if you want fetchloom to try hardlinks. reflinks may still work inside a synced folder. [windows](windows.md) covers drive choice and Defender scanning.

## project recovery

restore a broken or older lock from git when you need its exact pins. deleting it and running `fetchloom lock` can resolve newer inputs. a newer lock version requires a newer fetchloom.

if `add` cannot restore the previous manifest after a failed sync, decode the backup JSON string in its error details or remove the named entry. do not assume cache, lock and placement changes were rolled back.

if `remove` changed metadata but could not delete files, fix the reported local error and run sync to finish removal.

an interrupted project command can leave an owner record. only delete the exact owner path named by a waiting command after confirming that its owning process has ended. records from another operating system are never reclaimed by age or pid. [project state](reference/cache-folder.md#the-fetchloom-folder) explains the locking rules.

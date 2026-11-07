# troubleshooting

error text, exit codes and local status distinguish missing inputs from changed bytes and access problems.

```sh
fetchloom status
fetchloom -v sync
```

verbose output adds error kinds and work counters. use `--json` for [structured failures](reference/json-output.md), including every failed file in a download batch.

## collect a debug report

run the failing command again with `-vv` and save stderr, then add the version line:

```sh
fetchloom -vv sync 2> fetchloom-debug.txt
fetchloom --version -v >> fetchloom-debug.txt
```

on windows, run these lines in cmd or powershell 7; windows powershell 5.1 rewrites redirected stderr. attach `fetchloom-debug.txt`, the exit code, and the `data.toml` and `data.lock` entries for the dataset. debug lines say whether each file came from the cache or a download, why a placed file was hashed again, which placement method was used and why a better one was given up, which host a stored credential was sent to, and each retry and redirect.

logs never include a token or password: a credential is named by its host and header only, and a login or secret query value in a link shows as `***`. local paths and dataset references do appear, so read the file before sharing it. `FETCHLOOM_LOG=fetchloom::net=debug` limits the log to one part of fetchloom when the full log is too long.

## errors

| symptom on stderr | cause | fix |
|---|---|---|
| `fetchloom: command not found`, or `fetchloom is not recognized` on windows | the terminal started before rust added its folder to PATH | open a new terminal; if that fails, check `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on windows) holds fetchloom |
| `error: there is no command called "init"` | no setup command is needed | run `fetchloom add <link>` in the project folder |
| `error: <link> is missing` or `error: <link>... is missing` | `add` or `get` needs a reference | pass a dataset link, or a list to `get -i`; use `fetchloom get --help` for syntax |
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
| `warning: <path> was edited, and the file is shared with other projects, so they see the edit too` | the file is a hardlink to the cache, so an edit reaches every project using it | save the edit, then run `fetchloom sync --force` in each project; use `copy = true` for files you edit |
| `warning: iris was taken, so this one is iris-2` | the project already has a dataset with that name | keep it, or run `fetchloom remove iris-2` and add again with `--as <name>` |
| status exits 0 with differences | differences are a successful result | inspect all nine status arrays |
| an input is read-only | default placement protects shared bytes | set `copy = true` on the dataset and sync |
| a glob matches no files | it differs from listed paths or targets archive members | quote the glob and check [pattern rules](reference/data-toml.md#patterns) |
| adding the same reference rejects different filters | the existing entry already owns that reference and settings | edit `data.toml` or pass an unused `--as` name |
| an archive remains packed | the extension or bytes are unsupported, or extraction is disabled | check [archive rules](concepts.md#archives) |
| an archive has no dataset folder | it contains no regular files | use `extract = false` and sync to keep the packed archive |
| a source returns an access refusal | access is unavailable or unsupported | check the source's [credentials and limits](sources/README.md) |
| cache registration warns after sync | the cache could not record the project root | fix cache permissions and sync again before relying on gc retention |
| `cache gc` exits 50 after partial cleanup | some paths could not be removed or inspected | inspect `result.not_removed` causes; fix access or storage before retrying |
| `cache verify` exits 30 | cached bytes changed | allow a later online sync to replace damaged objects, then sync affected projects |
| `cache verify` exits 50 without changed bytes | objects, metadata or directory entries could not be read | inspect `result.unreadable` causes, fix access or storage and verify again |
| a windows file cannot be replaced or deleted | a program holds it open, or permissions block access | close the program, check permissions and retry |
| a long path fails | the filesystem or another program may have a shorter limit | follow the shorter-project-path hint; use shorter project and dataset folders |
| plain-word add or get refuses to guess | no interactive dataset choice is possible | choose a returned canonical id; `--yes` never selects one |
| an option is rejected before a command | command-specific options belong after that command | use `fetchloom add --as flowers zenodo:3608990`, not `fetchloom --as flowers add zenodo:3608990` |
| a download warns that it is a web page | the URL may name a page rather than data | find the source's download or raw link |

to make iris writable, edit its existing entry in `data.toml` and sync:

```toml
[datasets.iris]
ref = "zenodo:3608990"
copy = true
```

```sh
fetchloom sync
```

copied placement inside onedrive can report:

```text
warning: iris is inside onedrive, which breaks links, so the file was copied. a folder outside onedrive lets fetchloom try links
```

move the project outside synced storage if you want fetchloom to try hardlinks. reflinks may still work inside a synced folder. [windows](windows.md) covers drive choice and Defender scanning.

## project recovery

restore a broken or older lock from git when you need its exact pins. deleting it and running `fetchloom lock` can resolve newer inputs. a newer lock version requires a newer fetchloom.

a failed `add` attempts to restore both `data.toml` and `data.lock`. if either restoration fails, follow the named file's recovery details and decode its backup JSON string. cached downloads and files already placed can remain.

if `remove` changed metadata but could not delete files, fix the reported local error and run sync to finish removal.

an interrupted project command can leave an owner record. only delete the exact owner path named by a waiting command after confirming that its owning process has ended. records from another operating system are never reclaimed by age or pid. [project state](reference/project-folder.md#owner-record) explains the locking rules.

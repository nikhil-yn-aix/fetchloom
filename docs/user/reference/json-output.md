# JSON output

`--json` returns one versioned document on stdout for scripts to read.

```sh
fetchloom --json lock --check
fetchloom --json --yes sync
fetchloom --json status
```

`--json` prints one document on stdout, also for usage errors, the welcome, the version and help: `fetchloom help --json` describes every command, and `<command> --help --json` only that one, in the `help` result below. a text topic, such as `help exit-codes --json`, returns `result.topic` and `result.text`. stderr gets only debug logs you asked for, and with `--progress`, plain progress lines after 2 s and then every 10 s. JSON mode never asks a question; use `--yes` when approval is needed.

a result JSON cannot represent, such as a path that is not UTF-8, returns a complete `local.io` error document instead, exit 50, and so does a failure to write stdout, though what the command did stays done. a reader that closes its pipe early is no failure.

| field | value |
|---|---|
| `schema` | integer `1` |
| `command` | `add`, `remove`, `drop`, `sync`, `lock`, `build`, `status`, `list`, `export`, `push`, `search`, `path`, `get`, `cache`, `cache gc`, `cache verify`, `settings`, `auth`, `auth login`, `auth logout`, `completions`, `help`, or null. `completions` with `--json` is always a usage error, since a script is not JSON |
| `status` | `ok` or `error` |
| `result` | command result on success; also retained for cache verification findings, gc removal failures, a `cache gc --inputs` that needs a yes, search candidates, `get` runs of several links, what `export` found, what a `push` planned when the cache lacks files or it needs a yes, what a `build` did before it failed, and a `lock` whose merge needs a choice; null for other failures |
| `warnings` | array of strings, including in quiet mode |
| `errors` | empty on success; one error object on failure |
| `errors[].kind` | one of the names in [exit codes](exit-codes.md) |
| `errors[].message` | summary string |
| `errors[].details` | array of supporting strings |
| `errors[].help` | string or null |
| `errors[].fix` | the first runnable `fetchloom` command that `help` names, without placeholders, or null. quoted arguments, including folders containing spaces, stay intact. an agent or script can run it as the next step. an example (`as in`), a guess (`did you mean`), a command meant for a person's own terminal, one that first needs a broken file deleted or the failures listed above fixed, as the one error for several failed downloads asks, or one that pins other bytes than data.lock does, such as `lock --upgrade` after a source changed a pinned file, is never a fix, so those leave it null. in a run with `--locked`, a command that may write data.lock, such as `fetchloom lock`, is no fix either, whatever stopped the run, so a script that runs `fix` never undoes `--locked`; `help` still names it for a person. a command line holding a link with a query, a fragment or a login is never repeated, so its help says to run the same command and `fix` is null |
| `errors[].failures` | present for file download failures and for `get` runs of several links: every failed file or link, in job or command line order, with `file`, `kind`, `message`, `details` and `help` |
| `work` | counters below, present on success and failure |

batch download errors keep every file in `failures`, even though the human summary names at most ten. each file keeps its own kind, message, details and help. the overall kind and exit code come from the first failed file in job order, not completion order or severity. interruption and other failures that stop the whole batch return immediately instead.

SHA-256 mismatch details give each hash whole with `--json`. a person, `-v` included, reads each cut to its first 12 hex digits, or to just past the first digit where the two differ. parse `kind` and result fields rather than human messages.

bare `fetchloom` inside a project reports command `status`. outside a project, and for `--version`, command is null and result is `{"version":"0.1.0"}`. on a usage error, command is the prefix clap recognized, or null. `--json` is recognized anywhere before `--` on that error path; other display flags depend on what clap read.

## command results

| command | result fields |
|---|---|
| `add`, `remove`, `sync`, `lock` | `project`: absolute path; for `sync`, `selected`: the dataset names it was given, or null when it was given none; `datasets`: name array; `lock_changed`: boolean; `placement`: object below or null; with `--dry-run`, also `dry_run` and `changes`, [below](#dry-runs); for a `lock` that merged both sides of a git merge, also `merge`, below |
| `drop` | `project`: absolute path; `dropped`: one `{path, dataset, archive, pattern}` record for each file left out, by project-relative path, with `archive` the path in data.lock of the archive the file is inside, or null for a file the source lists, and `pattern` the one written for it; `copied`: one `{dataset, archive, from}` record for each archive drop gave a members table of its own as a copy of the table under the glob key `from`; `lock_changed`: boolean; `placement`: object below; with `--dry-run`, also `dry_run` and `changes`, [below](#dry-runs); [drop](../commands/drop.md#json) |
| `build` | `project`: absolute path; `selected`: the recipes named, or every recipe; `datasets`: the datasets whose pins changed; `lock_changed`; `placement`, as sync reports it; `steps`: one record a step in build order, with `name`, `action` (`ran`, `reused`, `fetched`, `up_to_date`, `accepted`, `skipped` or `prepared`), `key` and `tree`, null with `--prepare` where not known yet, `files`, `bytes`, nullable `check` with `matching`, `changed`, `missing` and `extra`, nullable `candidate`, the tree of a rebuild that differs, kept for `--accept`, nullable `log` and nullable `seconds`, kept beside a `build.failed` or `build.differs` error for the steps before it and the one that differs. with `--dry-run`: `project`, `selected`, `dry_run`: `true`, and `steps` with `name`, `action` (`would_run`, `would_fetch`, `would_reuse`, `may_run` or `up_to_date`; each but `may_run` the one the build then reports, `ran`, `fetched`, `reused` or `up_to_date`), nullable `key`, `changes`, `run`, `env`, `inputs` of `name` and `folder`, `code` and `output`; [build](../commands/build.md#json) |
| `get` | `file`: the absolute path of the file `-o` wrote or found, else null; `folder`: absolute path, the file's folder with `-o`, or null for several links; `files`: file records below, empty for several links; `items`: one record for each link, below |
| `search` | `candidates`: records [below](#search-candidates) |
| `list` | `datasets`: records with `name`, `ref`, null for a dataset a recipe makes, `from`, the datasets a recipe makes it from or null for a fetched one, nullable `resolved`, nullable `files` and `bytes`, the files the dataset puts in its folder as `list` counts them, project-relative `folder`, `placed`, false for a dataset with `place = false`, whose `folder` holds none of its files, nullable `title`, `license` and `doi`, nullable `access` (`public`, `login`, `terms` or `unknown`, as the source said), and nullable `locked_files`, an array of `path`, `size`, `sha256` and `url` records, `url` null for a link holding a login, a secret or a control character; `locked_files` describes locked source files, an archive as itself, or the files of a recipe's pinned result, whose `url` is null. `list <name>` returns only that dataset |
| `export` | `file`: the absolute path `-o` wrote, or null; `placed`: a record for each file in the data folder, archive members included, with `dataset`, project-relative `path`, `size`, `sha256` and `unpacked_from`, the archive's path in its dataset or null; `sources`: a record for each file data.lock pins as downloaded or imported, with `dataset`, `path` in the dataset, `size`, `sha256` and `url`, null for an imported file or a link that is not written; `unlocked`: dataset names data.lock does not pin as data.toml asks; `not_unpacked`: project-relative paths of archives whose files this machine does not know; `no_link`: `<dataset>/<path>` of files imported from a folder, which have no link, and of files whose link holds a login, a secret or a control character; `left_out`: a record for each dataset the chosen `--format` leaves out, with `dataset` and `reason`, `recipe` or `imported` for `links` and `aria2` and `not_placed` for `sha256`, which a text form also names in one warning. without `-o` it lists what is known and exits 0; with `-o` it fails as the text forms do. [export](../commands/export.md#what-it-leaves-out) |
| `push` | `target`: the mirror's absolute folder, with any `..` resolved; `started`: whether the push started a mirror there; `selected`: the names given, or null; `datasets`: every dataset pushed; `objects` and `bytes`: the files those datasets need on the mirror, each once, and their bytes; `planned` and `planned_bytes`: the files of those the push set out to write, as the mirror lacked them or held a damaged copy; `copied` and `copied_bytes`: the files written; `present` and `present_bytes`: the files found there after one look each; `replaced`: how many of `copied` took the place of a damaged copy; `trees`: the lists of archives' files written; `asks`: a `{dataset, access}` record for each dataset that asks first. an error before anything is written, such as `offline.missing`, `network.refused`, `unconfirmed` or a stop, keeps it, with `copied`, `replaced` and `trees` at 0 and the plan in `planned`. with `--dry-run`, also `dry_run` and `changes`, [below](#dry-runs); [push](../commands/push.md#json) |
| `path` | `path`: the absolute path of the dataset, file or subfolder; a recipe that changed since its build adds a warning saying what changed |
| `status` | `project`: absolute path; the arrays below |
| `cache` | `path`: configured cache root; `objects`: count; `bytes`: payload bytes; `needed`: cached payload bytes needed by registered locks; `linked`: of `needed`, the bytes of files that also have a name outside the cache, such as a project's hardlink, so deleting them would free nothing; `unneeded`: payload bytes no registered lock needs; `freeable`: of `unneeded`, what `cache gc` frees now: files at least a day old with no other name; `copies`: bytes of files in registered projects that are separate copies of cached files, placed as copies or clones, or placed as hardlinks that lost their link; `unlinked_copies`: of `copies`, those placed as hardlinks; `cloned`: of `copies`, the clones, which share blocks with the cache's copy and take no room of their own until either changes, so `copies - cloned` is what takes room twice; `inputs_only`: of `needed`, the bytes only builds read: files of datasets with `place = false` that every recipe reading them was built from; `projects`: root-path array; `unfinished_downloads`: downloads kept under `partial/` to continue later; `unfinished_bytes`: the disk space they take. byte counts are estimates in payload bytes, [cache](../commands/cache.md#inspect) |
| `cache gc` | `removed_objects`: removed object count; `removed_bytes`: their payload bytes; `kept_objects`: unneeded objects retained because they are less than a day old; `kept_bytes`: their payload bytes; `removed_leftovers`: removed partial or temporary file count; `removed_leftover_bytes`: the disk space they took; `not_removed`: failure records for paths that could not be removed or inspected for cleanup; `forgotten`: root paths of projects whose registration gc removed because their `data.toml` is gone; with `--inputs`, also `inputs`: `freed_objects` and `freed_bytes`, the raw input freed, each file once and apart from `removed_objects`, `datasets`, a `{project, name, objects, bytes, route: {mirrors}, costs: {recipes}}` record for each dataset freed, and `kept`, a `{project, name, objects, bytes, reason, mirror}` record for each dataset and reason it kept files for, as [raw input a mirror holds](../commands/cache.md#raw-input-a-mirror-holds) lists them; `unconfirmed` keeps it, with what a yes would free; with `--dry-run`, also `dry_run` and `changes`, [below](#dry-runs) |
| `auth` | `accounts`: records with `account`, `signed_in`, `from`, `user` and `valid`; [auth](../commands/auth.md#json) |
| `auth login`, `auth logout` | `account` with `user` and `saved_to`, or `removed_from` |
| `settings` | `settings`: one record for each setting, with `name`, `value`, `origin` (`environment`, `file`, `machine`, `default` or `flag`, and for mirrors that `--mirror` and a setting both give, `flag,environment` or `flag,file`) and `variable`; `machine`: `processors`, `memory`, the bytes available, null off linux, and `open_files`, null where the system sets no limit; `file`: `path` of the settings file, null when there is no folder for one, and `found`. [settings](../commands/settings.md#what-it-shows) |
| `help` | `options`: the options fetchloom itself takes, which are those every command takes and `--version`; `commands`: each command with `name`, `about`, `usage`, `arguments` and `examples`. each argument has `name`, `short`, `value`, `help`, `required` and `repeats` |
| `cache verify` | `objects`: examined count; `bytes`: examined payload bytes; `changed`: hash array; `stuck`: hashes whose change marks could not be written; `unreadable`: failure records for contents, metadata or directory entries that could not be read |

project `datasets` names the dataset added by `add` or removed by `remove`. for `sync` and `lock`, and for their dry runs, it names the datasets whose pins changed, or would change: pinned for the first time, pinned to another version or other files, or no longer in `data.lock`. a [rename](data-toml.md#dataset-names) names its new name and its old one, which data.lock no longer has; a `sync` given names changes no other dataset's pin, so its `datasets` holds only names from its `selected`, a renamed one's old entry staying, and its `placement` counts only their files. an entry whose `data.toml` settings changed without changing what it pins is not named. a duplicate `add`, a `sync` that changes no pin and a successful `lock --check` return an empty array. `remove` always reports `lock_changed = true`, including when the removed dataset had no lock entry. `lock` has null placement, and a successful `lock --check` has `lock_changed = false`.

`merge` has the dataset-name arrays `from_ours` and `from_theirs`, taken from our side or their side as git names them, the checked-out branch and the merged one in a merge, `same`, `removed` and `unpinned`, the fetched datasets in the merged `data.toml` with no pin of either side that answers them; a recipe neither side has built is left out of it, since only `fetchloom build` pins one. `datasets` then names the pins that changed from the checked-out branch's. a merge that needs a choice fails with `unconfirmed` and keeps `result.project` and `result.merge`, whose `conflicts` names each dataset to choose a side for with `--ours` or `--theirs`; its `fix` is null. [after a git merge](../commands/lock.md#after-a-git-merge) has the rules.

placement has integer counts `placed`, `replaced`, `removed` and `unchanged`; `kept`, an array of `{path, reason}`; `overwritten`, an array of `{path, reason}` for the files `--force` replaced that sync would otherwise keep, with the reason it would have kept each; and `restored`, an array of replaced files no edit of the project's explained: hardlinks that were the cache's copy of their bytes, or the copy the cache found changed, after an edit through a link, here or in another project, changed it, and files hashing every placed file found changed under an unchanged size and time; and `changed_copies`, an array of `{path, kept_as}` naming, for each restored link to the cache's changed copy, the file beside it that keeps the changed bytes; and `put_back`, an array of files fetchloom placed that someone deleted or moved, which it placed again since data.lock pins them, also counted in `placed`; and `unbuilt`, an array of the recipes whose pinned results the run left out, since neither the cache nor a mirror holds them and only `fetchloom build` makes them. a recipe not built yet, or changed since its build, is no difference in placement: sync places the result data.lock pins, if any, and adds a warning saying what changed. `removed` counts the files of a dataset with `place = false` sync removed too. paths are project-relative. `kept` and `overwritten` reasons are `changed` for edited managed files, `shared` for visibly changed hardlinks shared with other projects whose shared copy the cache no longer knows, `foreign` for other bytes at a target, `unneeded` for edited managed files no longer wanted, and `blocked` for targets another entry stands in the way of, whose path that entry's record adds as `blocked_by`. adopted identical files count as unchanged; removing one later adds a warning that it was already there when sync adopted it. unrelated extra files are not included in `kept`.

`get.files` lists placement targets, so an unpacked archive is replaced by its members. each record has `path`, `size`, `sha256`, `url`, `origin`, `method`, `unpacked_from`. the path is relative to `folder`. origin is `network`, `not_modified` or `cache`; method is `reflink`, `hardlink`, `copy`, `move`, or null when already present. `move` is a file `-o` took straight from a download nothing else needs, moved out of the cache, so it counts under none of `reflinks`, `hardlinks` and `copies`. `unpacked_from` is the selected archive path or null. unpacked members inherit the archive's URL and origin. with `-o`, the one record's path is the file's name. a file `-o` found holding the expected bytes before any request, by `--sha256`, a hash the source states or, offline, the link's last download, has origin `cache` and method null; one the server said is unchanged has origin `not_modified` and method null.

`get.items` has one record for each link, in the order given on the command line and then in the `--input-file` list. each has `reference`, the link as given with a login and secret query values masked; `status`, `ok` or `error`; `folder`, the link's absolute folder or null; `files`, file records as above, empty on failure; and `error`, null on success or an object with `kind`, `message`, `details`, `help` and, for failed downloads, `failures`, in the form of `errors[]`. one link without `-i` keeps `folder` and `files` at the top as well, and returns a null `result` on failure. several links, or any `-i` list, give a null top-level `folder` and empty `files`. when some of them fail, the run returns `error` with `result` still set: `errors[0].kind` and the exit code come from the first failed link in that order, and `errors[0].failures` names each failed link in `file`. an interruption, a refused question about the total size and a cache that cannot open return immediately with a null `result`.

status arrays, each a list of project-relative paths or dataset names, as [status](../commands/status.md#categories) explains:

| field | entries |
|---|---|
| `unlocked` | datasets data.lock does not pin as data.toml asks, and recipes whose result is not pinned as building them now would make |
| `removed` | datasets in data.lock but no longer in data.toml |
| `candidates` | recipes with a kept rebuild that differs from data.lock, each with `name`, `tree`, `stale`, true when `--accept` would refuse it, and `files`, each with `path`, `difference` (`changed`, `missing` or `extra`), and nullable `file` and `pinned`, the cache files of the rebuild's bytes and of the pinned ones |
| `missing` | pinned files that are absent |
| `edited` | files fetchloom placed that were changed |
| `untracked` | files fetchloom did not place, at pinned paths, with other bytes than the pins |
| `adoptable` | files fetchloom did not place, at pinned paths, holding the pinned bytes, which sync adopts |
| `outdated` | unchanged files that sync replaces: pinned to other bytes now, or read-only where the dataset asks for `copy = true` |
| `damaged` | files fetchloom placed that hold other bytes though no edit of the project's shows, as a hardlink an edit through another link changed |
| `blocked` | entries in a pinned file's way |
| `extra` | files no dataset places, which sync keeps |
| `unneeded` | files fetchloom placed that no dataset needs now, which the next sync removes |
| `not_unpacked` | archives whose files this machine does not know |
| `unbuilt` | recipes whose pinned result is on no machine or mirror here, which sync leaves out |
| `unplaced` | datasets with `place = false`; no difference |

JSON lists every difference, as rows in a pipe do, while a terminal shows 20 rows of each kind of file.

`status` finding differences and `sync` keeping conflicts still return `ok` and exit 0. inspect the arrays. `cache verify` finding changed bytes returns `integrity.mismatch` and exit 30; when no bytes changed but an object or directory could not be read, it returns `local.io` and exit 50. both errors retain the verification counts and findings in `result`, alongside `errors`. unreadable object and directory paths are also named in `warnings`; unreadable objects are neither marked nor removed. counts include only objects whose metadata could be read, so an unreadable directory can hide objects from the totals. successful `changed`, `stuck` and `unreadable` arrays are empty.

`cache gc` keeps objects no project needs until they are a day old by modification time, unless `--now` removes that grace period. unfinished downloads keep a day too, unless `--now` removes every one no running fetchloom holds. `kept_objects` and `kept_bytes` count those young objects, not objects retained for registered projects.

`cache gc` uses `local.io` and exit 50 when a removal fails, including after other removals succeeded. this kind also covers partial cleanup failures. `result` retains the successful removal counts and `not_removed` failure records; each contains `path`, `kind` (the I/O error kind), `os_code` (the OS code or null) and `message` (the original cause). `cache verify` uses the same records in `unreadable`. human output, error details and JSON `warnings` retain those causes. a failed stale-project registration removal is included in `not_removed`, with successful object-cleanup counts retained. a path already absent is not a failed removal.

successful sync still exits 0 if its cache registration cannot be written. human output and JSON `warnings` explain that gc may not keep the project's files and ask you to check cache permissions and run the command again.

## dry runs

`add`, `remove`, `drop`, `sync`, `lock`, `push` and `cache gc` with `--dry-run` return their usual result, counting what would happen, plus `dry_run: true` and a `changes` object listing what would change. without `--dry-run` neither field is present.

for `add`, `remove`, `drop` and `sync`, `lock_changed` says whether `data.lock` would change and `placement` counts what placing would do. `changes` has these arrays of project-relative paths:

| field | entries |
|---|---|
| `place` | files that would be placed where nothing is |
| `replace` | files that would be overwritten, also counted in `placement.replaced` |
| `remove` | managed files no dataset needs that would be deleted |
| `download` | files that would be downloaded first, as `{path, size}` with `path` as `<dataset>/<file>` and `size` in bytes, or null when only the download can tell |
| `unpack` | archives, as `<dataset>/<file>`, that would be unpacked first; their members are not in `place` or the counts |

a file already standing where a file goes whose hash only its download can learn is in neither `place` nor `replace`, nor in the counts: only that download, listed in `download`, tells whether it holds the pinned bytes.

`result` of `fetchloom --json sync --dry-run` in a fresh clone whose locked file is already in the cache, from a real run with the project path shortened:

```json
{"project":"/home/me/study","selected":null,"datasets":[],"lock_changed":false,"placement":{"placed":1,"replaced":0,"removed":0,"unchanged":0,"kept":[],"restored":[],"overwritten":[],"changed_copies":[],"put_back":[],"unbuilt":[]},"dry_run":true,"changes":{"place":["data/iris/Iris.csv"],"replace":[],"remove":[],"download":[],"unpack":[]}}
```

for `lock`, `lock_changed` and `datasets` say whether `data.lock` would change and which pins, as far as is known without downloading; `placement` is null. `changes.download` lists the files locking would download first, to learn a hash or, with `--upgrade`, to check a file no checksum pins, which the dry run counts as unchanged until then. `changes.unpack` lists the archives it would unpack to learn what they hold. `place`, `replace` and `remove` are empty.

for `push`, the counts say what it would write, and `changes` has `copy`, a `{path, sha256, size}` record for each file it would copy into the mirror, `path` being the first `<dataset>/<file>` that needs it, `trees`, the same for each list of an archive's files, `path` naming the archive; `download`, a `{path, size}` record for each file it would download first, as `<dataset>/<path>` in data.lock; and `unpack`, the archives among them. it downloads and writes nothing, starts no mirror and asks nothing.

for `cache gc`, `removed_objects`, `removed_bytes`, `kept_objects`, `kept_bytes`, `removed_leftovers` and `removed_leftover_bytes` count what gc would remove and keep, `inputs` with `--inputs` counts the raw input it would free, and `not_removed` lists failures for paths it could not inspect. `changes.remove` lists the path of each object, raw input among them, unpack record, leftover and project registration gc would delete, under the cache folder as `FETCHLOOM_CACHE` gives it, so a relative cache gives relative paths. a gc dry run returns `ok` even when `not_removed` is not empty.

the work counters still count metadata requests a dry run makes to resolve changed entries. `objects_written`, `reflinks`, `hardlinks`, `copies`, `placed_from_cache`, `data_folder_objects` and `data_folder_bytes` stay 0.

## search candidates

`search` returns `candidates` on success. plain-word `add` and `get` retain the same array when they fail with `reference.ambiguous`; no matches returns an empty array with `reference.unresolved`.

| field | value |
|---|---|
| `reference` | canonical reference string to pass to add or get |
| `title` | catalog title, or the repository id for hugging face |
| `source` | `hugging face`, `kaggle` or `zenodo` |
| `size` | byte count as the site lists it, which for kaggle is the zipped download; null when unknown |
| `downloads` | catalog download count, or null |
| `note` | access note, or null |

[catalog search](../commands/search.md) defines ordering and interactive selection. `list.datasets` instead reflects the manifest and lock, with null lock-derived values when a dataset has no lock entry; a stale entry can still supply values.

## work counters

all counts and byte quantities are unsigned integers. timestamps and large counters can exceed javascript's exact integer range.

| field | counts |
|---|---|
| `requests` | explicit metadata or file request attempts, including retries, counted as each is sent; redirect hops are not counted separately, and a request refused before it is sent, as in [mirror-only mode](../mirrors.md#mirror-only-mode), is not counted |
| `bytes_down` | body bytes read, including metadata and unsuccessful attempts; excludes headers and the saved resume prefix |
| `retries` | scheduled retry waits |
| `resumes` | accepted nonzero range starts |
| `objects_written` | new objects counted after successful downloads, copies from a mirror or the data folder, archive unpacking and the files a recipe's step wrote |
| `downloads_skipped` | downloads not made because the cache already held the bytes, as the link's earlier record or a stated MD5 showed; a download placing already knows the cache holds is never started, so it is not counted here |
| `reflinks`, `hardlinks`, `copies` | new placements by each method, including replacements |
| `mirror_objects`, `mirror_bytes` | pinned files a [mirror](../mirrors.md) gave this run, now in the cache, and their bytes. a file the cache already held is never asked for |
| `data_folder_objects`, `data_folder_bytes` | pinned files the cache lacked that this run copied in from the project's data folder, at paths data.lock places them at, each after its bytes hashed to its pin, and their bytes, as [moving a project](../commands/cache.md#moving-a-project-to-another-machine) describes. a mirror is asked only for what the data folder did not give |
| `placed_from_cache` | placements by `add`, `sync`, `path` and `get` from objects the run did not write to the cache itself; the other placements took bytes the run downloaded, unpacked or copied in from a mirror or the data folder. file times do not decide it, so it holds on filesystems that keep times in whole seconds and on file servers with their own clocks |
| `elapsed_ms` | elapsed whole milliseconds before rendering the final report |

`-v` ends with the same counters in words, as in `1 request · 47.7 MiB down · 1 hardlink · 1 placed from the cache · 2.0 s in all`. `1 file written to the cache` counts a write during the run, so a one-off `get -o` that moved its file out of the cache afterwards still shows it. there are no phase timings or bytes-written counters. cache verification reports directory and metadata read failures in `result.unreadable` rather than claiming those paths were verified.

a killed process may leave no valid JSON. treat a missing or invalid document as a failure. [scripting](../scripting.md) shows checks for exit codes, schema versions and preserved conflicts.

# JSON output

`--json` returns one versioned document on stdout for scripts to read.

```sh
fetchloom --json lock --check
fetchloom --json --yes sync
fetchloom --json status
```

`--json` prints one document on stdout, also for usage errors, the welcome, the version and help: `fetchloom help --json` describes every command, and `<command> --help --json` only that one, in the `help` result below. a text topic, such as `help exit-codes --json`, returns `result.topic` and `result.text`. stderr gets only debug logs you asked for, and with `--progress`, plain progress lines: at once for a download or a lookup, after 2 s for other work, then every 10 s. JSON mode never asks a question; use `--yes` when approval is needed.

in a pipe or a file the document is one line. when stdout is a terminal, it is indented two spaces a level, so a person can read it; it is the same document, with no color, so read all of stdout rather than its first line. either way the document is ASCII alone: every character past it, in any string or key, is written as a `\u` escape, such as `\u6570` for 数, and one past U+FFFF as two, so a reader that decodes stdout with another code page, as windows powershell 5.1 does with the console's, reads the same text, and no c1 control or bidi control reaches a terminal raw.

a result JSON cannot represent, such as a path that is not UTF-8, returns a complete `local.io` error document instead, exit 50, and so does a failure to write stdout, though what the command did stays done. a reader that closes its pipe early is no failure.

| field | value |
|---|---|
| `schema` | integer `1` |
| `command` | `add`, `remove`, `exclude`, `drop`, `sync`, `lock`, `status`, `audit`, `diff`, `list`, `export`, `push`, `search`, `path`, `get`, `cache`, `cache gc`, `cache verify`, `settings`, `settings set`, `settings unset`, `auth`, `auth login`, `auth logout`, `completions`, `help`, or null. `completions` with `--json` is always a usage error, since a script is not JSON |
| `status` | `ok` or `error` |
| `result` | command result on success; also retained for cache verification findings, gc removal failures, search candidates, `get` runs of several links, what `export` found, what a `push` planned when the cache lacks files or it needs a yes, and a `lock` whose merge needs a choice; null for other failures |
| `warnings` | array of warning objects, including in quiet mode: each has `kind`, `message`, `details` and `help`, as an error does; [warning kinds](#warning-kinds) lists every kind |
| `errors` | empty on success; one error object on failure |
| `errors[].kind` | one of the names in [exit codes](exit-codes.md) |
| `errors[].message` | summary string |
| `errors[].details` | array of supporting strings |
| `errors[].help` | string or null |
| `errors[].fix` | the first runnable `fetchloom` command that `help` names, without placeholders, or null. quoted arguments, including folders containing spaces, stay intact. an agent or script can run it as the next step. it is only ever a command fetchloom wrote for that step: one spelled out between backticks inside a name, a path or a value from data.toml, data.lock, a server or a file name never is, nor is the command that just failed, given back to run again, and a help that quotes either before fetchloom's own has no `fix`. an example (`as in`), a guess (`did you mean`), a command meant for a person's own terminal, such as `auth login`, which waits for a person to paste a key, one of the ways a help offers with `or`, as in ``run `fetchloom add ./flowers` to pin it, or `fetchloom search flowers` to search``, or one that comes after a step a person takes first, as a help's `then run` or `and run` says, such as a broken file mended or deleted or the failures listed above fixed, as the one error for several failed downloads asks, is never a fix, so those leave it null. nor is a command that deletes, overwrites or repins what you keep or answers a question with `--yes`: `remove`, `exclude`, `cache gc`, `auth logout`, `sync --force` or `--verify`, `lock --upgrade`, `--ours` or `--theirs`, `get` or `export` with `--force`, and `add` or `get` with `--web-page`, which only a person can say a page is the data for. nor is one that downloads, writes outside the project or publishes: `add`, `get`, `sync`, `path`, `lock`, `push` and `export -o`, which may name a file anywhere, but for their `--dry-run` forms and `lock --check`. a help that names one of those first has no `fix` at all, since what it names next is often the step after it; `help` still names each choice for a person, and their `--dry-run` forms are fixes. in a run with `--locked`, a command that may write data.lock is no fix either, whatever stopped the run, so a script that runs `fix` never undoes `--locked`; `help` still names it for a person. a command line holding a link with a query, a fragment or a login is never repeated, so its help says to run the same command and `fix` is null |
| `errors[].failures` | present for file download failures and for `get` runs of several links: every failed file or link, in job or command line order, with `file`, `kind`, `message`, `details` and `help` |
| `work` | counters below, present on success and failure |

batch download errors keep every file in `failures`, even though the human summary names at most ten. each file keeps its own kind, message, details and help. the overall kind and exit code come from the first failed file in job order, not completion order or severity. an interruption returns immediately instead. a failure that stops the whole batch, as a server that stops answering does, ends the downloads still running and keeps the files that failed before it in `failures`.

error details give each SHA-256 whole with `--json`. a person reads two hashes compared in one detail whole, one under the other, and any other hash cut to its first 12 hex digits, or to just past the first digit where it differs from another on the line. parse `kind` and result fields rather than human messages.

bare `fetchloom` inside a project reports command `status`. outside a project, and for `--version`, command is null and result is `{"version":"0.1.0","commit":"…"}`: `commit` is the git commit the build came from, 40 hex digits, or null for a build outside git. on a usage error, command is the prefix clap recognized, or null. `--json` is recognized anywhere before `--` on that error path; other display flags depend on what clap read.

## Command results

| command | result fields |
|---|---|
| `add`, `remove`, `sync`, `lock` | `project`: absolute path; for `sync`, `selected`: the dataset names it was given, or null when it was given none; `datasets`: name array; `lock_changed`: boolean; `placement`: object below or null; with `--dry-run`, also `dry_run` and `changes`, [below](#dry-runs); for a `lock` that merged both sides of a git merge, also `merge`, below; for a `sync` that could not get every file, the error's document keeps this result, with `waiting`, the datasets that wait for files, as [sync](../commands/sync.md) says |
| `drop` | `project`: absolute path; `dropped`: the names freed; `already`: names dropped before; `absent`: names of which nothing is on this machine, when any; `files`: files removed from the data folder; `objects`: cache files freed; `bytes`: what they took; `placement`: object below; `not_removed`: cache files whose removal failed, when any; with `--dry-run`, `dry_run: true` and the counts it would free. a refusal is `data.not_ready` for a file no mirror or source has, else the error a file that could not be checked met, such as `network.unreachable` or `offline.missing`, else `unconfirmed` for files checked by size alone, which `--yes` lets go, with each file in `details` |
| `exclude` | `project`: absolute path; `excluded`: one `{path, dataset, archive, pattern}` record for each file left out, by project-relative path, with `archive` the path in data.lock of the archive the file is inside, or null for a file the source lists, and `pattern` the one written for it; `copied`: one `{dataset, archive, from}` record for each archive exclude gave a members table of its own as a copy of the table under the glob key `from`; `lock_changed`: boolean; `placement`: object below; with `--dry-run`, also `dry_run` and `changes`, [below](#dry-runs); [exclude](../commands/exclude.md#json) |
| `get` | `file`: the absolute path of the file `-o` wrote or found, else null; `folder`: absolute path, the file's folder with `-o`, or null for several links; `files`: file records below, empty for several links; `items`: one record for each link, below. with `--dry-run`, `dry_run: true` and `changes`, as for `sync`, where `place` and `replace` are paths relative to the current folder; `files` is empty and `folder` or `file` says where the files would land |
| `search` | `candidates`: records [below](#search-candidates) |
| `list` | `datasets`: records with `name`, `ref`, nullable `resolved`, nullable `files` and `bytes`, the files the dataset puts in its folder as `list` counts them, project-relative `folder`, nullable `title`, `license` and `doi`, nullable `access` (`public`, `login`, `terms` or `unknown`, as the source said), and nullable `locked_files`, an array of `path`, `size`, `sha256` and `url` records, `url` null for a link holding a login, a secret or a control character; `locked_files` describes locked source files, an archive as itself. a dataset whose file list data.lock.d lacks has `waiting`, the project-relative part that is not there. `list <name>` returns only that dataset |
| `export` | `file`: the absolute path `-o` wrote, or null; `placed`: a record for each file in the data folder, archive members included, with `dataset`, project-relative `path`, `size`, `sha256` and `unpacked_from`, the archive's path in its dataset or null; `sources`: a record for each file data.lock pins as downloaded or imported, with `dataset`, `path` in the dataset, `size`, `sha256` and `url`, null for an imported file or a link that is not written; `unlocked`: dataset names data.lock does not pin as data.toml asks; `not_unpacked`: project-relative paths of archives whose files this machine does not know; `no_link`: `<dataset>/<path>` of files imported from a folder, which have no link, and of files whose link holds a login, a secret or a control character; `left_out`: a record for each dataset the chosen `--format` leaves out, with `dataset` and `reason`, `imported` for `links` and `aria2`, `dropped` for `sha256`, and `waiting`, for any form, for one whose file list data.lock.d lacks, which a text form also names in one warning; `citations`, for `bibtex`, `csl` and `ris`, a record for each dataset cited, with `dataset`, `from` (`doi.org`, or `data.lock` alone), `unasked`, why doi.org gave nothing, left out when it gave a record, `type`, the CSL type, `title`, `authors` (`{family, given}` or `{literal}` records), `issued`, the year, month and day as known, `publisher`, `container`, the journal of an article, `version`, `doi`, `url`, `license`, `accessed`, the day it was pinned, and `missing`, what a citation needs that the entry lacks: `author`, `year`, `title` or `link`; empty for other forms. without `-o` it lists what is known and exits 0; with `-o` it fails as the text forms do. [export](../commands/export.md#what-it-leaves-out) |
| `push` | `target`: the mirror's absolute folder, with any `..` resolved; `started`: whether the push started a mirror there; `selected`: the names given, or null; `datasets`: every dataset pushed; `objects` and `bytes`: the files those datasets need on the mirror, each once, and their bytes; `planned` and `planned_bytes`: the files of those the push set out to write, as the mirror lacked them or held a damaged copy; `copied` and `copied_bytes`: the files written; `present` and `present_bytes`: the files found there after one look each; `replaced`: how many of `copied` took the place of a damaged copy; `trees`: the lists of archives' files written; `asks`: a `{dataset, access}` record for each dataset that asks first. an error before anything is written, such as `offline.missing`, `network.refused`, `unconfirmed` or a stop, keeps it, with `copied`, `replaced` and `trees` at 0 and the plan in `planned`. with `--dry-run`, also `dry_run` and `changes`, [below](#dry-runs); [push](../commands/push.md#json) |
| `path` | `path`: the absolute path of the dataset, file or subfolder |
| `status` | `project`: absolute path; the arrays below |
| `audit` | `project`: absolute path; `files`, `served` and `unchecked`: the files data.lock pins for the datasets audited, those a mirror or source serves and those not checked; `datasets`: a record for each with `name`, `files`, `served`, `on_mirrors`, `no_size`, and `not_served`, `unchecked` and `lost_at_source`, each a list of `{file, outcome, size, message, help}` records, `outcome` being `changed`, `gone` or `imported` for a file not served and `refused` or `unknown` for one not checked, `size` the size served for `changed`, and `help` left out when there is none; `newer`: the version `lock --upgrade` would pin in place of data.lock's, or null; `changed`: a `{field, pinned, now}` record for each `title`, `license` or `access` the source states otherwise now; `unlisted`: null, or `{outcome, message, help}` when the source did not say what the dataset holds now, `gone`, `refused` or `unknown`. a failed `--check` keeps it. [audit](../commands/audit.md#json) |
| `cache` | `path`: configured cache root; `objects`: count; `bytes`: payload bytes; `needed`: cached payload bytes needed by registered locks; `linked`: of `needed`, the bytes of files that also have a name outside the cache, such as a project's hardlink, so deleting them would free nothing; `unneeded`: payload bytes no registered lock needs; `freeable`: of `unneeded`, what `cache gc` frees now: files at least a day old with no other name; `copies`: bytes of files in registered projects that are separate copies of cached files, placed as copies or clones, or placed as hardlinks that lost their link; `unlinked_copies`: of `copies`, those placed as hardlinks; `cloned`: of `copies`, the clones, which share blocks with the cache's copy and take no room of their own until either changes, so `copies - cloned` is what takes room twice; `projects`: root-path array; `waiting`: a `{project, dataset}` record for each fetched dataset of a registered project that waits for its file list, whose files `needed` does not count, as gc does not; `unfinished_downloads`: downloads kept under `partial/` to continue later; `unfinished_bytes`: the disk space they take. byte counts are estimates in payload bytes, [cache](../commands/cache.md#inspect) |
| `cache gc` | `removed_objects`: removed object count; `removed_bytes`: their payload bytes; `imported_objects` and `imported_bytes`: the part of those imported from files, which no source can give back; `kept_objects`: unneeded objects retained because they are less than a day old; `kept_bytes`: their payload bytes; `removed_leftovers`: removed partial or temporary file count; `removed_leftover_bytes`: the disk space they took; `not_removed`: failure records for paths that could not be removed or inspected for cleanup; `forgotten`: root paths of projects whose registration gc removed because their `data.toml` is gone; with `--dry-run`, also `dry_run` and `changes`, [below](#dry-runs) |
| `auth` | `accounts`: records with `account`, `signed_in`, `from`, `user`, `valid` and `kind`, which says how AWS keys, google credentials or azure credentials come, as `credential_process` or `az_cli`, or what a website's saved credential is, as `token` or `cookies`, with `expires` for cookies; [auth](../commands/auth.md#json) |
| `auth login`, `auth logout` | `account` with `user` and `saved_to`, or `removed_from` |
| `diff` | `project`; `from` and `to`, each a `revision` as named and the `commit` it names, both null for the project folder as it is now, and `has_lock`, false where that side has no data.lock in the project's folder, so every dataset counts as added or removed; `datasets`, a record for each dataset whose pin differs: `name`, `change` (`added`, `removed` or `changed`), `fields`, a `{name, before, after}` record for each field of data.lock that differs apart from the files, every field for a dataset added or removed, the counts `added`, `removed` and `changed`, and `files`, a `{path, change, before, after}` record for each file, `before` and `after` each a `{size, sha256}` or null, and, for a dataset whose file list a side's commit lacks, as one made with `git commit -a` can, `unlisted`, naming those sides, `from` or `to`, with its files not compared; and for a dataset whose table data.toml renamed with its pin unchanged, one record under its new name with `change` `changed`, no fields or files, and `renamed_from`, its old name |
| `settings` | `settings`: one record for each setting, with `name`, `value`, `origin` (`environment`, `file`, `machine`, `default`, `flag`, or `resources` for a value the [resources level](../guides/speed-and-memory.md) set, and for mirrors that `--mirror` and a setting both give, `flag,environment` or `flag,file`) and `variable`; `machine`: `processors`, `memory`, the bytes available, null where the system does not say, as on systems other than linux, windows and macos, and `open_files`, null where the system sets no limit; `file`: `path` of the settings file, null when there is no folder for one, `found`, and `bypassed`, true when `--no-config` or `FETCHLOOM_NO_CONFIG` left it unread. [settings](../commands/settings.md#what-it-shows) |
| `settings set`, `settings unset` | `key`; `file`, the settings file; `before` and `after`, the key's value there, null where it holds none, with text masked where it may carry a login or a key; `changed`, false when the file held that already; `created`, true when the change made the file; `overridden_by`, the `FETCHLOOM_` variable that gives the key its value in this run, or null |
| `help` | `options`: the options fetchloom itself takes, which are those every command takes and `--version`; `commands`: each command with `name`, `about`, `usage`, `arguments` and `examples`. each argument has `name`, `short`, `value`, `help`, `required` and `repeats` |
| `cache verify` | `objects`: examined count; `bytes`: examined payload bytes; `changed`: hash array; `stuck`: hashes whose change marks could not be written; `unreadable`: failure records for contents, metadata or directory entries that could not be read |

project `datasets` names the dataset added by `add` or removed by `remove`. for `sync` and `lock`, and for their dry runs, it names the datasets whose pins changed, or would change: pinned for the first time, pinned to another version or other files, or no longer in `data.lock`. a [rename](data-toml.md#dataset-names) names its new name and its old one, which data.lock no longer has; a `sync` given names changes no other dataset's pin, so its `datasets` holds only names from its `selected`, a renamed one's old entry staying, and its `placement` counts only their files. an entry whose `data.toml` settings changed without changing what it pins is not named. a duplicate `add`, a `sync` that changes no pin and a successful `lock --check` return an empty array. a `sync` that could not get every file names instead the datasets it placed whole, beside `waiting`. `remove` always reports `lock_changed = true`, including when the removed dataset had no lock entry. `lock` has null placement, and a successful `lock --check` has `lock_changed = false`.

`merge` has the dataset-name arrays `from_ours` and `from_theirs`, taken from our side or their side as git names them, the checked-out branch and the merged one in a merge, `same`, `removed` and `unpinned`, the datasets in the merged `data.toml` with no pin of either side that answers them. `datasets` then names the pins that changed from the checked-out branch's. a merge that needs a choice fails with `unconfirmed` and keeps `result.project` and `result.merge`, whose `conflicts` names each dataset to choose a side for with `--ours` or `--theirs`; its `fix` is null. [after a git merge](../guides/merge-conflicts.md#after-a-git-merge) has the rules.

placement has integer counts `placed`, `replaced`, `removed` and `unchanged`; `kept`, an array of `{path, reason}`; `overwritten`, an array of `{path, reason}` for the files `--force` replaced that sync would otherwise keep, with the reason it would have kept each; and `restored`, an array of replaced files no edit of the project's explained: hardlinks that were the cache's copy of their bytes, or the copy the cache found changed, after an edit through a link, here or in another project, changed it, and files hashing every placed file found changed under an unchanged size and time; and `changed_copies`, an array of `{path, kept_as}` naming, for each restored link to the cache's changed copy, the file beside it that keeps the changed bytes; and `put_back`, an array of files fetchloom placed that someone deleted or moved, which it placed again since data.lock pins them, also counted in `placed`. `removed` counts the files a drop cut short left, which sync removes too. paths are project-relative. `kept` and `overwritten` reasons are `changed` for edited managed files, `shared` for visibly changed hardlinks shared with other projects whose shared copy the cache no longer knows, `foreign` for other bytes at a target, `unneeded` for edited managed files no longer wanted, and `blocked` for targets another entry stands in the way of, whose path that entry's record adds as `blocked_by`. adopted identical files count as unchanged; removing one later adds a warning that it was already there when sync adopted it. unrelated extra files are not included in `kept`.

`get.files` lists placement targets, so an unpacked archive is replaced by its members. each record has `path`, `size`, `sha256`, `url`, `origin`, `method`, `unpacked_from`. the path is relative to `folder`. origin is `network`, `not_modified` or `cache`; method is `reflink`, `hardlink`, `copy`, `move`, or null when already present. `move` is a file `-o` took straight from a download nothing else needs, moved out of the cache, so it counts under none of `reflinks`, `hardlinks` and `copies`. `unpacked_from` is the selected archive path or null. unpacked members inherit the archive's URL and origin. with `-o`, the one record's path is the file's name. a file `-o` found holding the expected bytes before any request, by `--sha256`, a hash the source states or, offline, the link's last download, has origin `cache` and method null; one the server said is unchanged has origin `not_modified` and method null.

`get.items` has one record for each link, in the order given on the command line and then in the `--input-file` list. each has `reference`, the link as given, with its login and each query or fragment value masked unless the value's name is known to be harmless; `status`, `ok` or `error`; `folder`, the link's absolute folder or null; `files`, file records as above, empty on failure; and `error`, null on success or an object with `kind`, `message`, `details`, `help` and, for failed downloads, `failures`, in the form of `errors[]`. one link without `-i` keeps `folder` and `files` at the top as well, and returns a null `result` on failure. several links, or any `-i` list, give a null top-level `folder` and empty `files`. when some of them fail, the run returns `error` with `result` still set: `errors[0].kind` and the exit code come from the first failed link in that order, and `errors[0].failures` names each failed link in `file`. an interruption, a refused question about the total size and a cache that cannot open return immediately with a null `result`.

status arrays, each a list of project-relative paths or dataset names, as [status](../commands/status.md#categories) explains:

| field | entries |
|---|---|
| `unlocked` | datasets data.lock does not pin as data.toml asks, and datasets that wait for their file list, which a `lock.list_missing` warning names |
| `removed` | datasets in data.lock but no longer in data.toml |
| `missing` | pinned files that are absent |
| `edited` | files fetchloom placed that were changed |
| `untracked` | files fetchloom did not place, at pinned paths, with other bytes than the pins |
| `adoptable` | files fetchloom did not place, at pinned paths, holding the pinned bytes, which sync records as placed; no difference |
| `unreadable` | files fetchloom did not place, at pinned paths and of the pinned size, that this account cannot read, so what they hold is unknown; a sync cannot read them either |
| `outdated` | unchanged files that sync replaces: pinned to other bytes now, or read-only where the dataset asks for `copy = true` |
| `damaged` | files fetchloom placed that hold other bytes though no edit of the project's shows, as a hardlink an edit through another link changed |
| `blocked` | entries in a pinned file's way |
| `extra` | files no dataset places, which sync keeps |
| `unneeded` | files fetchloom placed that no dataset needs now, which the next sync removes |
| `not_unpacked` | archives whose files this machine does not know |

JSON lists every difference, as rows in a pipe do, while a terminal shows 20 rows of each kind of file.

`status` finding differences, `sync` keeping conflicts and a `sync` whose cache registration failed, which warns that gc may not keep the project's files, still return `ok` and exit 0, so inspect the arrays and `warnings`. `cache verify` finding changed bytes returns `integrity.mismatch` and exit 30; when no bytes changed but an object or directory could not be read, it returns `local.io` and exit 50. both errors retain the verification counts and findings in `result`, alongside `errors`. unreadable object and directory paths are also named in `warnings`; unreadable objects are neither marked nor removed. counts include only objects whose metadata could be read, so an unreadable directory can hide objects from the totals. successful `changed`, `stuck` and `unreadable` arrays are empty.

`cache gc` fails with `local.io`, exit 50, when a removal fails, also after other removals succeeded, and `result` keeps the counts of what it removed and `not_removed`: records with `path`, `kind` (the I/O error kind), `os_code` (the OS code or null) and `message` (the original cause), also for a stale project's registration it could not remove. `cache verify` uses the same records in `unreadable`. human output and `warnings` name each path once, with its cause in plain words and what to do, and the error says how many there were. a path already absent is no failed removal.

## Warning kinds

a warning's `kind` says what it is about, so a script can act on one without reading its `message`, whose words may change. `details` lists the paths or names it is about, often none, and `help` says what to do, or is null. a person reads the same warning on stderr, as `warning:` with its help on the line below.

```json
{"kind":"name.taken","message":"iris was taken, so this one is iris-2","details":[],"help":"choose a name with --as <name>"}
```

| kind | what it says |
|---|---|
| `archive.packed` | an archive stays packed: fetchloom does not unpack its format, it needs other parts, it came inside another archive, or `-o` saved it whole |
| `archive.unreadable` | a file named as an archive is no archive fetchloom can read, so it stays as it is |
| `auth.key_remains` | after `auth logout`, another place still holds a key for the account, which requests go on using |
| `auth.key_unsendable` | a key holds a character no request header can carry, such as a line break, so it is never sent |
| `auth.no_keyring` | this machine has no credential store, or `FETCHLOOM_KEYRING` is off, so `auth login` saved the key in fetchloom's own file: readable only by you on linux and macos, and on windows protected by your user profile's permissions when the file is inside the profile |
| `auth.overridden` | `auth login` saved a key, but a variable set here, such as `ZENODO_TOKEN`, or a file the source's tools read first, such as `~/.kaggle/access_token` before `kaggle.json`, gives the account another key, which runs use instead |
| `auth.readable` | other users of this machine may read a file that holds logins or keys, such as a `.netrc` open to them, or on windows a key file outside your user profile, which takes its folder's permissions |
| `auth.unusable` | AWS keys, google credentials or azure credentials are set up in a way fetchloom cannot use, as the message says, so buckets and containers that need them fail the same way; `auth` lists the account as having none |
| `cache.changed` | a cached file changed, so it is downloaded again the next time it is needed |
| `cache.foreign` | the cache folder, or a folder fetchloom writes in inside it, such as `objects/` or `tmp/`, when every user may enter the cache folder, belongs to another user, unless a group you are in shares it on purpose as a setgid folder only that group may write, or every user of the machine may write to it, as may a folder in `objects/` or, for `fetchloom cache`, a cached file, or on windows the cache folder is outside your user profile on a drive other than FAT or exFAT, so it takes its permissions from the folders above it; any of them may let others change the files projects place from it |
| `cache.over_budget` | a run told `--yes` goes on though its downloads take the cache past the `cache_budget` setting; `details` says what would free room |
| `cache.unreadable` | a path in the cache, or the data.lock of a project it keeps files for, could not be read or removed |
| `cache.unshared` | in a cache its group shares, a setgid folder the group may write, a folder in it lets only its owner write to it, or on linux the run's umask keeps the group from writing what the run adds, so the group's other members cannot add to the cache |
| `cache.unregistered` | the project could not join the cache's list of projects, so `cache gc` may delete files it needs |
| `download.encoded` | a server sent a file compressed with `Content-Encoding`, and it is kept as sent |
| `download.unnamed` | a server gave no name for a file |
| `download.web_page` | a link meant for a data file answered with a web page |
| `export.incomplete` | a citation `export` wrote is built from data.lock alone, marked incomplete in the file: offline, doi.org was not asked; doi.org gave no record for its DOI; or the dataset has no DOI |
| `export.left_out` | the list `export` wrote leaves out files no link holds, as `result.left_out` says |
| `git.data_folder_ignored` | a line of the project's `.gitignore` hides the whole data folder from git, the person's own files there too, though each dataset's folder keeps itself out of git with a `.gitignore` inside it; said once per line, which fetchloom never edits; `help` names the line to delete |
| `git.dataset_unignored` | a dataset places a `.gitignore` of its own at the top of its folder, so fetchloom puts none there and git sees the dataset's files; `help` names the line to add to the project's `.gitignore` |
| `git.import_in_tree` | files an import read are in the git work tree, where `git add .` takes them, or git tracks them already; `help` names the commands that keep them out |
| `git.lock_part_untracked` | git does not have lists in `data.lock.d` that data.lock names, so a clone cannot read data.lock; `details` names them |
| `import.skipped` | an import left out links or special files, which fetchloom never follows; `details` names them |
| `license.restricted` | a dataset's license does not allow some uses, which the message names |
| `lock.list_missing` | a dataset waits for its file list, which data.lock names and `data.lock.d` lacks, as in a clone of a commit that left it out; every other dataset is used. `details` names the part |
| `lock.stale_part` | `data.lock.d` holds lists data.lock no longer names; `details` names them |
| `merge.formatting` | a merge kept one side's entry of a dataset, which the other side writes differently only in comments or spacing |
| `merge.unpinned` | after a merge, neither side's data.lock pins datasets as data.toml now asks |
| `mirror.damaged` | a mirror holds a damaged copy of a file, so it was not used |
| `mirror.skipped` | a mirror is missing, unreadable, unreachable or busy, so the run skips it |
| `name.taken` | the name an add chose was taken, so the dataset got a number |
| `network.private_origin` | a later run downloads from a private address only once `allowed_origins` lists it; `details` names each origin |
| `network.retry` | a request failed and is tried again |
| `path.tilde` | a path starts with a folder named `~`, which is not your home folder there |
| `place.adopted_removed` | a removal deletes files that were already there when fetchloom adopted them |
| `place.blocked` | something in the way keeps files out of the data folder |
| `place.copied` | files were copied rather than linked to the cache |
| `place.exclude_unreached` | an `exclude` names files inside an archive, which only its members table reaches; `details` names them |
| `place.kept` | a file differs from data.lock and stays as it is, as `result.placement.kept` says |
| `place.left_behind` | files stay in a folder no dataset uses now; `details` names them |
| `place.overwritten` | a dry run of `sync --force` would replace a file sync keeps |
| `place.path_folder` | a dataset's `path` makes a folder named like the one file it holds |
| `place.put_back` | files fetchloom placed were deleted or moved, so they were put back; `details` names them |
| `place.restored` | a changed file was put back as data.lock pins it |
| `place.set_aside` | a file `sync --force` moved out of the way could not go back, as when a folder now has its path, so it is kept under a hidden name, which `details` names; every sync says so until the file is moved |
| `place.unportable` | files have names windows and fat disks cannot store |
| `project.not_restored` | a stopped add could not put data.toml and data.lock back as they were |
| `project.owner_removed` | `.fetchloom/owner` held something no fetchloom wrote, as a cloned project can, so taking the project removed it and the command went on; the message names the file |
| `push.left_out` | a push of every dataset leaves out one `add --no-sync` pinned by what its source states, which only a sync downloads; `details` names each |
| `search.partial` | a search site did not answer in time, or its search failed; for a site that did not answer, `help` names the command that searches it alone |
| `settings.broken` | `settings set` or `settings unset` changed a settings file that still holds a problem runs stop at, such as a key that is no setting, which stops every run, or a cache folder runs refuse, which stops every run that opens the cache; the change did not bring it, and a change that brings a problem of its own is refused |
| `settings.colors_unread` | `FETCHLOOM_COLORS` holds entries fetchloom cannot read, which keep their default colors |
| `settings.missing_folder` | `settings set` saved a mirror folder that does not exist, which runs skip until it does, or a push folder that does not exist yet, which the first push makes |
| `settings.overridden` | `settings set` or `settings unset` changed a setting that a `FETCHLOOM_` variable set here gives its own value, so runs where it is set keep that value |
| `settings.switch_unread` | `FETCHLOOM_ACCESSIBLE` or `FETCHLOOM_NO_HINTS` holds neither true nor false, so it stays off rather than stopping the run |
| `settings.unknown_variable` | a variable set is one typo from one fetchloom reads, so it changes nothing; `details` names it |
| `space.short` | a run told `--yes` goes on though a disk may lack the room it needs, or a dry run's downloads would lack it, which the message measures |
| `source.archived` | a file is gone from its link in data.lock, or changed there, and came from software heritage or the wayback machine, named in the message, whose copy hashes to its pin: [offline and recovery](../concepts/offline-and-recovery.md#web-archives) |
| `source.modified` | `audit` found a file at its pinned size whose server says it changed after data.lock pinned it; `details` names each file |
| `source.moved` | a file is gone from its link in data.lock and came from where the source lists it now |
| `source.partial` | what a source lists may not be all of the dataset, as for an osf project with components or a record that holds only a README |
| `trace.unwritable` | `FETCHLOOM_TRACE` names a file fetchloom cannot write, so the run records no timeline |

## dry runs

`add`, `remove`, `exclude`, `sync`, `lock`, `push`, `cache gc` and `get` with `--dry-run` return their usual result, counting what would happen, plus `dry_run: true` and a `changes` object listing what would change. without `--dry-run` neither field is present.

for `add`, `remove`, `exclude` and `sync`, `lock_changed` says whether `data.lock` would change and `placement` counts what placing would do. `changes` has these arrays of project-relative paths:

| field | entries |
|---|---|
| `place` | files that would be placed where nothing is |
| `replace` | files that would be overwritten, also counted in `placement.replaced` |
| `remove` | managed files no dataset needs that would be deleted |
| `download` | files that would be downloaded first, as `{path, size}` with `path` as `<dataset>/<file>` and `size` in bytes, or null when only the download can tell |
| `unpack` | archives, as `<dataset>/<file>`, that would be unpacked first; their members are not in `place` or the counts |

a file already standing where a file goes whose hash only its download can learn is in neither `place` nor `replace`, nor in the counts: only that download, listed in `download`, tells whether it holds the pinned bytes.

for `get`, `changes` has the same arrays, `place` and `replace` relative to the current folder and `download` naming each file by its path in what the link lists.

`result` of `fetchloom --json sync --dry-run` in a fresh clone whose locked file is already in the cache, from a real run with the project path shortened:

```json
{"project":"/home/me/study","selected":null,"datasets":[],"lock_changed":false,"placement":{"placed":1,"replaced":0,"removed":0,"unchanged":0,"kept":[],"restored":[],"overwritten":[],"changed_copies":[],"put_back":[]},"dry_run":true,"changes":{"place":["data/iris/Iris.csv"],"replace":[],"remove":[],"download":[],"unpack":[]}}
```

for `lock`, `lock_changed` and `datasets` say whether `data.lock` would change and which pins, as far as is known without downloading; `placement` is null. `changes.download` lists the files locking would download first, to learn a hash or, with `--upgrade`, to check a file no checksum pins, which the dry run counts as unchanged until then. a file of a dataset that resolves again to a revision whose bytes cannot change, such as a hugging face commit, is not checked, so it is not listed. `changes.unpack` lists the archives it would unpack to learn what they hold. `place`, `replace` and `remove` are empty.

for `push`, the counts say what it would write, and `changes` has `copy`, a `{path, sha256, size}` record for each file it would copy into the mirror, `path` being the first `<dataset>/<file>` that needs it, `trees`, the same for each list of an archive's files, `path` naming the archive; `download`, a `{path, size}` record for each file it would download first, as `<dataset>/<path>` in data.lock; and `unpack`, the archives among them. it downloads and writes nothing, starts no mirror and asks nothing.

for `cache gc`, `removed_objects`, `removed_bytes`, `imported_objects`, `imported_bytes`, `kept_objects`, `kept_bytes`, `removed_leftovers` and `removed_leftover_bytes` count what gc would remove and keep, and `not_removed` lists failures for paths it could not inspect. `changes.remove` lists the path of each object, unpack record, leftover and project registration gc would delete, under the cache folder as `FETCHLOOM_CACHE` gives it, so a relative cache gives relative paths. a gc dry run returns `ok` even when `not_removed` is not empty.

the work counters still count metadata requests a dry run makes to resolve changed entries. `objects_written`, `reflinks`, `hardlinks`, `copies`, `placed_from_cache`, `data_folder_objects` and `data_folder_bytes` stay 0.

## search candidates

`search` returns `candidates` on success: at most 12 without `--limit`, else up to `--limit` from each site. plain-word `add` and `get` retain the same array when they fail with `reference.ambiguous`; no matches returns an empty array with `reference.unresolved`.

| field | value |
|---|---|
| `reference` | canonical reference string to pass to add or get |
| `title` | the title the site gives, or the repository id for hugging face |
| `source` | `hugging face`, `kaggle`, `zenodo`, `ENA`, `GEO`, `dandi`, `openneuro`, `arXiv` or `semantic scholar` |
| `size` | byte count as the site lists it: for kaggle the zipped download, for dandi the version's assets and for openneuro the snapshot; null when unknown, as always for ENA and GEO |
| `downloads` | the download count the site gives, or null, as always for ENA, GEO and dandi |
| `note` | an access note, as `needs access`; for a paper, its first author and year, then `no open-access pdf` for one `add` would refuse, as `Vaswani et al. 2017, no open-access pdf`; or null |

[search](../commands/search.md) defines ordering and interactive selection. `list.datasets` instead reflects the manifest and lock, with null lock-derived values when a dataset has no lock entry; a stale entry can still supply values.

## work counters

all counts and byte quantities are unsigned integers. timestamps and large counters can exceed javascript's exact integer range.

| field | counts |
|---|---|
| `requests` | explicit metadata or file request attempts, including retries, counted as each is sent; redirect hops are not counted separately, and a request refused before it is sent, as in [mirror-only mode](../guides/mirrors.md#mirror-only-mode), is not counted |
| `bytes_down` | body bytes read, including metadata and unsuccessful attempts; excludes headers and the saved resume prefix |
| `retries` | scheduled retry waits |
| `resumes` | accepted nonzero range starts |
| `objects_written` | new objects counted after successful downloads, copies from a mirror or the data folder, and archive unpacking |
| `downloads_skipped` | downloads not made because the cache already held the bytes, as the link's earlier record or a stated MD5 showed; a download placing already knows the cache holds is never started, so it is not counted here |
| `reflinks`, `hardlinks`, `copies` | new placements by each method, including replacements, and files a sync linked to the cache in their place, as copies of cached files and files already in place |
| `mirror_objects`, `mirror_bytes` | pinned files a [mirror](../guides/mirrors.md) gave this run, now in the cache, and their bytes. a file the cache already held is never asked for |
| `reused_objects`, `reused_bytes` | pinned files the cache lacked that this run cloned or copied in from hugging face's cache, kagglehub's or a folder `reuse` names, each after its bytes hashed to its pin, and their bytes: [reusing other caches](../guides/other-tools.md#reusing-other-caches) |
| `data_folder_objects`, `data_folder_bytes` | pinned files the cache lacked that this run copied in from the project's data folder, at paths data.lock places them at, each after its bytes hashed to its pin, and their bytes, as [moving a project](../commands/cache.md#moving-a-project-to-another-machine) describes. a mirror is asked only for what the data folder did not give |
| `placed_from_cache` | placements by `add`, `sync`, `path` and `get` from objects the run did not write to the cache itself; the other placements took bytes the run downloaded, unpacked or copied in from a mirror or the data folder. file times do not decide it, so it holds on filesystems that keep times in whole seconds and on file servers with their own clocks |
| `elapsed_ms` | elapsed whole milliseconds before rendering the final report |

`-v` ends with the same counters in words, as in `1 request · 47.7 MiB down · 1 hardlink · 1 placed from the cache · 2.0 s in all`. `1 file written to the cache` counts a write during the run, so a one-off `get -o` that moved its file out of the cache afterwards still shows it. there are no phase timings or bytes-written counters. cache verification reports directory and metadata read failures in `result.unreadable` rather than claiming those paths were verified.

a killed process may leave no valid JSON. treat a missing or invalid document as a failure. [scripting](../guides/scripts.md) shows checks for exit codes, schema versions and preserved conflicts.

# project folder

`.fetchloom/` holds a project's local lock, owner record and placement state.

`.fetchloom` must be a real directory. a symlink or junction is refused before fetchloom writes its lock, owner, state or ignore file.

| path | contents |
|---|---|
| `.fetchloom/.gitignore` | `*` and LF, written when the folder is created |
| `.fetchloom/lock` | OS project lock; file contents are unused |
| `.fetchloom/owner` | JSON identifying the modifying command, shared by windows and WSL |
| `.fetchloom/owner.<pid>.<16 hex digits>.tmp` | owner record being written before hardlink publication; a hard kill can leave this file behind |
| `.fetchloom/state` | compact JSON describing tracked placements |
| `.fetchloom/.<name>.<8 hex digits>.tmp` | metadata being written; a hard kill can leave this file behind |
| `.fetchloom/.fetchloom-<16 hex digits>.probe` | an empty hidden file, there only while a removal tests whether the volume can hardlink |

inside a git repository, `add` adds `/<data_dir>/` to the project's `.gitignore` if absent. `.fetchloom/` ignores itself separately.

## owner record

| field | value |
|---|---|
| `os` | operating system name, such as `windows` or `linux` for WSL |
| `pid` | process id on that operating system |
| `started` | UTC time when the command claimed the project, in `YYYY-MM-DDTHH:MM:SSZ` form |
| `command` | subcommand name, without arguments or credentials |

fetchloom takes the OS lock, writes and flushes the complete owner JSON to `owner.<pid>.<16 hex digits>.tmp` beside `owner`, then hardlinks that file to `owner`. the hardlink fails if `owner` already exists, so a newly published owner is never empty. the temporary name is removed after the attempt.

when hardlinks fail for another reason, fetchloom falls back to exclusive creation followed by a temporary-file write and rename; only this fallback can expose an empty reservation.

dropping the command's guard removes `owner` before releasing the OS lock, including when the command returns an error.

an existing owner is stale when its `os` matches this process and this process holds the OS lock, or when the file is empty and its modification time is more than ten seconds old. without a working OS lock, a same-os record is not stale.

before removing a stale record, fetchloom reads it again and requires the same bytes and modification time.

any other record, including nonempty invalid JSON, makes the command wait. the waiting message identifies the holder when readable and gives the exact `owner` path to delete if that process is gone.

a record from another os is never reclaimed by its age or pid. delete it only after that process has ended.

## placement state

| field | value |
|---|---|
| `version` | integer `1` |
| `files` | object keyed by project-relative path, using `/` |
| `files[path].dataset` | dataset name |
| `files[path].sha256` | locked member or file hash, 64 hex digits |
| `files[path].method` | `reflink`, `hardlink` or `copy`. an adopted file is `hardlink` when it is the cached object's own file, else `copy` |
| `files[path].look.size` | byte count |
| `files[path].look.modified` | modification time in nanoseconds since 1970; zero if unavailable, capped at the largest unsigned 64-bit integer |
| `files[path].writable` | `true` when the file was placed or adopted writable |
| `files[path].checked` | start of the scan used to place, adopt or vouch for the file, in nanoseconds since 1970; marks written during that run still put the file in doubt |
| `synced` | absent until a `sync` records it. `manifest` and `lock` hold the SHA-256 of `data.toml` and `data.lock` as that sync left them, in hex, and `complete` is `true` when every file they ask for ended up in place; [the settled-sync shortcut](../../dev/architecture/sync-loop.md#settled-sync-shortcut) uses it to skip planning |

there is no inode or file-id field. missing, unreadable, malformed or unsupported-version state counts as empty. matching existing files can then be adopted into state and later removed as managed files. metadata comparisons do not prove content integrity.

a file whose `look.modified` is not older than the state file's own modification time is hashed before its look is trusted, since both times come from one filesystem clock and a coarse clock hides an edit made in the same tick. a hash that differs counts as an edit. a sync that finds such files unchanged writes the state again, which settles them.

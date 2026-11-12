# project folder

the project folder holds `data.toml` and `data.lock`, and its `.fetchloom/` holds what this machine knows of the project: a lock, a record of which command holds it, and what sync placed. [project](../../dev/architecture/project.md) in the developer guide has the mechanics.

`.fetchloom` must be a real folder; a link or junction there is refused. a first `add` that adds nothing removes the `.fetchloom` it made.

| path | contents |
|---|---|
| `.fetchloom/.gitignore` | `*` and LF, written when the folder is created |
| `.fetchloom/lock` | OS project lock; file contents are unused |
| `.fetchloom/owner` | JSON identifying the modifying command, shared by windows and WSL |
| `.fetchloom/owner.<pid>.<16 hex digits>.tmp` | owner record being written before hardlink publication; a hard kill can leave this file behind |
| `.fetchloom/state` | compact JSON describing tracked placements |
| `.fetchloom/builds/<recipe>.log` | the output of the last run of a [recipe](../recipes.md)'s step, made anew for each run, readable by you only on linux and macos, keeping the latest 16 MiB |
| `.fetchloom/data.toml.recovery.<16 hex digits>`, `.fetchloom/data.lock.recovery.<16 hex digits>` | exact pre-add metadata, retained if rollback fails |
| `.fetchloom/.<name>.<8 hex digits>.tmp` | metadata being written; a hard kill can leave this file behind |
| `.fetchloom/.fetchloom-<16 hex digits>.probe` | an empty hidden file, there only while a removal tests whether the volume can hardlink |

inside a git repository, every command that places files, `add`, `sync` and a `path` that syncs its dataset first, adds `/<data_dir>/` to the project's `.gitignore` if absent. `.fetchloom/` ignores itself.

inside the data folder, a file being copied or cloned into place is first written to a hidden `.fetchloom-<16 hex digits>.copy` beside its destination and then renamed. a hard kill can leave one behind; `status` does not list it, and the next `sync` deletes it.

## finding the project

project commands use the nearest folder at or above the working directory that holds `data.toml`. the search never leaves the git repository it starts in: it checks each folder for `data.toml` first, then stops at the first folder holding `.git`, as a folder or as the file a submodule or worktree has. a `data.toml` above that folder belongs to another repository's history, where a command would pin data in the wrong one, and a stray `data.toml` in a home folder would otherwise take in every repository below it. any entry named `data.toml` marks its folder, so one that is no file, as a folder made by mistake, stops a command there with `local.io`, exit 50, as `/home/ada/thesis/data.toml is a folder, not a file`, rather than a search above it or an offer to start a project; nothing is written.

in a git repository with no `data.toml` of its own, project commands fail with exit 50 and name the repository, and `add` starts a new project in the working directory. to use a project above the repository on purpose, name its folder with `-C`, as in `fetchloom -C .. sync`.

## owner record

| field | value |
|---|---|
| `os` | operating system name, such as `windows` or `linux` for WSL |
| `pid` | process id on that operating system |
| `started` | UTC time when the command claimed the project, in `YYYY-MM-DDTHH:MM:SSZ` form |
| `command` | subcommand name, without arguments or credentials |

the record lets windows and WSL, which cannot see each other's locks, keep out of each other's way. a command that finds the project held waits, naming the command and process id the record holds, as in ``Waiting for `fetchloom add` (pid 4242) working in this project``. a record a killed command on this system left is taken over once its lock is free. a record from another system, or one that cannot be read, makes the command wait, and the waiting line gives the exact `owner` path: delete it only once that process has ended.

## placement state

| field | value |
|---|---|
| `version` | integer `1` |
| `files` | object keyed by project-relative path, using `/` |
| `files[path].dataset` | dataset name |
| `files[path].sha256` | locked member or file hash, 64 hex digits |
| `files[path].method` | `reflink`, `hardlink` or `copy`. an adopted file is `hardlink` when it is, or sync linked it to, the cached object's own file, else `copy` |
| `files[path].look.size` | byte count |
| `files[path].look.modified` | modification time in nanoseconds since 1970; zero if unavailable, capped at the largest unsigned 64-bit integer |
| `files[path].writable` | `true` when the file was placed or adopted writable |
| `files[path].checked` | start of the scan used to place, adopt or vouch for the file, in nanoseconds since 1970; marks written during that run still put the file in doubt |
| `files[path].adopted` | `true` when the file was already at its place with the pinned bytes before sync took it as placed, so a removal says so; absent otherwise |
| `synced` | absent until a `sync` records it. `manifest` and `lock` hold the SHA-256 of `data.toml` and `data.lock` as that sync left them, in hex, and `complete` is `true` when every file they ask for ended up in place; [the settled-sync shortcut](../../dev/architecture/sync-loop.md#settled-sync-shortcut) uses it to skip planning |

missing, unreadable or malformed state counts as empty, and files already holding the pinned bytes are then adopted again. a file changed too soon after the state was written to tell by its time is hashed before its look is trusted.

recovery files hold `data.toml` and `data.lock` as they were before an add changed them, readable by you only on unix, and go once the add succeeds or is undone. errors name them without printing them. a killed add can leave one; compare it with the current files before using it.

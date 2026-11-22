# project folder

the project folder holds `data.toml` and `data.lock`, and its `.fetchloom/` holds what this machine knows of the project: a lock, a record of which command holds it, and what sync placed. [project](../../dev/architecture/project.md) in the developer guide has the mechanics.

`.fetchloom` must be a real folder; a link or junction there is refused. a first `add` that adds nothing removes the `.fetchloom` it made.

`data.toml` and `data.lock` must be plain files in the project folder. fetchloom never follows a link there, since a cloned repository could point one at any file of yours, and an error could then show a line of it: a link is refused with `project.invalid`, exit 50, and a folder or a special file, such as a pipe, with `local.io`, before a byte is read. the refusal of a link says to delete it if it came with a clone, and to put a real file in its place only if the link was yours, since a copy of what a clone's link names, such as your cloud keys, would sit where git takes it. fetchloom reads at most 256 MiB of `data.toml` and 1 GiB of `data.lock`.

| path | contents |
|---|---|
| `.fetchloom/.gitignore` | `*` and LF, written when the folder is created |
| `.fetchloom/lock` | OS project lock; file contents are unused |
| `.fetchloom/owner` | JSON identifying the modifying command, shared by windows and WSL |
| `.fetchloom/owner.<pid>.<16 hex digits>.tmp` | owner record being written before hardlink publication; a hard kill can leave this file behind |
| `.fetchloom/state` | compact JSON describing tracked placements |
| `.fetchloom/data.toml.recovery.<16 hex digits>`, `.fetchloom/data.lock.recovery.<16 hex digits>` | exact pre-add metadata, retained if rollback fails |
| `.fetchloom/pending-lock` | the data.lock `exclude` writes after data.toml, below the SHA-256 of that data.toml and the project folder it ran in, there only between its first write and its last. a hard kill between them leaves it, and the next command that changes the project writes that data.lock when data.toml is still the one exclude wrote in this folder, then removes it; one a clone or a copy of the project brought along is removed unused |
| `.fetchloom/.<name>.<8 hex digits>.tmp` | metadata being written; a hard kill or a power cut can leave this file behind, and the next command that changes the project removes one of `data.toml`, `data.lock`, the state, `.fetchloom/.gitignore` or a list's part |
| `.fetchloom/.fetchloom-<16 hex digits>.probe` | an empty hidden file, there only while a removal tests whether the volume can hardlink |

every command that places files, `add`, `sync` and a `path` that syncs its dataset first, puts a `.gitignore` in each dataset's folder before its files land, which keeps them out of git, in a git repository or not. fetchloom never edits the project's own `.gitignore`. `.fetchloom/` ignores itself.

inside the data folder, a file being copied or cloned into place is first written to a hidden `.fetchloom-<16 hex digits>.copy` beside its destination and then renamed. a hard kill can leave one behind; `status` does not list it, and the next `sync` deletes it.

## finding the project

project commands use the nearest folder at or above the working directory that holds `data.toml`. the search never leaves the git repository it starts in: it checks each folder for `data.toml` first, then stops at the first folder holding `.git`, as a folder or as the file a submodule or worktree has. a `data.toml` above that folder belongs to another repository's history, where a command would pin data in the wrong one, and a stray `data.toml` in a home folder would otherwise take in every repository below it. any entry named `data.toml` marks its folder, so one that is no file, as a folder made by mistake, stops a command there with `local.io`, exit 50, as `/home/ada/thesis/data.toml is a folder, not a file`, rather than a search above it or an offer to start a project; nothing is written.

in a git repository with no `data.toml` of its own, project commands fail with exit 50 and name the repository, and `add` starts a new project in the working directory. to use a project above the repository on purpose, name its folder with `-C`, as in `fetchloom -C .. sync`.

where no project is found, a folder up to three below that holds a `data.toml` is most likely the one meant, as after one `cd ..` too many, so the help names it with the command to run there, as ``there is one in myproj/: run `fetchloom -C myproj sync` ``. with none below, the help says to go into the project's folder, or to start a project there with `fetchloom add <link>`. at the root of a disk, a project below is as likely another user's as yours, so none is named, and the help says to go into your project's folder. hidden folders and links are passed by. a folder whose name a shell needs quoted, as `my project`, is named quoted, and one whose name no command in a help can carry, as one holding a backtick, still counts but goes unnamed, so a first `add` never wraps it unasked either.

## owner record

| field | value |
|---|---|
| `os` | operating system name, such as `windows` or `linux` for WSL |
| `pid` | process id on that operating system |
| `started` | UTC time when the command claimed the project, in `YYYY-MM-DDTHH:MM:SSZ` form |
| `command` | subcommand name, without arguments or credentials |
| `folder` | the project folder as that system reads it: its number, its creation time, the system's name and its path, so a record a copy or a clone carries is told from one written here |

the record lets windows and WSL, which cannot see each other's locks, keep out of each other's way. a command that finds the project held waits, naming the command and process id the record holds, as in ``Waiting for `fetchloom add` (pid 4242) working in this project``. a record a killed command on this system left is taken over once its lock is free, and one no fetchloom writes, or one written for another folder, as a clone or a copy carries, is removed with a warning. a record another system wrote for this same folder, as windows and WSL both name a folder on a windows drive, makes the command wait, since that process may run on this machine, and so does an empty one, which a command on another system may be writing still; the waiting line gives the exact `owner` path: delete it only once that process has ended. a record another system wrote for a folder this system cannot match with its own, as a project copied from windows to a linux disk carries, holds the project for no process on this machine, and one on another machine this one cannot check, so the command stops at once with `project.invalid`, exit 50, naming the record's command, system and process id, and the record stays: delete it once no fetchloom runs in the project on that system.

## placement state

| field | value |
|---|---|
| `version` | integer `3`; a state of any other version counts as empty |
| `at` | the byte offset where `files` ends, padded with spaces to 20 characters |
| `files` | object keyed by project-relative path, using `/` |
| `files[path].dataset` | dataset name |
| `files[path].sha256` | locked member or file hash, 64 hex digits |
| `files[path].method` | `reflink`, `hardlink` or `copy`. an adopted file is `hardlink` when it is, or sync linked it to, the cached object's own file, else `copy` |
| `files[path].look.size` | byte count |
| `files[path].look.modified` | modification time in nanoseconds since 1970; zero if unavailable, capped at the largest unsigned 64-bit integer |
| `files[path].writable` | `true` when the file was placed or adopted writable |
| `files[path].checked` | start of the scan used to place, adopt or vouch for the file, in nanoseconds since 1970; marks written during that run still put the file in doubt |
| `files[path].adopted` | `true` when the file was already at its place with the pinned bytes before sync took it as placed, so a removal says so; absent otherwise |
| `files[path].output` | `true` when the file is an output's: the author's own, pinned where their pipeline wrote it, or a writable copy a sync placed there; absent otherwise |
| `files[path].unlinkable` | `true` when a sync found that this copy of its own cannot be linked to the cache's copy, as when it holds other bytes or the disk refused the link, so no later sync tries again; absent otherwise |
| `synced` | absent until a `sync` records it. `manifest` and `lock` hold the SHA-256 of `data.toml` and `data.lock` as that sync left them, in hex, and `complete` is `true` when every file they ask for ended up in place; [the settled-sync shortcut](../../dev/architecture/sync-loop.md#settled-sync-shortcut) uses it to skip planning |
| `dropped` | the datasets [`drop`](../commands/drop.md) left out on this machine, which a plain sync leaves out |
| `freeing` | those of `dropped` whose drop was cut short before it freed what the cache holds of them, which the same drop finishes |
| `folder` | the project folder the state was written for: its file number, `inode`, its creation time in nanoseconds since 1970, `created`, the system that read them, `os`, and its resolved path, `path`, each where the system tells it. a state a clone, a copy or git brought along matches none, so its records are not used |
| `aside` | the files `sync --force` moved out of the way, by the hidden name each is kept under, with the path it had, written before any moves |
| `replacing` | the files fetchloom placed that a sync removes to place the pinned ones, written before it removes any |
| `uncached` | that a sync found the cache at `cache` holding none of the objects of this project's own copies of cached files, with `copies`, a sum of their SHA-256 in path order, and `newest`, the newest change of the folders their objects would be in, in nanoseconds since 1970, so a settled sync need not look again while neither changed |
| `markers` | the dataset folders fetchloom put a `.gitignore` of its own in, by path in the project, each with the SHA-256 of what it last wrote there |
| `left` | the folders an output's move left holding files, whose markers stay while they hold any |
| `followed` | the folder each data folder link led to when fetchloom first followed it, resolved, by the link's path in the project |
| `told` | what runs told a person once, each as its kind and its subject, as `refill:data/iris`, so a later run says it again only of a new subject |
| `id` | the project's id, which each `.gitignore` of fetchloom's in a dataset folder names |
| `blocks` | `files` cut into blocks of about 64 KiB, each as its first path and its byte range, start and end, in the file. a `path` or `sync` that names datasets reads only the blocks their folders need |

missing, unreadable or malformed state counts as empty, and files already holding the pinned bytes are then adopted again. a state whose `at` or `blocks` do not match its records is read whole. one written for another folder, as a clone or a copy of the project brings it, counts as empty too, and none of its records is read. a file changed too soon after the state was written to tell by its time is hashed before its look is trusted.

recovery files hold `data.toml` and `data.lock` as they were before an add changed them, readable by you only on unix, and go once the add succeeds or is undone. errors name them without printing them. a killed add can leave one; compare it with the current files before using it.

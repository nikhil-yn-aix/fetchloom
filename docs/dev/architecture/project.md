# project

`project` owns discovery, project records and the lock that serializes changes to one project's files.

## ownership

`Project` finds the nearest ancestor containing `data.toml`, without crossing the top of the git repository the search starts in, and reads the manifest, lock and local state. it also writes metadata and checks the data-folder chain; command handlers decide when those operations happen.

## files

| file | what it holds | types and functions |
|---|---|---|
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project.rs) | discovery, locking and metadata writes | `Project`, `Held`, `Hold`, `Owner`, `find`, `hold`, `write`, `write_state`, `data_dir` |
| [manifest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/manifest.rs) | manifest parsing and edits that preserve comments, a new dataset in name order | `Manifest`, `Entry`, `Reader`, `parse`, `with_dataset`, `without_dataset`, `with_dataset_from`, `tables`, `outside` |
| [lock.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock.rs) | pin validation and deterministic serialization | `Lock`, `Locked`, `LockedFile`, `Drift`, `parse`, `render`, `spec`, `timestamp` |
| [lock/canonical.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/canonical.rs) | fast reader for fetchloom's own lock output | `read` |
| [lock/derived.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/derived.rs) | the entries of datasets a recipe made: inputs, command, code, variables, key and tree | `Derived`, `Input`, `Code`, `check`, `check_files`, `write` |
| [lock/lists.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/lists.rs) | the compressed file lists in `data.lock.d`, of sources past 50,000 files and of every recipe result, typed by their header line | `Listed`, `Row`, `texts`, `listed`, `read`, `compress`, `is_part`, `inline` |
| [lock/merge.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/merge.rs) | data.toml and data.lock merged a dataset at a time after a git merge, with the choices a person made | `merge`, `Merged`, `Summary`, `Side`, `Choices`, `decide` |
| [lock/merge/markers.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/merge/markers.rs) | the two sides, and the base, git's conflict lines hold | `Sides`, `split`, `first` |
| [lock/merge/git.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/merge/git.rs) | the merge base from git's index, with git found and run as a bounded reader | `find`, `base` |
| [state.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/state.rs) | local observations and settled-sync evidence | `State`, `Placed`, `Look`, `Synced`, `nanos` |

## how it works

### three records with different authority

| record | establishes | failure behavior |
|---|---|---|
| `data.toml` | requested sources, filters, extraction and destinations | invalid input stops the command with a location and explanation |
| `data.lock` | resolved references, SHA-256, size and optional archive tree hash, or a recipe's inputs, command, code, key and result tree | malformed, duplicate-name or unsupported locks fail; silently resolving anew could pin different bytes |
| `.fetchloom/state` | what this machine placed and how it looked | unreadable or unsupported state counts as empty; matching files must then be checked before adoption |

`Manifest` uses `toml_edit` so add and remove can preserve human formatting. unknown keys fail. when either file fails to read and holds git's conflict lines at a line start, `Project` reports an unfinished merge naming the first such line instead of the parser's error, with `fetchloom lock` as its help and JSON `fix`. manifest citations omit a line's text when it overlaps a parsed reference span; invalid TOML has no trusted spans and cites positions only. this also protects inline tables and escaped keys without scanning every reference for every citation. the lock writer sorts datasets and files for stable diffs. `canonical::read` accepts the writer's narrow format quickly, then checks the same field grammar with comments; other valid TOML falls back to the general parser and the same path checks. escaped strings grow with the token being decoded; a long following URL does not reserve extra space for the path.

`Look` stores size and modification time, not the full filesystem metadata object. `Placed` also records the digest, dataset, method, writable mode and last checked time. `Synced` stores exact input hashes and completeness for the [settled shortcut](sync-loop.md#settled-sync-shortcut).

### mutation ownership

mutating commands acquire the OS file lock on `.fetchloom/lock`, then publish `.fetchloom/owner`. the owner record matters when windows and WSL share a project but cannot see each other's OS locks. `Held` removes its owner record before releasing the OS lock, including on an error path.

waiting commands poll every 200 ms, report the holder once and remain cancellable. a same-OS owner can be reclaimed after the OS lock is acquired; an owner from another OS needs conservative handling. an empty reservation never expires automatically: its writer may be paused on another OS, whose lock this process cannot observe. stale removal rereads the owner before deleting it. the record is flushed before it gets its name, so a power cut leaves a whole record, which the next same-OS command takes over, never an empty one. a command that holds the OS lock and cannot write the record, as on a full disk, goes on without it; without OS locks the record is the lock, so the failure stops the command.

a command that set out to start a project and added nothing removes `.fetchloom` with `Project::unstart`, taking out the lock file last while it still holds it. `hold` therefore checks, once it has the lock, that `.fetchloom/lock` still names the locked file (device and inode on unix, volume serial and file index on windows), and opens the name again when it does not, as partial claims do.

`status`, `list`, a ready `path` and `lock --check` need no mutation hold. a `sync` that cannot take the hold for a local I/O reason, as on a checkout this user may not write, answers without it when it would write nothing to the project: the settled check a dry run uses, plus an ignore rule already in place. it then takes the cache's shared hold to list the project, or only reads the cache when that hold fails too. anything else keeps the hold's error. a `path` that waits must reread the manifest and check readiness again because the preceding command may have completed the work.

### publication

metadata writes stage in `.fetchloom` when it exists, otherwise beside their destination. state is encoded into a complete JSON buffer before the staged file is written. they flush the new file and rename it into place. on unix, they then flush the destination directory and, when different, the staging directory. first creation of `.fetchloom` also flushes the project root. a directory-flush error after rename reports that the new file is already visible but its publication could not be confirmed durable. the manifest and lock remain separate replacements, not one transaction. the owner record has separate claim-and-publication rules because two OS environments must not both acquire it.

before a new add changes metadata, it saves exact recovery copies with exclusive names. the copies must be flushed before the first metadata replacement. a failed restoration retains them and reports their paths; their contents never enter diagnostics. a guard removes them on success or complete rollback.

payload durability belongs to [placement](placing.md#copy-durability). writing state only after copied bytes are flushed is part of the sync contract, not something `write_state` can enforce by itself.

## lock reading

runs read a lock with `Lock::read`, which loads and checks the lists a lock names below the project; `Lock::parse` is for tests. data.lock has one format, `version = 1`; any other version is refused, a higher one with a request for a newer fetchloom. both readers give the same parsed datasets, each holding its files or the list that holds them, and one step then reads and checks the lists. a writer writes missing list parts before `data.lock`, and `Project::sweep_lists` removes parts no lock names once the new lock is final: right after `lock` and `sync` write it, and after `add` commits, since a failed add puts the old lock back. `Lock` keeps fetched datasets in `datasets` and the results of recipes in `derived`, one name in one list only. a result's files always come from a list whose header is the tree's, `path\tsize\tsha256`, and reading recomputes its tree hash; a source list's header names its six columns, so one kind of list never reads as the other. no command makes results yet: relocking moves them into the new lock unchanged, unless a fetched dataset now has the name, and the plan places nothing from them. already canonical locks use the narrow reader first. the next pass skips blank lines and valid TOML comments around the writer’s fields and after values. hash signs inside escaped or plain strings stay data. field order, duplicate checks, sizes and safe paths retain the same validation; other hand-edited TOML falls back to the general parser.

## merges

`lock` in a project whose data.toml or data.lock holds git's conflict lines merges instead of resolving: `markers::split` gives each file's sides, and the base when every conflict shows one, as git's `diff3` and `zdiff3` styles do. otherwise `git::base` reads stage 1 of git's index, or an empty base when git holds only stages 2 and 3, as for a file both sides added. git is the one program fetchloom runs here, and only as a reader: the first `git` on `PATH` in an absolute folder, refused when it lies inside the project, run with `--no-lazy-fetch` and `-c core.fsmonitor=false`, no shell, a closed stdin, 256 MiB of output and 10 s at most. git older than 2.45 refuses `--no-lazy-fetch`, and any failure means no base; nothing retries without those protections.

`merge` decides each dataset from its text alone on each side and at the base (`decide`): one side's change wins, alike sides stay, and anything else needs a choice. the same values written apart keep one side's text with a warning. the text outside the datasets must agree or have changed on one side only, which picks the side whose document the merge edits; tables from the other side are copied with the comments touching their headers, in name order. the merged text is read again and checked against the decisions before anything is written. data.lock is merged at the level of parsed entries, so a listed dataset is compared by its list's names, count and hash without reading a part; a pin is kept only when its spec answers the merged entry, a choice for data.toml brings that side's pin, and recipe results merge by name. only the kept pins' parts are read, through the usual reader, which checks them.

## ready path waiters

a waiting `path` notices a changed state file and runs the normal read-only readiness check before trying the mutation hold again. waiters whose files are now ready can finish together instead of handing the owner record from reader to reader. the state stamp only triggers a recheck: fresh state alone never vouches for missing, changed or doubtful files. unchanged stamps and failed readiness checks keep the normal hold-and-reread path.

## invariants and traps

- commit the manifest and lock, not local observations. a copied state file is not proof of another machine's files.
- do not treat a broken lock like missing state. recovery may require restoring the lock from git or explicitly resolving all entries again.
- a tree hash pins archive contents without expanding the lock to one record per member.
- root discovery uses the nearest manifest. nested projects are distinct roots. a folder holding `.git`, a folder or a file, ends the search once its own manifest is checked, so a manifest above a repository needs `-C`; `ignored_in_git` uses the same repository test.
- path checks reject traversal and links in protected directory chains, but do not make a sandbox against another process swapping directories during a run.

## tests

| files | cargo filter |
|---|---|
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project.rs), [manifest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/manifest.rs), [lock.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock.rs), [state.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/state.rs) | `cargo test --locked --lib project::` |
| [merge.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/merge.rs), [merge tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/merge.rs) | `cargo test --locked --lib project::lock::merge`; `cargo test --locked --test cli merge::`; the git cases run against the git on `PATH` and skip without one, and those that read the index need git 2.45 or later |
| [concurrent.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/concurrent.rs) | `cargo test --locked --test cli concurrent::` |
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/project.rs), [add.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/add.rs), [moved.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/moved.rs) | `cargo test --locked --test cli project::`; `cargo test --locked --test cli add::`; `cargo test --locked --test cli moved::` |

the lock reader models use fixed generated inputs and mutations to compare canonical and general TOML decoding, including escaped strings, integer limits, line endings, path rejection and deterministic rendering.

`path` and `status` reject portable case collisions in the plan before reporting locations or creating project metadata. Linux command tests cover this independently of the host filesystem’s case sensitivity.

## related

[sync loop](sync-loop.md), [placing](placing.md), [data.toml](../../user/reference/data-toml.md), [data.lock](../../user/reference/data-lock.md), [project-folder records](../../user/reference/project-folder.md).

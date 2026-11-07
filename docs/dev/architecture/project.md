# project

`project` owns discovery, project records and the lock that serializes changes to one project's files.

## ownership

`Project` finds the nearest ancestor containing `data.toml` and reads the manifest, lock and local state. it also writes metadata and checks the data-folder chain; command handlers decide when those operations happen.

## files

| file | what it holds | types and functions |
|---|---|---|
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project.rs) | discovery, locking and metadata writes | `Project`, `Held`, `Hold`, `Owner`, `find`, `hold`, `write`, `write_state`, `data_dir` |
| [manifest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/manifest.rs) | manifest parsing and edits that preserve comments | `Manifest`, `Entry`, `Reader`, `parse`, `with_dataset`, `without_dataset` |
| [lock.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock.rs) | pin validation and deterministic serialization | `Lock`, `Locked`, `LockedFile`, `parse`, `render`, `spec`, `timestamp` |
| [lock/canonical.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/canonical.rs) | fast reader for fetchloom's own lock output | `read` |
| [state.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/state.rs) | local observations and settled-sync evidence | `State`, `Placed`, `Look`, `Synced`, `nanos` |

## how it works

### three records with different authority

| record | establishes | failure behavior |
|---|---|---|
| `data.toml` | requested sources, filters, extraction and destinations | invalid input stops the command with a location and explanation |
| `data.lock` | resolved references, SHA-256, size and optional archive tree hash | malformed, duplicate-name or unsupported locks fail; silently resolving anew could pin different bytes |
| `.fetchloom/state` | what this machine placed and how it looked | unreadable or unsupported state counts as empty; matching files must then be checked before adoption |

`Manifest` uses `toml_edit` so add and remove can preserve human formatting. unknown keys fail. manifest citations omit a line's text when it overlaps a parsed reference span; invalid TOML has no trusted spans and cites positions only. this also protects inline tables and escaped keys without scanning every reference for every citation. the lock writer sorts datasets and files for stable diffs. `canonical::read` accepts the writer's narrow format quickly; other valid TOML falls back to the general parser and the same path checks. escaped strings grow with the token being decoded; a long following URL does not reserve extra space for the path.

`Look` stores size and modification time, not the full filesystem metadata object. `Placed` also records the digest, dataset, method, writable mode and last checked time. `Synced` stores exact input hashes and completeness for the [settled shortcut](sync-loop.md#settled-sync-shortcut).

### mutation ownership

mutating commands acquire the OS file lock on `.fetchloom/lock`, then publish `.fetchloom/owner`. the owner record matters when windows and WSL share a project but cannot see each other's OS locks. `Held` removes its owner record before releasing the OS lock, including on an error path.

waiting commands poll every 200 ms, report the holder once and remain cancellable. a same-OS owner can be reclaimed after the OS lock is acquired; an owner from another OS needs conservative handling. an empty reservation never expires automatically: its writer may be paused on another OS, whose lock this process cannot observe. stale removal rereads the owner before deleting it.

`status`, `list`, a ready `path` and `lock --check` need no mutation hold. a `path` that waits must reread the manifest and check readiness again because the preceding command may have completed the work.

### publication

metadata writes stage in `.fetchloom` when it exists, otherwise beside their destination. they flush the new file and rename it into place. on unix, they then flush the destination directory and, when different, the staging directory. first creation of `.fetchloom` also flushes the project root. a directory-flush error after rename reports that the new file is already visible but its publication could not be confirmed durable. the manifest and lock remain separate replacements, not one transaction. the owner record has separate claim-and-publication rules because two OS environments must not both acquire it.

before a new add changes metadata, it saves exact recovery copies with exclusive names. the copies must be flushed before the first metadata replacement. a failed restoration retains them and reports their paths; their contents never enter diagnostics. a guard removes them on success or complete rollback.

payload durability belongs to [placement](placing.md#copy-durability). writing state only after copied bytes are flushed is part of the sync contract, not something `write_state` can enforce by itself.

## invariants and traps

- commit the manifest and lock, not local observations. a copied state file is not proof of another machine's files.
- do not treat a broken lock like missing state. recovery may require restoring the lock from git or explicitly resolving all entries again.
- a tree hash pins archive contents without expanding the lock to one record per member.
- root discovery uses the nearest manifest. nested projects are distinct roots.
- path checks reject traversal and links in protected directory chains, but do not make a sandbox against another process swapping directories during a run.

## tests

| files | cargo filter |
|---|---|
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project.rs), [manifest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/manifest.rs), [lock.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock.rs), [state.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/state.rs) | `cargo test --locked --lib project::` |
| [concurrent.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/concurrent.rs) | `cargo test --locked --test cli concurrent::` |
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/project.rs), [moved.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/moved.rs) | `cargo test --locked --test cli project::`; `cargo test --locked --test cli moved::` |

the lock reader models use fixed generated inputs and mutations to compare canonical and general TOML decoding, including escaped strings, integer limits, line endings, path rejection and deterministic rendering.

## related

[sync loop](sync-loop.md), [placing](placing.md), [data.toml](../../user/reference/data-toml.md), [data.lock](../../user/reference/data-lock.md), [project-folder records](../../user/reference/project-folder.md).

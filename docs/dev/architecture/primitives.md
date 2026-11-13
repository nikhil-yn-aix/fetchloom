# primitives

shared primitives keep byte identity, path rules, failure handling and worker limits consistent across components.

## ownership

these modules contain small value types and policies used by more than one component. they avoid command-specific decisions; their callers supply paths, settings and effects.

## files

| file | what it holds | types and functions |
|---|---|---|
| [digest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/digest.rs) | typed digests and incremental hashing | `Sha256`, `Md5`, `Hasher`, `from_hex`, `of_reader`, `sha256` |
| [name.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/name.rs) | suggested names, path checks, alias folding and `folder/name` as listings write it | `sanitize`, `dataset_name`, `title_name`, `reference_name`, `escapes`, `metadata`, `unstorable`, `fold`, `closest`, `joined` |
| [glob.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/glob.rs) | checked whole-path selection patterns | `Glob`, `new`, `matches`, `is_top_level_only` |
| [units.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/units.rs) | human sizes, rates, durations, counts and clipping, lists of names cut to the first few, and sizes typed as `512M` | `size`, `rate`, `speed`, `duration`, `count`, `grouped`, `clip`, `listed`, `some_of`, `bytes` |
| [volume.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/volume.rs) | filesystem capability and safe read-only removal | `Volume`, `can_hardlink`, `replace`, `remove_hardlink`, `is_plain`, `create_hidden` |
| [settings.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/settings.rs) | every setting with its origin: variable, settings file, resources level, machine or default | `Settings`, `File`, `Set`, `Origin`, `read`, `defaults`, `hash_batches` |
| [settings/resources.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/settings/resources.rs) | resources levels: what lean, default and max give the knobs they derive from the machine, and what a run spends once every knob is set | `Level`, `Limits`, `limits`, `Spending`, `spending` |
| [space.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/space.rs) | free space on the disk holding a folder: statvfs on unix, the volume list on windows; what that disk is formatted as, and the 4 GiB less one byte FAT32 holds in one file | `free`, `short`, `Filesystem`, `cache_holds` |
| [machine.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/machine.rs) | what the machine allows: processors, available memory on linux inside cgroup limits and whether a cgroup holds it, open files, and the processors the system has online | `Machine`, `read`, `processors_online` |
| [threads.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/threads.rs) | local worker ceilings | `Threads`, `read`, `limit`, `archives` |
| [error.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/error.rs) | stable failure kinds and explanations: details cut to the first few, a command's report so far, and the help for an answer only a newer fetchloom can read | `Kind`, `Error`, `FileFailure`, `name`, `exit_code`, `advice`, `details_up_to`, `with_report`, `report_answer`, `ISSUES` |
| [stop.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/stop.rs) | shared cancellation, as an `Error` or, for work on files, an `io::Error` | `Stop`, `ask`, `asked`, `is_asked`, `check`, `check_io` |
| [suggested.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/suggested.rs) | the fetchloom command a help or hint names, after `-C` and the folder the run works in when it was given `-C`, so it finds the same project from any folder | `command`, `set_directory` |
| [work.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/work.rs) | atomic counters independent of output | `Work`, `Counts`, `add`, `counts` |

## how it works

### identity and names

`Sha256` and `Md5` reject invalid encoded digests at construction. `Hasher` always computes SHA-256 and optionally MD5; callers that already have a SHA-256 pin avoid redundant MD5 work.

`sanitize` cleans suggested names; validation of manifest and locked paths instead refuses unsafe input. do not replace validation with sanitization, which could silently change a requested destination. `fold` uses ASCII lowercase for ASCII names, or NFC followed by lowercase, uppercase and lowercase for unicode collision detection. lowercasing first joins capital and small sharp s; the uppercase round trip also joins the two greek sigmas.

path components longer than 255 UTF-8 bytes are refused, except on windows, which counts the 255 in UTF-16 units as ntfs does. windows also rejects reserved device names, forbidden characters and trailing dots or spaces. metadata path checks keep data placements out of fetchloom and project-control files.

`Glob` compiles patterns once and matches the whole dataset-relative path. `*` and `?` stay inside one folder, `**` spans folders, and character sets match one character. `**` must occupy its own path component. patterns use `/` even on windows.

### worker budgets

`FETCHLOOM_THREADS` accepts 1 through 256; unset or empty preserves defaults. `Threads` limits each local worker group, not the total number of OS threads in a process. network writers, hash workers, DNS helpers and progress threads are separate.

| work | default and cap behavior |
|---|---|
| async runtime | up to four workers, reduced by the thread cap |
| archive readers | up to `min(4, ceil(logical CPUs / 4))`; an explicit cap counts only readers with an archive still pending before sharing the budget with their writers |
| archive writers | up to eight per archive, no more than logical CPUs; zero writer budget stores on the reader |
| placement, object lookup and survey hashing | up to eight workers, no more than logical CPUs, with at most one per 64 items |
| cache verification | logical CPU count, reduced by the cap |
| large fallback copies outside linux | uncapped copies overlap reads and writes with three reusable 1 MiB buffers; a cap avoids a reader thread per placement worker |

local placement, cache verification and archive workers use one fallible scoped-thread helper. refused work stays owned until the caller joins it; started workers finish normally and results preserve input order. archive readers store inline when no writer starts, and close their queue before joining fallback consumers. job panics still propagate.

before constructing the async runtime, the CLI starts and joins one fallible probe thread. an immediate refusal returns `local.io` (exit 50), including in JSON, without invoking the runtime builder. release builds abort on panics and retain the bug-report hook. the probe does not reserve capacity: a later refusal during tokio worker creation can still panic.

linux copies can use the kernel's `copy_file_range` through `io::copy`. `FETCHLOOM_JOBS` controls network work and does not replace these local budgets. listings, plans and member records still grow with file count.

### settings and resources levels

`Settings::read` takes each value from its variable, then the settings file, then the `resources` level, then the machine or fetchloom's default. `Level::limits` is a pure function of the level and `Machine`: lean gives jobs, host jobs, a local worker cap and a memory budget, max gives jobs and host jobs, and default gives none, so a run without a level reads exactly the defaults. a value a level gave carries `Origin::Resources`, which `settings` shows as `resources = lean` and JSON as `resources`.

the memory a level gives sizes only the hash queues of downloads (`hash_batches`). `most_files` and the archive limits stay sized from the memory set by name, else the default level's budget, so lean never refuses a listing or an archive that default accepts. `cli::run` narrows `Threads` to the level's worker cap only when `FETCHLOOM_THREADS` is unset or empty, before the runtime is built, so the variable wins as any setting set by name does.

`Settings::spending` turns the settings, the run's `Threads` and the processors online into what `-v` tells as a command that downloads, hashes or places files starts (`cli::spends` names them): downloads at once, to one host (never more than downloads at once), local workers (`Threads` over the processors this process may use), the memory budget and hash lanes, with a reason for each that a level, a variable, the settings file, the processors or the memory room holds. it is a pure function of its inputs, tested on a table of machines and settings; the processors online are read only at `-v`, from `/sys/devices/system/cpu/online`.

### volume, errors and cancellation

`Volume` learns hardlink support only when an operation needs the answer. on filesystems that cannot hardlink, a refused read-only removal may clear the flag and retry. a known hardlink never takes that path because the flag belongs to every name of the file.

`Kind` maps a stable string to an exit code. `Error` adds one message, details, help and optional partial results; it is hand-written rather than derived through an error framework. a refusal a sign-in could lift carries `auth::Need`, the account to sign in to, set by `Error::needs`, so `error` names `auth`'s types while `auth` returns `Error`. `Stop` shares a `tokio::sync::watch` channel carrying a boolean. `ask` uses `send_replace(true)`, `is_asked` reads the current value synchronously, and `asked` subscribes and waits asynchronously for it to become true.

## invariants and traps

- `Sha256::of_file` is a general path reader. cache integrity checks use `read_unfollowed` plus `of_reader` where following a final link is forbidden.
- a cap of one must not deadlock an archive reader waiting for a writer that was never started.
- cancellation must interrupt waits as well as work loops.
- counters describe performed operations, not intended work or rendered lines.
- display clipping must not split a unicode character.

## tests

hash-pipeline tests pause the actual worker with channel messages, check queue capacity with `try_send`, and observe claim lifetimes through weak handles while the worker is known to be draining. timeouts only bound failed tests; they are not evidence that an operation waited.

CLI resource-cap tests check accepted settings, configuration errors and complete output bytes. they do not infer token accounting or worker counts from elapsed time, which also includes scheduling and filesystem waits.

| files | cargo filter |
|---|---|
| [digest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/digest.rs), [name.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/name.rs), [glob.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/glob.rs), [units.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/units.rs) | `cargo test --locked --lib digest::`; `cargo test --locked --lib name::`; `cargo test --locked --lib glob::`; `cargo test --locked --lib units::` |
| [volume.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/volume.rs), [threads.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/threads.rs), [error.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/error.rs) | `cargo test --locked --lib volume::`; `cargo test --locked --lib threads::`; `cargo test --locked --lib error::` |
| [caps.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/caps.rs), [unattended.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/unattended.rs), [placing.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/placing.rs) | `cargo test --locked --test cli caps::`; `cargo test --locked --test cli unattended::`; `cargo test --locked --test cli placing::` |

## related

[placing](placing.md), [archives](archives.md), [output](output.md), [environment](../../user/reference/environment.md), [exit codes](../../user/reference/exit-codes.md).

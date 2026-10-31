# primitives

shared primitives keep byte identity, path rules, failure handling and worker limits consistent across components.

## ownership

these modules contain small value types and policies used by more than one component. they avoid command-specific decisions; their callers supply paths, settings and effects.

## files

| file | what it holds | types and functions |
|---|---|---|
| [digest.rs](../../../src/digest.rs) | typed digests and incremental hashing | `Sha256`, `Md5`, `Hasher`, `from_hex`, `of_reader`, `sha256` |
| [name.rs](../../../src/name.rs) | suggested names, path checks and alias folding | `sanitize`, `dataset_name`, `escapes`, `metadata`, `unstorable`, `fold`, `closest` |
| [glob.rs](../../../src/glob.rs) | checked whole-path selection patterns | `Glob`, `new`, `matches`, `is_top_level_only` |
| [units.rs](../../../src/units.rs) | human sizes, rates, durations and clipping | `size`, `rate`, `speed`, `duration`, `count`, `clip` |
| [volume.rs](../../../src/volume.rs) | filesystem capability and safe read-only removal | `Volume`, `can_hardlink`, `replace`, `remove_hardlink`, `is_plain`, `create_hidden` |
| [threads.rs](../../../src/threads.rs) | local worker ceilings | `Threads`, `read`, `limit`, `archives` |
| [error.rs](../../../src/error.rs) | stable failure kinds and explanations | `Kind`, `Error`, `FileFailure`, `name`, `exit_code`, `advice` |
| [stop.rs](../../../src/stop.rs) | shared cancellation | `Stop`, `ask`, `asked`, `is_asked`, `check` |
| [work.rs](../../../src/work.rs) | atomic counters independent of output | `Work`, `Counts`, `add`, `counts` |

## how it works

### identity and names

`Sha256` and `Md5` reject invalid encoded digests at construction. `Hasher` always computes SHA-256 and optionally MD5; callers that already have a SHA-256 pin avoid redundant MD5 work.

`sanitize` cleans suggested names; validation of manifest and locked paths instead refuses unsafe input. do not replace validation with sanitization, which could silently change a requested destination. `fold` uses ASCII lowercase for ASCII names, or NFC followed by uppercase and lowercase for unicode collision detection.

path components longer than 255 UTF-8 bytes are refused on every platform. windows also rejects reserved device names, forbidden characters and trailing dots or spaces. metadata path checks keep data placements out of fetchloom and project-control files.

`Glob` compiles patterns once and matches the whole dataset-relative path. `*` and `?` stay inside one folder, `**` spans folders, and character sets match one character. `**` must occupy its own path component. patterns use `/` even on windows.

### worker budgets

`FETCHLOOM_THREADS` accepts 1 through 256; unset or empty preserves defaults. `Threads` limits each local worker group, not the total number of OS threads in a process. network writers, hash workers, DNS helpers and progress threads are separate.

| work | default and cap behavior |
|---|---|
| async runtime | up to four workers, reduced by the thread cap |
| archive readers | up to `min(4, ceil(logical CPUs / 4))`; the cap shares a budget between active readers and their writers |
| archive writers | up to eight per archive, no more than logical CPUs; zero writer budget stores on the reader |
| placement, object lookup and survey hashing | up to eight workers, no more than logical CPUs, with at most one per 64 items |
| cache verification | logical CPU count, reduced by the cap |
| large fallback copies outside linux | uncapped copies overlap reads and writes with three reusable 1 MiB buffers; a cap avoids a reader thread per placement worker |

linux copies can use the kernel's `copy_file_range` through `io::copy`. `FETCHLOOM_JOBS` controls network work and does not replace these local budgets. listings, plans and member records still grow with file count.

### volume, errors and cancellation

`Volume` learns hardlink support only when an operation needs the answer. on filesystems that cannot hardlink, a refused read-only removal may clear the flag and retry. a known hardlink never takes that path because the flag belongs to every name of the file.

`Kind` maps a stable string to an exit code. `Error` adds one message, details, help and optional partial results; it is hand-written rather than derived through an error framework. `Stop` combines an atomic flag with async notification so workers can poll and waiting tasks can wake.

## invariants and traps

- `Sha256::of_file` is a general path reader. cache integrity checks use `read_unfollowed` plus `of_reader` where following a final link is forbidden.
- a cap of one must not deadlock an archive reader waiting for a writer that was never started.
- cancellation must interrupt waits as well as work loops.
- counters describe performed operations, not intended work or rendered lines.
- display clipping must not split a unicode character.

## tests

| files | cargo filter |
|---|---|
| [digest.rs](../../../src/digest.rs), [name.rs](../../../src/name.rs), [glob.rs](../../../src/glob.rs), [units.rs](../../../src/units.rs) | `cargo test --locked --lib digest::`; `cargo test --locked --lib name::`; `cargo test --locked --lib glob::`; `cargo test --locked --lib units::` |
| [volume.rs](../../../src/volume.rs), [threads.rs](../../../src/threads.rs), [error.rs](../../../src/error.rs) | `cargo test --locked --lib volume::`; `cargo test --locked --lib threads::`; `cargo test --locked --lib error::` |
| [caps.rs](../../../tests/cli/caps.rs), [unattended.rs](../../../tests/cli/unattended.rs), [placing.rs](../../../tests/cli/placing.rs) | `cargo test --locked --test cli caps::`; `cargo test --locked --test cli unattended::`; `cargo test --locked --test cli placing::` |

## related

[placing](placing.md), [archives](archives.md), [output](output.md), [environment](../../user/reference/environment.md), [exit codes](../../user/reference/exit-codes.md).

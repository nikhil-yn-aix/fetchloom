# cache

`cache` reports shared storage, collects unneeded bytes and checks stored objects.

```sh
fetchloom cache
fetchloom cache gc
fetchloom cache verify
```

## usage

```sh
fetchloom cache [options] [command]
fetchloom cache gc [options]
fetchloom cache verify [options]
```

`cache gc --now` also frees unneeded objects and unpack records less than a day old. `cache gc --dry-run`, or `-n`, shows what gc would free and deletes nothing. [global options](README.md#global-options) apply to all three forms.

## inspect

`fetchloom cache` reports the cache path, the bytes it holds and the projects using it. it prints the bare path on stdout only when stdout is not a terminal or quiet mode hides the story; at a terminal the story already shows it. JSON returns it in `result.path`.

byte totals exclude indexes and partial downloads. downloads a run stopped before they finished keep their bytes under `partial/`, so a later run can continue them; `fetchloom cache` names how many there are and what they hold in an `Unfinished` line, and JSON reports them as `result.unfinished_downloads` and `result.unfinished_bytes`. what they hold is the space they take on the disk: a download split into parts writes each part where it belongs, so until it finishes its file can be far longer than what the disk holds for it. a full disk under the cache names what they hold too, in the error. [cache folder](../reference/cache-folder.md) gives default locations and record formats; `FETCHLOOM_CACHE` overrides the root.

## gc

1. read registered project locks to find needed objects and unpacked members.
2. remove unneeded objects and unpack records once they are at least 24 hours old by modification time, not last use; `--now` removes this grace period.
3. clean unfinished downloads nobody came back for in a day, or with `--now` every one no running fetchloom holds, old temporary files and the lock files of downloads nobody holds, and forget links whose bytes are gone, then report deleted counts and bytes.
4. report unneeded objects kept because they are less than a day old.

```sh
fetchloom cache gc --now
```

`--now` ignores modification times for unneeded objects and unpack records, including future times that FAT timestamp rounding can produce. `--now` also removes unfinished downloads that no running fetchloom holds, however new, so the next run of those downloads starts over; a download in progress keeps its bytes, since its run holds its lock. temporary files keep their one-day grace period even with `--now`.

the kept-file hint names the retained bytes:

```text
  hint: 4.1 KiB no project needs is kept for a day in case you want it back; `fetchloom cache gc --now` frees it now
```

a gc dry run shows the same hint. `cache gc` JSON reports these young unneeded objects as `result.kept_objects` and `result.kept_bytes`, excluding objects a registered project needs. plain `fetchloom cache` ends with a similar hint, but its JSON reports `bytes` and `needed`: their difference is the unneeded payload, regardless of age.

successful project sync registers the project, including sync through `add` or `path`. `get` and standalone `lock` do not register. a registration warning means gc may not keep that project's bytes; check permissions and sync again.

`cache gc` removes a registration when its manifest is missing, as when a project folder moved, and then frees what only that project needed. it names each project it forgets this way, in a `Forgot` line and in `result.forgotten`; a dry run names them with `-v` and in `result.forgotten`. run `fetchloom sync` in a project's new folder to register it again. a failed registration removal warns, appears in `result.not_removed` and returns `local.io` (exit 50), even if object cleanup succeeded. inspection skips that project but keeps its registration, in case its drive is disconnected. an unreadable registration, manifest or lock stops inspection or gc with an error naming the path. inaccessible paths do not count as missing.

removing a cache object leaves placed files intact. removed bytes are not necessarily freed disk space because a hardlink can still hold them.

gc warns about paths it could not remove or inspect for cleanup. JSON retains successful counts and failure records in `result.not_removed`, alongside the error. each record has `path`, `kind`, `os_code` and `message`. follow the cause-specific advice: fix access for permission errors, or check storage for I/O errors, then retry. already absent paths do not count as failed removals.

### runs that use the cache

every command that opens the cache to work with it, such as `add`, `sync`, `get`, `cache` and `cache verify`, holds the empty `gc.lock` file in the cache folder with a shared OS lock while it works, and `cache gc` takes that lock alone, as cargo and uv lock their caches. so gc never removes an object that a running command has found and is about to place. while another command uses the cache, gc prints `Waiting for other fetchloom runs to finish with the cache` and starts once they have; a command started during gc waits for it in the same way. ctrl-c stops either wait. dry runs, `cache gc --dry-run` among them, take no lock and never wait, so what a gc dry run lists can differ from what a gc run later frees. when the filesystem explicitly reports that locks are unsupported, shared-cache commands can continue, but gc refuses to delete without an exclusive lock. other lock errors stop the command. gc also refuses when a read-only cache root prevents creating its lock. a read-only `gc.lock` can still protect shared readers; on NFS, gc needs write access to that file to take its exclusive lock.

### reminder

fetchloom does not automatically collect unneeded payload objects, apart from a packed archive no project needs once its files are unpacked. abandoned staging files are cleaned during unpacking or gc. freeing bytes is `cache gc`'s, which you run on purpose. so the cache cannot grow unnoticed, after an `add`, `sync`, `remove` or `get` at a terminal, at most once a week, fetchloom checks what no project needs and, when files gc would free total 1 GiB or more, says so once:

```text
  hint: the cache holds 3.2 GiB no project needs; `fetchloom cache gc` frees it
```

scripts, CI and coding agents never see it, since it only runs when a person can read it. with `-q` or `FETCHLOOM_NO_HINTS=1`, which hide hints, it does not read the cache at all. the first run only starts the weekly clock.

### dry run

```sh
fetchloom cache gc --now --dry-run
```

`--dry-run` runs the same sweep with every deletion recorded instead of made, so it applies the same age rules and keeps the same files. the cache, partial downloads and project registrations stay as they are, and a cache that does not exist yet is not created. explicit `FETCHLOOM_TRACE` output is still created or truncated. stderr when one unneeded 32-byte object is in the cache, matching a test:

```text
 Would free 1 file · 32 B from the cache
         Ok dry run, nothing was changed
```

`-v` names each path gc would delete, including the registrations of projects whose `data.toml` is gone. a path it cannot remove is named in a warning and does not fail the dry run; an object folder or project registration it cannot read stops it, as it stops gc. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## verify

1. read each cached object's metadata and rehash its bytes.
2. note each changed file, so the next download that needs it replaces it.
3. report examined counts, changed hashes and unreadable paths.

unreadable objects stay unmarked with warnings. failures to write change marks are also reported. JSON retains counts and findings in `result` beside the error. unreadable directories can hide objects from the totals; counts cover only objects whose metadata could be read.

sync and get refuse unreadable change marks with `local.io` (exit 50), because a read failure cannot show that cached bytes are safe. fix the named path's storage or permissions, verify the cache and retry; do not delete the marks to bypass the error.

verification does not check independent copies in `data/`. after it finds changed bytes, run `fetchloom sync` in each project that needs the affected files: a file placed as a hardlink to the changed copy is put back without `--force`, even in the project whose link was edited, since that edit changed the copy every project shares; the changed copy stays beside it as `<path>.changed`. see [status](status.md#categories). cache scans check cancellation between entries; verification also checks between hash reads.

## exit codes

| result | exit |
|---|---|
| successful inspection, gc or verification | 0 |
| verification found changed bytes | 30, `integrity.mismatch` |
| verification found unreadable paths but no changed bytes | 50, `local.io` |
| gc could not acquire its exclusive cache lock | 50, `local.io`; nothing is removed |
| gc could not remove or inspect a cleanup path | 50, `local.io`, even after other removals succeeded |
| interrupted work | 130 |

[exit codes](../reference/exit-codes.md) lists usage and other local failures.

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a recent unused object remains | gc uses a 24-hour age rule | collect later or use `cache gc --now` |
| `get` data is collected | one-off downloads do not register a project | use a project for inputs whose cache retention matters |
| a damaged independent copy passes cache verification | only cache objects are checked | inspect that project's files separately |
| gc reports errors after deleting files | cleanup can partially succeed | inspect the causes in `not_removed` and retry after fixing them |

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

`cache gc --now` also frees unneeded objects and unpack records less than a day old. [global options](README.md#global-options) apply to all three forms.

stdout excerpt from `fetchloom --help`:

```text
  cache   show the cache, free what no project needs, or check every file in it
```

## inspect

1. open the configured cache and inspect objects and registered project locks.
2. report the cache path, payload sizes and registered projects in the story.
3. print the bare cache path on stdout only when stdout is not a terminal or quiet mode hides the story.

at a terminal, the story's cache line already shows the path. JSON returns it in `result.path`.

byte totals exclude indexes and partial downloads. [cache folder](../reference/cache-folder.md) gives default locations and record formats; `FETCHLOOM_CACHE` overrides the root.

## gc

1. read registered project locks to find needed objects and unpacked members.
2. remove unneeded objects and unpack records once they are at least 24 hours old by modification time, not last use; `--now` removes this grace period.
3. clean abandoned partial and temporary files, then report deleted counts and bytes.
4. report unneeded objects kept because they are less than a day old.

```sh
fetchloom cache gc --now
```

`--now` ignores modification times for unneeded objects and unpack records, including future times that FAT timestamp rounding can produce. partial downloads and temporary files keep their one-day grace period even with `--now`. active partial-download locks are respected.

the kept-file summary gives the retained bytes and file count and says they become eligible when a day old. JSON reports them as `result.kept_objects` and `result.kept_bytes`. these counts exclude objects a registered project needs.

successful project sync registers the project, including sync through `add` or `path`. `get` and standalone `lock` do not register. a registration warning means gc may not keep that project's bytes; check permissions and sync again.

a missing manifest removes a registration. an unreadable registration, manifest or lock stops inspection or gc with an error naming the path. inaccessible paths do not count as missing.

removing a cache object leaves placed files intact. removed bytes are not necessarily freed disk space because a hardlink can still hold them.

gc warns about paths it could not remove or inspect for cleanup. JSON retains successful counts and failed paths in `result.not_removed`, alongside the error. close programs holding those paths or fix permissions, then retry. already absent paths do not count as failed removals.

## verify

1. read each cached object's metadata and rehash its bytes.
2. mark changed objects for replacement on a later download.
3. report examined counts, changed hashes and unreadable paths.

unreadable objects stay unmarked with warnings. failures to write change marks are also reported. JSON retains counts and findings in `result` beside the error. unreadable directories can hide objects from the totals; counts cover only objects whose metadata could be read.

verification does not check independent copies in `data/`. after repairing cache damage, sync each project that needs the affected files. a project whose own linked file shows changed bytes keeps it as an edit; save any needed changes and use `fetchloom sync --force` there. see [shared edits](sync.md#local-changes). cache scans check cancellation between entries; verification also checks between hash reads.

## exit codes

| result | exit |
|---|---|
| successful inspection, gc or verification | 0 |
| verification found changed bytes | 30, `integrity.mismatch` |
| verification found unreadable paths but no changed bytes | 50, `local.io` |
| gc could not remove or inspect a cleanup path | 50, `local.io`, even after other removals succeeded |
| interrupted work | 130 |

[exit codes](../reference/exit-codes.md) lists usage and other local failures.

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a recent unused object remains | gc uses a 24-hour age rule | collect later or use `cache gc --now` |
| `get` data is collected | one-off downloads do not register a project | use a project for inputs whose cache retention matters |
| a damaged independent copy passes cache verification | only cache objects are checked | inspect that project's files separately |
| gc reports errors after deleting files | cleanup can partially succeed | inspect `not_removed` and retry after fixing access |

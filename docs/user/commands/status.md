# status

`status` lists differences between the project's pins and local data without downloading anything.

```sh
fetchloom status
fetchloom status --json
```

## usage

```sh
fetchloom status [options]
```

there are no command-specific options. [global options](README.md#global-options) apply.

## what happens

1. read the manifest, lock and saved placement state.
2. compare file sizes and modification times, hashing where those records need checking.
3. report changed manifest entries, missing or different files, extra paths and unavailable archive trees.

status changes no project files. it is not a full integrity scan. [state records](../reference/project-folder.md#placement-state) explains when metadata can be trusted.

stderr when `iris.csv` is missing and `notes.txt` is extra:

```text
    Missing data/iris/iris.csv
      Extra data/iris/notes.txt
      Found 2 differences from data.lock
  hint: `fetchloom sync` places missing or updated files and reports any conflicts it keeps
```

## categories

| JSON field | difference |
|---|---|
| `unlocked` | a manifest entry is missing from the lock or has a changed spec |
| `removed` | a locked dataset is no longer in the manifest |
| `missing` | a target path is absent |
| `edited` | a managed path treated as locally changed; a metadata change is ignored when the file already holds the wanted bytes |
| `untracked` | a target exists without a state record |
| `outdated` | an unchanged managed file needs a new pin or writable placement |
| `edited_elsewhere` | a hardlink to the cache's copy holds other bytes, because an edit through a link, here or in another project, changed that shared copy |
| `extra` | a scanned path is outside known targets |
| `not_unpacked` | an archive's member record is unavailable here |

files modified too close to the state write are hashed because a coarse filesystem clock can hide an edit. that check can detect changed bytes even when the saved size and modification time still match.

files placed as hardlinks are the cache's own copy of their bytes, which every project using that file shares. an edit made in place through any of those links, after making the read-only file writable, changes that shared copy, so status reports the file as damaged rather than as your edit, and a plain `fetchloom sync` puts the pinned file back:

```text
    Damaged data/iris/iris.csv · linked to the cache's copy, which was changed
```

this holds while the file is still the cache's copy, or the very copy the cache found changed, even after another project's sync gave the cache a new one. a file saved anew, as most editors save, is your own file and stays an edit. a changed hardlink whose shared copy the cache no longer knows appears as edited, followed by `· changed through a hardlink other projects may share`; sync keeps it with reason `shared`, as [local changes](sync.md#local-changes) describes.

human output shows at most 20 paths per file category. unlocked or removed datasets and not-unpacked archives are not truncated. JSON includes every difference. without an archive record, files placed from it can also appear as extra.

## exit codes

0 means status completed, even when it found differences. scripts must inspect all nine arrays. invalid usage exits 2; local and project errors use 50. see [exit codes](../reference/exit-codes.md) for all failure kinds.

## common mistakes

| symptom | cause | fix |
|---|---|---|
| exit 0 despite missing files | differences are results, not command failures | inspect arrays in JSON |
| a file shows as damaged that you did not touch | an edit through another project's link changed the cache's copy you share | run `fetchloom sync`; use `copy = true` on datasets you edit |
| fewer paths appear in the terminal | human categories are limited to 20 paths | use `--json` |
| an unchanged file is outdated after `copy = true` | it still needs writable placement | run `fetchloom sync` |

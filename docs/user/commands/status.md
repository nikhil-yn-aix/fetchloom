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
| `edited` | a managed file's size or modification time changed |
| `untracked` | a target exists without a state record |
| `outdated` | an unchanged managed file needs a new pin or writable placement |
| `edited_elsewhere` | a shared hardlink was marked changed and its bytes differ |
| `extra` | a scanned path is outside known targets |
| `not_unpacked` | an archive's member record is unavailable here |

a visibly changed hardlink appears as edited, with a story explaining that it is shared with the cache through a link. sync preserves it with reason `shared`; [local changes](sync.md#local-changes) covers restoration and the narrower `edited_elsewhere` repair case.

human output shows at most 20 paths per file category. unlocked or removed datasets and not-unpacked archives are not truncated. JSON includes every difference. without an archive record, files previously placed from it can also appear as extra.

## exit codes

0 means status completed, even when it found differences. scripts must inspect all nine arrays. invalid usage exits 2; local and project errors use 50. see [exit codes](../reference/exit-codes.md) for all failure kinds.

## common mistakes

| symptom | cause | fix |
|---|---|---|
| exit 0 despite missing files | differences are results, not command failures | inspect arrays in JSON |
| a shared edit is not yet visible on windows | another project's hardlink change has not been detected | sync that project or run `cache verify`; use writable copies for edits |
| fewer paths appear in the terminal | human categories are limited to 20 paths | use `--json` |
| an unchanged file is outdated after `copy = true` | it still needs writable placement | run `fetchloom sync` |

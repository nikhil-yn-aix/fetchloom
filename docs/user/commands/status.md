# status

`status` lists differences between the project's pins and local data without downloading anything.

```sh
fetchloom status
fetchloom status --verify
fetchloom status --json
```

## usage

```sh
fetchloom status [options]
```

| option | effect |
|---|---|
| `--verify` | check the SHA-256 of every placed file, so a change that kept its size and time shows too |

[global options](README.md#global-options) also apply.

## what happens

1. read the manifest, lock and saved placement state.
2. compare file sizes and modification times, hashing where those records need checking.
3. report changed manifest entries, missing or different files, extra paths, unavailable archive trees and recipe results nothing here holds.

a project still as the last complete sync left it, with `data.toml` and `data.lock` unchanged byte for byte and every placed file looking as placed, is settled, as `sync` finds it: status lists only files fetchloom did not place as extra. that holds even when the cache no longer records what an archive held, as after moving the project and running `fetchloom cache gc --now` before any sync there.

status changes no project files. without `--verify` it is not a full integrity scan: when nothing differs, its last line says what it compared, `Ok everything matches data.lock by size and time`, since a change that keeps a file's size and modification time does not show that way. `--verify` hashes every placed file, lists the ones whose bytes changed as damaged, and ends with `Ok everything matches data.lock, checked byte for byte` when none did; `fetchloom sync --verify` puts them back. [state records](../reference/project-folder.md#placement-state) explains when metadata can be trusted.

stderr when `iris.csv` is missing and `notes.txt` is extra:

```text
    Missing data/iris/iris.csv
      Extra data/iris/notes.txt
      Found 2 differences from data.lock
  hint: `fetchloom sync` places missing or updated files and reports any conflicts it keeps
```

stderr in a fresh clone whose cache holds neither the recipe `clean`'s result nor `raw`, which has `place = false` and which clean is made from:

```text
 Not placed raw · place = false in data.toml
    Unbuilt clean · its pinned result is not on this machine or a mirror, nor is raw, which it is made from
      Found 1 difference from data.lock
  hint: `fetchloom build clean` makes it on this machine
```

an `exclude` in a dataset that names a file inside an archive leaves the file where it is, since a dataset's patterns choose among the files its source lists. status then adds a hint naming the [`fetchloom drop`](drop.md) that leaves the file out, as in `hint: exclude "data/train.csv" in datasets.dataset names a file inside dataset.zip, which exclude does not reach; run \`fetchloom drop data/dataset/data/train.csv\``. status shows it when it plans, as it does after data.toml or the data folder changed, and sync warns the same way.

## categories

| JSON field | difference |
|---|---|
| `unlocked` | a manifest entry is missing from the lock or has a changed spec or `extract`, or a [recipe](../recipes.md)'s result is not pinned as building it now would make; the line says what moved, such as `clean · clean.py changed`, and a hint points at `fetchloom build --dry-run` instead of sync |
| `removed` | a locked dataset is no longer in the manifest |
| `candidates` | a recipe has a kept rebuild, files a build made that differ from what data.lock pins for the same key; the line names [`fetchloom build --accept`](build.md#a-rebuild-that-differs), which pins it. it is no difference from data.lock, so it leaves the count alone |
| `missing` | a target path is absent. sync puts it back and says so; to leave a file out for good, run [`fetchloom drop`](drop.md) |
| `edited` | a managed path treated as locally changed; a metadata change is ignored when the file already holds the wanted bytes |
| `untracked` | a target exists without a state record |
| `outdated` | an unchanged managed file needs a new pin or a mode that fits its dataset: writable for `copy = true`, or, for a link to the cache's copy someone made writable, read-only again |
| `damaged` | a managed file holds other bytes, though no edit of this project's shows: a hardlink to the cache's copy, still shared with it or with other projects, after an edit through a link, here or in another project, changed that shared copy; or a file that shares its bytes with nothing, a copy, a reflink or a link left with a name of its own, whose size and modification time are unchanged, which hashing every placed file finds and a change mark puts in doubt too |
| `blocked` | an entry stands in a file's way: a link, a special file or a folder where the file belongs, or a file or link where a folder on its way belongs |
| `extra` | a scanned path is outside known targets, such as a file sync placed of a dataset that now has `place = false`, which the next sync removes |
| `not_unpacked` | an archive's member record is unavailable here |
| `unbuilt` | a recipe's pinned result has files missing from the data folder, or placed for an older pin, whose bytes neither the cache nor a folder [mirror](../mirrors.md) holds, so a sync leaves it out and only [`fetchloom build`](build.md) makes it here. its files are in no other list; the line names the datasets it is made from that the cache lacks too, which the build fetches first, and a hint names the build. status asks no web mirror, so with one set it lists the files as missing, since sync asks it |
| `unplaced` | no difference: a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read), which has no files in the data folder; a faint `Not placed` line names it |

files modified too close to the state write are hashed because a coarse filesystem clock can hide an edit. that check can detect changed bytes even when the saved size and modification time still match.

files placed as hardlinks are the cache's own copy of their bytes, which every project using that file shares. an edit made in place through any of those links, after making the read-only file writable, changes that shared copy, so status reports the file as damaged rather than as your edit, and a plain `fetchloom sync` puts the pinned file back, keeping the changed copy beside it as `<path>.changed`:

```text
    Damaged data/iris/iris.csv · linked to the cache's copy, which was changed
```

this holds while the file is still the cache's copy, or the very copy the cache found changed, even after another project's sync gave the cache a new one. a file saved anew, as most editors save, is your own file and stays an edit. a changed hardlink whose shared copy the cache no longer knows appears as edited, followed by `· changed through a hardlink other projects may share`; sync keeps it with reason `shared`, as [local changes](sync.md#local-changes) describes.

an entry in a file's way is listed once as blocked, rather than each file behind it as missing, with what it is:

```text
    Blocked data/iris · a file is where a folder belongs
```

a plain `fetchloom sync` leaves it and warns. `fetchloom sync --force` removes it, a link itself and never what it points to, then places the files; a folder that holds anything stays even then, since its files may be your work. a file fetchloom placed that no dataset needs now, which sync removes anyway, blocks nothing: it is listed as extra, and the files behind it as missing.

human output shows at most 20 paths per file category. unlocked or removed datasets and not-unpacked archives are not truncated. JSON includes every difference. once something changed since the last complete sync, files placed from an archive this machine has no record of can also appear as extra.

## exit codes

0 means status completed, even when it found differences. scripts must inspect every array. invalid usage exits 2; local and project errors use 50. see [exit codes](../reference/exit-codes.md) for all failure kinds.

## common mistakes

| symptom | cause | fix |
|---|---|---|
| exit 0 despite missing files | differences are results, not command failures | inspect arrays in JSON |
| a file shows as damaged that you did not touch | an edit through another project's link changed the cache's copy you share | run `fetchloom sync`; use `copy = true` on datasets you edit |
| fewer paths appear in the terminal | human categories are limited to 20 paths | use `--json` |
| an unchanged file is outdated after `copy = true` | it still needs writable placement | run `fetchloom sync` |

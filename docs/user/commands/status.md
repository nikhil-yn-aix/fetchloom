# status

`status` lists how the data folder differs from what `data.lock` pins, without downloading anything.

```sh
fetchloom status
fetchloom status --verify
fetchloom status --check
fetchloom status --json
```

## usage

```sh
fetchloom status [options]
```

| option | effect |
|---|---|
| `--verify` | check the SHA-256 of every placed file, so a change that kept its size and time shows too |
| `--check` | fail with `data.not_ready`, exit 1, when the data is not ready to use, naming each file or dataset with what makes it ready, as `sync --strict` does after a sync. it still changes nothing |

[global options](README.md#global-options) also apply.

## what happens

1. read `data.toml`, `data.lock` and what the last sync placed.
2. compare file sizes and modification times, hashing where those cannot vouch for a file.
3. report changed manifest entries, missing or different files, extra paths, unavailable archive trees and recipe results nothing here holds, one row on stdout for each, so `fetchloom status | grep Missing` or `fetchloom status > status.txt` gets them. the closing line, hints and warnings go to stderr; `-q` keeps the rows and drops the closing line and hints.
4. in a git repository, warn of each file git does not have that a clone needs: a list in [`data.lock.d`](../reference/data-lock.md#big-datasets) that data.lock names, and a file of a [recipe](../recipes.md)'s `code`, each with the `git add` that adds it. a code file `.gitignore` leaves out gets its own warning. with or without git, warn of each list in `data.lock.d` that data.lock no longer names, as after `git checkout -- data.lock` put an older one back, with the command that deletes it; status itself deletes nothing.

status changes nothing. without `--verify` it is not a full integrity check: when nothing differs, its last line says what it compared, `Ok everything matches data.lock by size and time`. `--verify` checks the hash of every placed file, lists the ones whose bytes changed as damaged, and ends with `Ok everything matches data.lock, checked byte for byte` when none did; `fetchloom sync --verify` puts them back. a project still as the last complete sync left it is settled, as `sync` finds it, and status lists only files fetchloom did not place.

bare `fetchloom` in a project prints the closing line alone, with one hint: `fetchloom sync` when a plain sync sets every difference right, as in a fresh clone, and `fetchloom status` when something needs a look first, such as an edited or untracked file.

when `iris.csv` is missing and `notes.txt` is extra, stdout holds the rows:

```text
    Missing data/iris/iris.csv
      Extra data/iris/notes.txt
```

and stderr the closing line and the hint:

```text
      Found 2 differences from data.lock
  hint: `fetchloom sync` places missing or updated files and reports any conflicts it keeps
```

in a fresh clone whose cache holds neither the recipe `clean`'s result nor `raw`, which has `place = false` and which clean is made from, stdout holds the row:

```text
    Unbuilt clean · its pinned result is not on this machine or a mirror, nor is raw, which it is made from
```

and stderr the rest, since a dataset with `place = false` is no difference:

```text
 Not placed raw · place = false in data.toml
      Found 1 difference from data.lock
  hint: `fetchloom build clean` makes it on this machine
```

## categories

| JSON field | difference |
|---|---|
| `unlocked` | a dataset data.lock does not pin as data.toml asks, or a [recipe](../recipes.md) whose result is not pinned as building it now would make; the line says what changed, such as `clean · clean.py changed`. in a project with no data.lock, each line says `data.lock does not exist`, and the hint says to ask whoever shares the project to commit it, or to run `fetchloom lock` |
| `removed` | a pinned dataset data.toml no longer has, as `Removed iris · not in data.toml, so \`fetchloom sync\` takes it out of data.lock`. a pin that answers a dataset data.toml has under another name, as after a rename, is no removal: that dataset is `unlocked`, and sync keeps the pin under its new name |
| `candidates` | a recipe has a kept rebuild that differs from what data.lock pins, which [`fetchloom build --accept`](build.md#a-rebuild-that-differs) pins; `-v` names the cache file holding each file that differs. it is no difference from data.lock, so it leaves the count alone |
| `missing` | a pinned file is absent. sync puts it back; to leave it out for good, run [`fetchloom drop`](drop.md) |
| `edited` | a file fetchloom placed was changed; a change of its time alone is ignored when it still holds the pinned bytes |
| `untracked` | a file fetchloom did not place stands at a path data.lock places one, with other bytes: sync keeps it, and `--force` replaces it |
| `adoptable` | a file fetchloom did not place stands at a path data.lock places one, and already holds the pinned bytes, so a plain sync [adopts](sync.md#already-in-place) it without `--force`; the line says so, as `Untracked data/raw/tips.csv · it holds the pinned bytes, so \`fetchloom sync\` adopts it` |
| `outdated` | an unchanged managed file needs a new pin or a mode that fits its dataset: writable for `copy = true`, or, for a link to the cache's copy someone made writable, read-only again |
| `damaged` | a file fetchloom placed holds other bytes, though no edit of this project's shows: a hardlink whose shared copy an edit through another link changed, or a file whose bytes changed under an unchanged size and time, which `--verify` finds |
| `blocked` | an entry stands in a file's way: a link, a special file or a folder where the file belongs, or a file or link where a folder on its way belongs |
| `extra` | a file in the data folder that no dataset places, which fetchloom did not place or which changed since, such as notes of your own: sync keeps it |
| `unneeded` | a file fetchloom placed that no dataset needs now and nobody changed, as one of a dataset gone from data.toml or one with `place = false`: the next sync removes it, and its line says so |
| `not_unpacked` | this machine has no list of an archive's files, as in a fresh clone, so they are not checked: `Unchecked data/dataset/dataset.zip · this machine has no list of the files it holds, so they are not checked`. the files fetchloom placed from it are neither extra nor unneeded until a sync reads the list |
| `unbuilt` | a recipe's pinned result is missing from the data folder, and neither the cache, the data folder nor a folder [mirror](../mirrors.md) holds it, so sync leaves it out and only [`fetchloom build`](build.md) makes it here. the line names what the build would fetch first. status asks no web mirror, so with one set it lists the files as missing instead |
| `unplaced` | no difference: a dataset with [`place = false`](../reference/data-toml.md#datasets-only-recipes-read), named on a faint `Not placed` line on stderr. files an earlier sync placed of it are `unneeded` until the next sync removes them, as `Not placed images · place = false in data.toml, so \`fetchloom sync\` removes its 13 files from data/images` |

a file changed too soon after the sync that placed it is hashed, since a coarse filesystem clock can hide an edit.

an edit through a hardlink, after making the file writable, changes the cache's copy that every project placing the file shares, so status reports the file as damaged rather than edited, and a plain `fetchloom sync` puts the pinned file back, keeping the changed copy as `<path>.changed`, as [local changes](sync.md#local-changes) describes:

```text
    Damaged data/iris/iris.csv · linked to the cache's copy, which was changed
```

a file saved anew, as most editors save, is your own file and stays an edit. a changed hardlink whose shared copy the cache no longer knows appears as edited, followed by `· changed through a hardlink other projects may share`.

an entry in a file's way is listed once as blocked, rather than each file behind it as missing:

```text
    Blocked data/iris · a file is where a folder belongs
```

a plain `fetchloom sync` leaves it and warns, and `fetchloom sync --force` removes it, a link itself and never what it points to; a folder that holds anything stays even then.

a pipe or a file gets every file on a row of its own, so `fetchloom status | grep part04` finds each one. at a terminal, a folder whose files are all gone is one row:

```text
    Missing data/many/part03/ · 30 files
    Missing data/many/part04/ · 30 files
```

and each kind shows at most 20 rows, then one that says how to see the rest, as ``and 5 more · `fetchloom status | less` shows all``.

datasets and archives are never cut short, and JSON includes every difference.

## exit codes

0 means status completed, even when it found differences, so scripts inspect every array in JSON, or ask for `--check`, which exits 1 with `data.not_ready` while the data is not ready to use: a dataset data.lock does not pin as data.toml asks, or no data.lock at all, a recipe that needs a build or whose result is not here, or a file data.lock places that is missing, outdated, damaged, edited, blocked or held by a file fetchloom did not place. extra files, those no dataset needs and datasets with `place = false` leave the data ready. invalid usage exits 2, and local and project errors 50; [exit codes](../reference/exit-codes.md) lists every failure kind.

## common mistakes

| symptom | cause | fix |
|---|---|---|
| `warning: data.lock no longer names data.lock.d/clean.450ae405bf79.tsv.zst, so nothing reads it` | a list a build or a big dataset wrote is left after `git checkout` or a reset put an older data.lock back | run the delete or `git rm` its help names; never `git add data.lock.d` before that |
| `warning: data.lock names data.lock.d/clean.450ae405bf79.tsv.zst, which git does not have` | a build or a big dataset wrote a new list, and `git commit -a` left it out | run `git add data.lock.d`, then commit |
| `warning: git does not have prep.py, code of clean` | a recipe's code was never added to git, so `git commit -a` left it out | run the `git add` the help names, then commit |

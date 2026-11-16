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

[global options](global-options.md) also apply.

## what happens

1. read `data.toml`, `data.lock` and what the last sync placed.
2. compare file sizes and modification times, hashing where those cannot vouch for a file.
3. report changed manifest entries, missing or different files, extra paths and unavailable archive trees, one row on stdout for each, so `fetchloom status | grep Missing` or `fetchloom status > status.txt` gets them. the closing line, hints and warnings go to stderr; `-q` keeps the rows and drops the closing line and hints.
4. in a git repository, warn of each file git does not have that a clone needs: a list in [`data.lock.d`](../reference/data-lock.md#big-datasets) that data.lock names, with the `git add` that adds it. with or without git, warn of each list in `data.lock.d` that data.lock no longer names, as after `git checkout -- data.lock` put an older one back, with the command that deletes it; status itself deletes nothing.

status changes nothing and asks no server: [`audit`](audit.md) asks the sources whether they still serve each pinned file. without `--verify` it is not a full integrity check: when nothing differs, its last line says what it compared, `Ok everything matches data.lock by size and time`. `--verify` checks the hash of every placed file, lists the ones whose bytes changed as damaged, and ends with `Ok everything matches data.lock, checked byte for byte` when none did; `fetchloom sync --verify` puts them back. a project still as the last complete sync left it is settled, as `sync` finds it, and status lists only files fetchloom did not place.

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

## categories

| JSON field | difference |
|---|---|
| `unlocked` | a dataset data.lock does not pin as data.toml asks. in a project with no data.lock, each line says `data.lock does not exist`, and the hint says to ask whoever shares the project to commit it, or to run `fetchloom lock` once each import's files are imported again. a dataset whose file list data.lock names and `data.lock.d` lacks, as in a clone of a commit that left it out, is here too, on a `Waiting` line, as `Waiting photos · its file list data.lock.d/photos.025d92b1be6b.tsv.zst is not there`, with a warning that says how to get it back; every other dataset is compared as before, as [big datasets](../reference/data-lock.md#big-datasets) says. no sync pins an import, since no source lists its files, so one data.lock holds no pin of, as a stop between the two writes of an add leaves one, or whose entry changed since it was pinned, says so, as `Unlocked big · imported from files, and data.lock holds no pin of them`, and a hint gives the steps that pin it |
| `removed` | a pinned dataset data.toml no longer has, as `Removed iris · not in data.toml, so \`fetchloom sync\` takes it out of data.lock`. a pin that answers a dataset data.toml has under another name, as after a rename, is no removal: that dataset is `unlocked`, and sync keeps the pin under its new name |
| `missing` | a pinned file is absent. sync puts it back; to leave it out for good, run [`fetchloom exclude`](exclude.md) |
| `edited` | a file fetchloom placed was changed; a change of its time alone is ignored when it still holds the pinned bytes |
| `untracked` | a file fetchloom did not place stands at a path data.lock places one, with other bytes, as `Untracked data/iris/iris.csv · holds other bytes than data.lock pins`: sync keeps it, and `--force` replaces it |
| `adoptable` | no difference: a file fetchloom did not place stands at a path data.lock places one and already holds the pinned bytes, as in a copy of the project or one last synced by an older version of fetchloom. a plain sync [records it as placed](sync.md#already-in-place) without `--force`, and one faint line on stderr counts them all, as `In place 4 files already hold the pinned bytes; \`fetchloom sync\` records them as placed` |
| `outdated` | an unchanged managed file needs a new pin or a mode that fits its dataset: writable for `copy = true`, or, for a link to the cache's copy someone made writable, read-only again. a read-only file that holds the pinned bytes where `copy = true` asks for writable copies is here too, though fetchloom did not place it, since sync replaces it |
| `damaged` | a file fetchloom placed holds other bytes, though no edit of this project's shows: a hardlink whose shared copy an edit through another link changed, or a file whose bytes changed under an unchanged size and time, which `--verify` finds |
| `blocked` | an entry stands in a file's way: a link, a special file or a folder where the file belongs, or a file or link where a folder on its way belongs |
| `extra` | a file in the data folder that no dataset places, which fetchloom did not place or which changed since, such as notes of your own: sync keeps it. files a system writes beside yours by itself, as finder's `.DS_Store` and `._` files or windows' `Thumbs.db` and `desktop.ini`, are no difference and in no list. what a system's folder holds, as a disk's trash or a `__MACOSX` folder, is extra, since it may be yours |
| `unneeded` | a file fetchloom placed that no dataset needs now and nobody changed, as one of a dataset gone from data.toml: the next sync removes it, and its line says so |
| `not_unpacked` | this machine has no list of an archive's files, as in a fresh clone, so they are not checked: `Unchecked data/dataset/dataset.zip · this machine has no list of the files it holds, so they are not checked`. the files fetchloom placed from it, and in a copy of the project every file in the folder it unpacks to, are neither extra nor unneeded until a sync reads the list |

a file changed too soon after the sync that placed it is hashed, since a coarse filesystem clock can hide an edit.

an edit through a hardlink, after making the file writable, changes the cache's copy that every project placing the file shares, so status reports the file as damaged rather than edited, and a plain `fetchloom sync` puts the pinned file back, keeping the changed copy as `<path>.changed`, as [local changes](sync.md#local-changes) describes:

```text
    Damaged data/iris/iris.csv · linked to the cache's copy, which was changed
```

after that sync, status lists the kept copy as extra and says what to do with it:

```text
      Extra data/iris/iris.csv.changed · a changed copy sync kept of data/iris/iris.csv: compare the two, keep what you need, then delete it
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

0 means status completed, even when it found differences, so scripts inspect every array in JSON, or ask for `--check`, which exits 1 with `data.not_ready` while the data is not ready to use: a dataset data.lock does not pin as data.toml asks, or no data.lock at all, a dataset that waits for its file list, or a file data.lock places that is missing, outdated, damaged, edited, blocked or held by a file fetchloom did not place. extra files and those no dataset needs leave the data ready. invalid usage exits 2, and local and project errors 50; [exit codes](../reference/exit-codes.md) lists every failure kind.

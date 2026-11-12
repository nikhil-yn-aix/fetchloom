# lock

`lock` records exact input files without placing them in `data/`, and merges `data.toml` and `data.lock` after a git merge.

```sh
fetchloom lock
fetchloom lock --check
fetchloom lock --upgrade iris flowers
fetchloom lock --upgrade
fetchloom lock --upgrade --dry-run
fetchloom lock --ours iris --theirs penguins
```

## usage

```sh
fetchloom lock [options]
```

| option | effect |
|---|---|
| `--check` | check manifest membership, spec hashes and `extract`, and that each recipe is built as data.lock pins it, without changing anything |
| `--upgrade [name ...]` | resolve named datasets again; with no names, consider every dataset |
| `-n`, `--dry-run` | list what locking would change without writing `data.lock` or downloading |
| `--ours <name>` | after a git merge or rebase, keep our side, as git names it, of a dataset the merge needs a choice for; repeat it for more |
| `--theirs <name>` | the same, keeping their side |

`--check` conflicts with `--upgrade`, and `--ours` and `--theirs` with both. [global options](README.md#global-options) also apply.

## what happens

1. read `data.toml` and the existing lock, if present.
2. keep entries whose specs and `extract` are unchanged, unless an upgrade was requested.
3. resolve changed entries and learn file sizes, hashes and archive trees. this may download data into the cache.
4. write `data.lock` without placing files or registering the project with the cache.

as in `sync`, each dataset pinned for the first time gets a `Pinned` line with what it resolved to, its title, file count and size, so a typo in an id that pins an unrelated record stands out. up to 20 get a line each; the rest, and changed pins, are counted in one `Locked` line. with `--json`, `result.datasets` names the datasets whose pins changed.

files already pinned need not be cached. a source that states a SHA-256 and size lets a file be locked without a download, but an archive that unpacks needs its tree hash, so it is downloaded.

a passing `lock --check` ends with `Ok data.lock matches data.toml` on stderr, and with `-q` prints nothing. it checks data.toml against the lock, not the files in the data folder. an empty data.toml passes even without a lock, while `sync --locked` needs `data.lock` to exist.

only `fetchloom build` pins a [recipe](../recipes.md), so `lock` keeps a recipe's pin while `data.toml` has the recipe, and drops it with the recipe. `lock --check` fails with `build.needed`, exit 60, when data.lock pins no result for a recipe or something it is made from changed since, after any `lock.outdated` finding. `lock` itself still exits 0 then: it warns about each such recipe as `sync` does, and its last line names the recipes that need a build. stderr after mc's code, c.py, changed, from a real run:

```text
warning: mc changed since it was built: c.py changed
  help: data.lock still pins the result built before; run `fetchloom build mc`, then commit data.lock and data.lock.d
         Ok data.lock pins the fetched datasets as data.toml asks; mc needs a build
```

## upgrading

`--upgrade` asks each source again for the latest version of its reference, and checks again each file no checksum pins, such as a plain link's, and each file whose source now states an MD5 that data.lock lacks. a bucket object whose listing gives the ETag and size this machine recorded is not downloaded again, as [buckets](../sources/buckets.md#pins) explains. a reference that names a version stays on it; edit `ref` to choose another.

an [import](add.md#files-on-this-machine) has no source with a newer version: `--upgrade <name>` naming one exits 10 with `reference.unresolved`, and `--upgrade` alone keeps it as data.lock pins it and says so with a `Skipped` line.

a change of `select` or `exclude` lists the version data.lock already resolved, not a newer one. an `extract` change alone keeps the pinned files and lists nothing again; it needs the network only for archives the cache lacks. changing `ref`, deleting the lock or an upgrade resolves the reference in data.toml anew. [the spec hash](../reference/data-lock.md#the-spec-hash) defines these rules.

after an upgrade, run `fetchloom sync` to place the new inputs, and commit `data.lock`. a pin that did not change keeps its `locked` time.

## after a git merge

a git merge that could not combine two changes leaves both sides in `data.toml` or `data.lock`, between git's `<<<<<<<`, `=======` and `>>>>>>>` lines. every other command then stops with `project.invalid` and names `fetchloom lock`, which merges both files one dataset at a time, as `pnpm install` merges a lockfile, without resolving or downloading anything:

- a dataset only one side added, changed or removed takes that side.
- a dataset both sides have alike stays. two pins of the same files made at different times are alike.
- a dataset each side changed its own way stops the merge with `unconfirmed`, exit 1, naming each one, and nothing is written. so does a dataset one side removed while the other pinned other files for it, by a re-import or `lock --upgrade`. those leave its entry in `data.toml` as it was, so git merges `data.toml` by itself and takes the removal, and only `data.lock` holds conflict lines.
- `--ours <name>` keeps our side of such a dataset and `--theirs <name>` their side, its entry in `data.toml` and its pin alike, each whole as that side's branch has it when git can tell: a removed dataset comes back with its comments and every key of its entry. `--yes` never picks a side.

ours and theirs are git's sides. in a merge, ours is the branch checked out and theirs the branch merged in. in a rebase, ours is the branch you rebase onto, with the commits already replayed, and theirs is your commit being replayed. a cherry-pick's theirs is the commit picked.

a dataset's pin follows its merged entry: one side's pin is kept only when it answers that entry, so a dataset whose entry no pin answers is left for the next `fetchloom sync` or `fetchloom lock` to pin, with a warning. a [recipe](../recipes.md)'s result merges as a pin does: taken from the side that built it anew, with a choice needed when both sides built it their own way, and dropped with the recipe; a merged data.toml whose recipes would not read, such as one side's recipe reading a dataset the other side removed, stops the merge and names why. the merge writes `data.toml`, the lists in `data.lock.d` the kept pins name, and `data.lock`, then removes the lists no pin names. `--dry-run` shows the merge without writing, and `--check` and `--upgrade` stop as other commands do until the merge is written. a dataset with the same values but other comments on each side keeps one side's text, with a warning naming it. `[project]` and comments outside the datasets must agree or have changed on one side only; otherwise the merge stops with `project.invalid`, exit 50, and that part is for a person: keep one side's lines between git's conflict lines, such as one `data_dir`, delete the other side's lines and the conflict lines, then run `fetchloom lock`. deleting `data.lock` to get past a conflict instead pins every dataset anew, which can pin newer data than either branch had.

telling an addition on one side from a removal on the other needs the merge base, what both branches started from. `lock` asks git for each side of a file, and for their base, from git's index or the commits being merged, so a side holds only the lines its branch wrote. git is found on `PATH` outside the project and runs with no shell, no input and no transport, under a time and size limit, so it never reaches the network.

without git, or once `git add` has marked the files resolved, `lock` reads the sides from the conflict lines, and the base from the `|||||||` section that git's `diff3` and `zdiff3` conflict styles write. without a base, every dataset the sides disagree on needs a choice; `git config merge.conflictStyle zdiff3` before merging or rebasing makes the conflict lines show it. a pin read from conflict lines that does not read whole, as when git merged in lines of the other side's pin, stops the merge with `project.invalid`, exit 50, and help to copy that side's table back by hand.

from real runs with git 2.43 on `PATH`, after two branches each added a dataset beside `uci` and a merge stopped with git's default conflict style. stderr:

```sh
$ fetchloom lock
     Merged data.toml and data.lock · iris from ours, penguins from theirs, 1 the same
```

the same merge with no git on `PATH`, where the conflict lines show no base:

```sh
$ fetchloom lock
error: the merge needs a choice for 2 datasets
  iris: only ours has it, and fetchloom cannot tell whether ours added it or theirs removed it
  penguins: only theirs has it, and fetchloom cannot tell whether theirs added it or ours removed it
  no merge base was found: git on PATH, before `git add` marks the files resolved, or `git config merge.conflictStyle zdiff3` before merging or rebasing, lets fetchloom see what each side changed
  help: choose a side for each with `fetchloom lock --ours <name>` or `fetchloom lock --theirs <name>`; both repeat
$ fetchloom lock --ours iris --theirs penguins
     Merged data.toml and data.lock · iris from ours, penguins from theirs, 1 the same
```

and with git, after one branch removed `udata` while the other ran `lock --upgrade udata`, merged either way:

```sh
$ fetchloom lock
error: the merge needs a choice for udata
  udata: one side changed it and the other removed it
  help: choose a side for each with `fetchloom lock --ours <name>` or `fetchloom lock --theirs <name>`; both repeat
```

`-v` first shows what each side pins of a dataset that needs a choice: a recipe result's key, how many files and bytes, and up to 3 of the files only that side pins, each with its size and the first 12 digits of its SHA-256. it picks no side. the same merge, on the branch that removed `udata`:

```sh
$ fetchloom -v lock
            udata, ours: not in data.lock
            udata, theirs: 1 file · 11 B
              u.csv · 11 B · sha256 2805e4684085
error: the merge needs a choice for udata
  udata: one side changed it and the other removed it
  help: choose a side for each with `fetchloom lock --ours <name>` or `fetchloom lock --theirs <name>`; both repeat
  kind: unconfirmed
            33 ms in all
```

run `fetchloom sync` next, then commit `data.toml` and `data.lock` to finish the merge.

with `--json`, `result.merge` has the arrays `from_ours`, `from_theirs`, `same`, `removed` and `unpinned`, by dataset name, and `result.datasets` names the pins that changed from our side's. an `unconfirmed` error keeps `result.merge` with `conflicts`, the datasets that need a choice. its `fix` is null, since only a person picks a side.

## dry run

`lock --dry-run`, or `-n`, lists what `lock` would change without writing `data.lock` or the cache and without downloading files; with `--upgrade`, it shows which datasets have newer versions. it still resolves entries, so a source may be asked what a link holds. its lines are those of a real lock, with `Would pin`, `Would lock`, `Would remove` and `Would upgrade`. `Would fetch` names each file a real lock would download to learn its hash, including, under `--upgrade`, a file no checksum pins, which the dry run counts as unchanged. `Would open` names each archive it would unpack. `--check --dry-run` is a check. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## exit codes

0 means pins were updated, a merge was written or a check passed. a stale lock under `--check` exits 1 with `lock.outdated`, a recipe data.lock does not pin as built now exits 60 with `build.needed`, and a merge that needs a choice exits 1 with `unconfirmed`. incompatible options exit 2, as do `--ours` and `--theirs` without conflict lines, or naming a dataset that needs no choice. conflict lines out of git's order, a side that does not read, a pin read from the conflict lines that does not read whole, or a `[project]` part that differs on both sides, exit 50 with `project.invalid`. resolution, download and local failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| `data/` has old files | lock does not place files | run `fetchloom sync` |
| an explicit version did not advance | the reference still names that version | edit `ref` deliberately, then lock |
| `--check` passes despite edited data | it checks specs, not contents | use `status`; use `cache verify` for cached objects |
| rebuilding a broken lock changes inputs | fresh resolution can choose newer data | restore the original lock from git when those pins matter |
| `data.toml has an unfinished git merge conflict`, or the same for `data.lock` | a git merge left both sides between its conflict lines | run `fetchloom lock`, which [merges them](#after-a-git-merge) |
| `the merge needs a choice` for a dataset only one side has | no merge base: git is not on `PATH`, or the files were already `git add`ed, and git's conflict lines show none, as its default style does | choose with `--ours` or `--theirs`, or merge again with `git config merge.conflictStyle zdiff3` |

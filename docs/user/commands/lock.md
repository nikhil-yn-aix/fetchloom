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
| `--ours <name>` | after a git merge, keep the checked-out branch's side of a dataset the two sides changed each their own way; repeat it for more |
| `--theirs <name>` | the same, keeping the merged branch's side |

`--check` conflicts with `--upgrade`, and `--ours` and `--theirs` with both. [global options](README.md#global-options) also apply.

## what happens

1. read `data.toml` and the existing lock, if present.
2. keep entries whose specs and `extract` are unchanged, unless an upgrade was requested.
3. resolve changed entries and learn file sizes, hashes and archive trees. this may download data into the cache.
4. write `data.lock` without placing files or registering the project with the cache.

each dataset pinned for the first time gets a `Pinned` line of its own, with what it resolved to, its title when the source gives one, its file count and its size, so a typo in an id that pins an unrelated record stands out. up to 20 first pins get a line each; the rest, and changed pins of datasets locked before, are counted in one `Locked` line.

with `--json`, `result.datasets` names the datasets whose pins changed, as `sync` does: pinned for the first time, pinned to other data, or no longer in `data.lock`.

already pinned ordinary files need not be cached. a source that supplies SHA-256 and size can allow locking without a download. an extracted archive needs its tree hash.

`lock --check` is silent on success at normal verbosity. it checks the manifest against the lock, not local file contents. an empty manifest passes even without a lock; `sync --locked` instead requires `data.lock` to exist.

a [recipe](../recipes.md) is pinned only by `fetchloom build`, so `lock` keeps a recipe's pin as it is while `data.toml` has the recipe, and drops it with the recipe. `lock --check` reads each recipe's code and fails with `build.needed`, exit 60, when data.lock pins no result for it or something it is made from moved since, after any `lock.outdated` finding. `lock` itself still exits 0 then: it warns about each such recipe as `sync` does, naming what moved, and its last line names the recipes that need a build instead of calling data.lock up to date. stderr after mc's code, c.py, changed, from a real run:

```text
warning: mc changed since it was built: c.py changed
  help: data.lock still pins the result built before; run `fetchloom build mc`, then commit data.lock and data.lock.d
         Ok data.lock pins the fetched datasets as data.toml asks; mc needs a build
```

## upgrading

`--upgrade` revalidates unchecksummed URLs and files whose source now states an MD5 that `data.lock` lacks. a specifically versioned reference still names that version. edit `ref` if you want another explicit version.

an [import](add.md#files-on-this-machine) has no source with a newer version: `--upgrade <name>` naming one exits 10 with `reference.unresolved`, and `--upgrade` alone keeps it as data.lock pins it and says so with a `Skipped` line.

filter changes resolve the existing `resolved` reference. an `extract` change alone keeps the pinned files and lists nothing again; it needs the network only for archives the cache lacks. changing `ref`, deleting the lock or requesting an upgrade resolves the manifest reference instead. [spec hashes](../reference/data-lock.md#the-spec-hash) defines these rules.

after an upgrade, run `fetchloom sync` to place the new inputs and commit `data.lock`. unchanged resolved references and file lists retain their previous `locked` timestamp.

## after a git merge

a git merge that could not combine two changes leaves both sides in `data.toml` or `data.lock`, between git's `<<<<<<<`, `=======` and `>>>>>>>` lines. every other command then stops with `project.invalid` and names `fetchloom lock`, which merges both files one dataset at a time, as `pnpm install` merges a lockfile, without resolving or downloading anything:

- a dataset only one side added, changed or removed takes that side.
- a dataset both sides have alike stays. two pins of the same files made at different times are alike.
- a dataset each side changed its own way stops the merge with `unconfirmed`, exit 1, naming each one, and nothing is written. `--ours <name>` keeps the checked-out branch's side and `--theirs <name>` the merged branch's, its entry in `data.toml` and its pin alike. `--yes` never picks a side.

a dataset's pin follows its merged entry: one side's pin is kept only when it answers that entry, so a dataset whose entry no pin answers is left for the next `fetchloom sync` or `fetchloom lock` to pin, with a warning. a [recipe](../recipes.md)'s result merges as a pin does: taken from the side that built it anew, with a choice needed when both sides built it their own way, and dropped with the recipe; a merged data.toml whose recipes would not read, such as one side's recipe reading a dataset the other side removed, stops the merge and names why. the merge writes `data.toml`, the lists in `data.lock.d` the kept pins name, and `data.lock`, then removes the lists no pin names. `--dry-run` shows the merge without writing, and `--check` and `--upgrade` stop as other commands do until the merge is written. a dataset with the same values but other comments on each side keeps one side's text, with a warning naming it; `[project]` and comments outside the datasets must agree, or the merge stops and asks for that part by hand.

telling an addition on one side from a removal on the other needs the merge base, what both branches started from. `lock` reads it from the `|||||||` section of git's `diff3` and `zdiff3` conflict styles, or else asks git for it from its index. that git must be 2.45 or later, found on `PATH` outside the project; it runs with no shell and no input, under a time and size limit, and with `--no-lazy-fetch`, so it never reaches the network. without a base, every dataset the sides disagree on needs a choice. `git config merge.conflictStyle zdiff3` before merging lets an older git show the base too. zdiff3 moves the lines both sides share at either end of a conflict out of it, such as the first lines of a dataset both branches added, though the base never had them; when the base read with those lines does not read as data.toml or data.lock, `lock` asks git as above, then reads it again without the tables a conflict cuts, so zdiff3 merges as `diff3` does. when the conflict lines show a base and none of these reads, the error names only git 2.45, never a conflict style already in use.

from a real run with git 2.43, after two branches each added a dataset beside `uci`:

```sh
$ fetchloom lock
error: the merge needs a choice for 2 datasets
  iris: only ours has it, and fetchloom cannot tell whether ours added it or theirs removed it
  penguins: only theirs has it, and fetchloom cannot tell whether theirs added it or ours removed it
  no merge base was found: git 2.45 or later, or `git config merge.conflictStyle zdiff3` before merging, lets fetchloom see what each side changed
  help: choose a side for each with `fetchloom lock --ours <name>` or `fetchloom lock --theirs <name>`; both repeat
$ fetchloom lock --ours iris --theirs penguins
     Merged data.toml and data.lock · iris from ours, penguins from theirs, 1 the same
```

with git 2.47 on `PATH`, the same `fetchloom lock` read the base from git and printed that `Merged` line itself. run `fetchloom sync` next, then commit `data.toml` and `data.lock` to finish the merge.

with `--json`, `result.merge` has the arrays `from_ours`, `from_theirs`, `same`, `removed` and `unpinned`, by dataset name, and `result.datasets` names the pins that changed from the checked-out branch's. an `unconfirmed` error keeps `result.merge` with `conflicts`, the datasets that need a choice. its `fix` is null, since only a person picks a side.

## dry run

`lock --dry-run`, or `-n`, lists what `lock` would change without writing `data.lock` or the cache and without downloading files. with `--upgrade`, it shows which datasets have newer versions. it still resolves entries, so a source may be asked what a link holds, and it takes no project hold. its lines are those of a real lock, with `Would pin`, `Would lock`, `Would remove` and `Would upgrade` as verbs. `Would fetch` names each file a real lock would download first to learn its hash; a file no checksum pins is also listed there under `--upgrade`, since only its bytes tell whether it changed, and until then the dry run counts it as unchanged. `Would open` names each archive it would unpack to learn what it holds. `--check` already changes nothing; `--check --dry-run` is a check. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## exit codes

0 means pins were updated, a merge was written or a check passed. a stale lock under `--check` exits 1 with `lock.outdated`, a recipe data.lock does not pin as built now exits 60 with `build.needed`, and a merge that needs a choice exits 1 with `unconfirmed`. incompatible options exit 2, as do `--ours` and `--theirs` without conflict lines, or naming a dataset that needs no choice. conflict lines out of git's order, or a side that does not read, exit 50 with `project.invalid`. resolution, download and local failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| `data/` has old files | lock does not place files | run `fetchloom sync` |
| an explicit version did not advance | the reference still names that version | edit `ref` deliberately, then lock |
| `--check` passes despite edited data | it checks specs, not contents | use `status`; use `cache verify` for cached objects |
| rebuilding a broken lock changes inputs | fresh resolution can choose newer data | restore the original lock from git when those pins matter |
| `data.toml has an unfinished git merge conflict`, or the same for `data.lock` | a git merge left both sides between its conflict lines | run `fetchloom lock`, which [merges them](#after-a-git-merge) |
| `the merge needs a choice` for a dataset only one side has | no merge base: git is older than 2.45 or not on `PATH`, or the file was already `git add`ed, and git's conflict lines show none, as its default style does | choose with `--ours` or `--theirs`, or merge again with `git config merge.conflictStyle zdiff3` |

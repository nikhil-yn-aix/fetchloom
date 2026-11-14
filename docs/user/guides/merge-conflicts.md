# Resolve a Git merge conflict

## after a git merge

a git merge that could not combine two changes leaves both sides in `data.toml` or `data.lock`, between git's `<<<<<<<`, `=======` and `>>>>>>>` lines. every other command then stops with `project.invalid`, as `data.toml has an unfinished git merge conflict`, and names `fetchloom lock`, which merges both files one dataset at a time, as `pnpm install` merges a lockfile, without resolving or downloading anything:

- a dataset only one side added, changed or removed takes that side.
- a dataset both sides have alike stays. two pins of the same files made at different times are alike.
- a dataset each side changed its own way stops the merge with `unconfirmed`, exit 1, naming each one, and nothing is written. so does a dataset one side removed while the other pinned other files for it, by a re-import or `lock --upgrade`. those leave its entry in `data.toml` as it was, so git merges `data.toml` by itself and takes the removal, and only `data.lock` holds conflict lines.
- `--ours <name>` keeps our side of such a dataset and `--theirs <name>` their side, its entry in `data.toml` and its pin alike, each whole as that side's branch has it when git can tell: a removed dataset comes back with its comments and every key of its entry. `--yes` never picks a side.
- a pin is kept only when it answers the merged entry; an entry no pin answers is left for the next `fetchloom sync` or `fetchloom lock` to pin, with a warning.
- a [recipe](recipes.md)'s result merges as a pin does: from the side that built it anew, with a choice when both sides built it their own way, and dropped with the recipe. a merged data.toml whose recipes would not read, as when one side's recipe reads a dataset the other side removed, stops the merge, saying why.
- the same values with other comments on each side keep one side's text, with a warning naming it.
- `[project]` and comments outside the datasets must agree or have changed on one side only. otherwise the merge stops with `project.invalid`, exit 50, and that part is for a person: keep one side's lines between git's conflict lines, such as one `data_dir`, delete the other side's lines and the conflict lines, then run `fetchloom lock`.

ours and theirs are git's sides. in a merge, ours is the branch checked out and theirs the branch merged in. in a rebase, ours is the branch you rebase onto, with the commits already replayed, and theirs is your commit being replayed. a cherry-pick's theirs is the commit picked.

the merge writes `data.toml`, the lists in `data.lock.d` the kept pins name, and `data.lock`, then removes the lists no pin names. `--dry-run` shows the merge without writing, and `--check` and `--upgrade` stop as other commands do until it is written. deleting `data.lock` to get past a conflict instead pins every dataset anew, which can pin newer data than either branch had.

telling an addition on one side from a removal on the other needs the merge base, what both branches started from. `lock` asks git for each side of a file and for their base, from git's index or the commits being merged, so a side holds only the lines its branch wrote. git is found on `PATH` outside the project and runs with no shell, no input and no transport, under a time and size limit, so it never reaches the network.

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

`-v` also shows, after [what the run spends](speed-and-memory.md#see-what-a-run-uses), what each side pins of a dataset that needs a choice: a recipe result's key, how many files and bytes, and up to 3 of the files only that side pins, each with its size and the first 12 digits of its SHA-256. it picks no side. with git, after one branch removed `udata` while the other ran `lock --upgrade udata`, merged either way, on the branch that removed it:

```sh
$ fetchloom -v lock
  Resources default: 8 downloads at once, 4 to one host, 24 local workers, 4.0 GiB for buffers, 2 hash lanes
            buffers: 4.0 GiB at most, less than a quarter of the 162.7 GiB available
            udata, ours: not in data.lock
            udata, theirs: 1 file · 12 B
              u.csv · 12 B · sha256 6f4b398e3e87
error: the merge needs a choice for udata
  udata: one side changed it and the other removed it
  help: choose a side for each with `fetchloom lock --ours <name>` or `fetchloom lock --theirs <name>`; both repeat
  kind: unconfirmed
            17 ms in all
```

run `fetchloom sync` next, then commit `data.toml` and `data.lock` to finish the merge.

with `--json`, `result.merge` has the arrays `from_ours`, `from_theirs`, `same`, `removed` and `unpinned`, by dataset name, and `result.datasets` names the pins that changed from our side's. an `unconfirmed` error keeps `result.merge` with `conflicts`, the datasets that need a choice. its `fix` is null, since only a person picks a side.

## a git merge conflict

a git merge that could not combine two changes leaves both sides in the file between conflict lines, `<<<<<<<`, `=======` and `>>>>>>>`. every command then stops with `project.invalid`, exit 50, saying `data.lock has an unfinished git merge conflict` and naming the first such line; `data.toml` gets the same. its help, and the JSON `fix`, name `fetchloom lock`, which [merges both files](#after-a-git-merge) one dataset at a time without downloading; each side of `data.lock` must read on its own.

the lists in `data.lock.d` are named by their contents, so a merge brings each branch's new parts in beside the others without a conflict. the merge reads only the parts of the pins it keeps, which may be all git left of a side, and removes the parts no pin names once `data.lock` is written.

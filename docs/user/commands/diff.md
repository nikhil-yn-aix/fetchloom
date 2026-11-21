# diff

`diff` shows what changed in data.lock between two git commits, the file lists in data.lock.d included, since git shows those lists only as compressed bytes.

```sh
fetchloom diff
fetchloom diff HEAD~1 HEAD --json
```

## usage

```sh
fetchloom diff [options] [from] [to]
```

`from` is HEAD when none is named, and `to` the project folder as it is now. `fetchloom help diff` says more.

each dataset whose pin differs gets a line saying what changed, then a line for each file, as `git diff --name-status` gives one. a change of `select` or `exclude` names each side's patterns, as data.toml asks for them where that side's data.lock pinned them. stdout after `fetchloom exclude data/draws/b.csv`, matching a test:

```text
draws changed: exclude none → ["b.csv"]; 1 file removed
D  draws/b.csv  2 B
```

where a side's data.toml was edited after its data.lock was written, the line says only `select or exclude changed in data.toml`.

a file pinned with the same bytes at another link, as a hand edit of data.lock can leave it, is told too, since a clone asks that link first: on stdout, `sim changed: 1 file at another link`, then `M  sim/draws.csv  20 B, the same bytes, link 127.0.0.1 → example.org`, naming the host before and after, or `at another link on <host>` when the host is the same. a file whose bytes changed names both hosts the same way when its link went to another host. `lock --check` does not look at links, as [lock](lock.md#what-happens) says, so diff is the way to review them.

a commit that lacks the file list of a dataset, as `git commit -a` leaves a new list in data.lock.d out, has that dataset told with what else changed and its files not compared, and diff goes on. a side with no data.lock in the project's folder is said, as every dataset then counts as added or removed, and a dataset data.toml names that data.lock does not pin yet gets a hint to pin it. diff takes two commits, never a range such as `HEAD~1..HEAD`, and in a shallow clone, which lacks older commits, it says how to fetch more. a commit whose data.lock cannot be read stops diff, with that commit named.

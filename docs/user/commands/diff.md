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

a commit that lacks the file list of a dataset, as `git commit -a` leaves a new list in data.lock.d out, has that dataset told with what else changed and its files not compared, and diff goes on. a side with no data.lock in the project's folder is said, as every dataset then counts as added or removed, and a dataset data.toml names that data.lock does not pin yet gets a hint to pin it. diff takes two commits, never a range such as `HEAD~1..HEAD`, and in a shallow clone, which lacks older commits, it says how to fetch more. a commit whose data.lock cannot be read stops diff, with that commit named.

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

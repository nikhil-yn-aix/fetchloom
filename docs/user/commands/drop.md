# drop

`drop` frees the space a pinned dataset takes on this machine, keeping it in `data.toml` and `data.lock`, once each of its files is found elsewhere.

```sh
fetchloom drop pets
fetchloom drop pets --dry-run
fetchloom sync pets
```

## usage

```sh
fetchloom drop [options] <name>...
```

| option | effect |
|---|---|
| `-n`, `--dry-run` | ask where each file is found and show what would be freed, deleting nothing |

[global options](global-options.md) also apply.

## what happens

1. check that every file of each dataset can be had again: a [mirror](../guides/mirrors.md) folder holds a copy that hashes to its pin, or a web mirror or its source serves it at its pinned size, as [`status --sources`](status.md) asks. a recipe's result or a dataset added from a folder counts only on a mirror, and `--offline` only on a folder mirror. a dataset that fails stays as it was.
2. delete the unchanged files fetchloom placed for it in the data folder; an edited file stays and is named.
3. note in `.fetchloom/state`, which only this machine reads, that it is dropped, then delete its files in the cache that no other dataset or project needs, and print the bytes freed.

a plain `fetchloom sync` leaves a dropped dataset out and says so; `fetchloom sync <name>` or `fetchloom path <name>` brings it back. `status` and `list` show it as dropped, and `cache gc` does not keep its files for this project.

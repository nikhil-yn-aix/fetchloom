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

1. check that every file of each dataset can be had again: a [mirror](../guides/mirrors.md) folder holds a copy that hashes to its pin, or, with `--yes`, a web or S3 mirror or its source serves it at its pinned size, as [`status --sources`](status.md) asks. a size alone does not show the bytes are the same, so without `--yes` such a dataset stays, with `unconfirmed` (exit 1) and the way to keep a copy first: `fetchloom push <name> --to <folder>`, then `fetchloom drop <name> --mirror <folder>`. a recipe's result or a dataset added from a folder, which no source gives back, counts only on a folder mirror. a file no mirror or source has keeps its dataset with `data.not_ready` (exit 1); one that could not be checked, as when the network or its source gave no answer, with the error its download would meet, such as `network.unreachable` (exit 20). `--offline` asks only folder mirrors. a dataset that fails stays as it was, and a dataset of which nothing is on this machine has nothing to drop, which is no failure.
2. note in `.fetchloom/state`, which only this machine reads, that it is dropped, so a drop cut short leaves it dropped, then delete the unchanged files fetchloom placed for it in the data folder; an edited file stays and is named. in a copy of the project, whose state another folder wrote, a file at its pinned place that holds the pinned bytes counts as placed, as `status` counts it in place, and goes; one with other bytes stays and is named.
3. delete its files in the cache that no other dataset or project needs, and print the bytes freed. a dataset dropped before whose files came back into the cache, as a build or a push brings them, is freed again.

a plain `fetchloom sync` leaves a dropped dataset out and says so, and `build` and `add` leave it out too unless they name it: a build fetches a dropped input into the cache only when a step that reads it runs, and places it nowhere; `fetchloom sync <name>`, `fetchloom path <name>` or an `add` of its link brings it back, the add saying `Ok <name> is already in data.toml and back on this machine`, a recipe's result or an import from the folder mirror that holds it, as `fetchloom sync <name> --mirror <folder>`. `status` and `list` show it as dropped, and `cache gc` does not keep its files for this project.

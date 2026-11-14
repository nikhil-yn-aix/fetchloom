# Free disk space and delete data

## freeing space

`fetchloom remove <name>` takes a dataset out of data.toml, data.lock and the data folder. a file you edited stays:

```text
$ fetchloom remove penguins
warning: data/penguins/penguins.csv is no longer needed, but it has changed, so it stays
  help: delete it if you do not need it
    Removed penguins
```

the cache keeps the bytes. `fetchloom cache gc` removes what no project needs once it is a day old, and `--now` removes it at once, here the CSV and the dropped member:

```text
$ fetchloom cache gc --now
    Removed 2 files · 17.6 KiB from the cache
    Cleaned the cache, keeping files needed by 1 project
```

raw data that only built recipes read, kept out of the data folder with `place = false`, stays in the cache for builds; once a folder mirror holds it, `fetchloom cache gc --inputs` frees the cache's copy too, as [raw input a mirror holds](../commands/cache.md#raw-input-a-mirror-holds) shows.

[cache](../commands/cache.md) says how gc decides what a project needs.

## deleting a dataset from a machine

a data-use agreement can require deleting a dataset's bytes from a machine. fetchloom keeps them in more than one place, and frees only the ones it manages:

1. in each project that uses the dataset, run `fetchloom remove <name>`, after `fetchloom remove <name> --dry-run` to see what goes. it keeps files you edited and other files in the dataset's folder, such as the `<path>.changed` copies sync kept, and names them in warnings: delete those yourself. a data folder that `data_dir` no longer names keeps all its files, which sync named once when `data_dir` changed: delete that folder yourself too. deleting a whole project folder works too.
2. delete the folders `fetchloom get` wrote; no project records them.
3. run `fetchloom cache gc --now`, after `fetchloom cache gc --now --dry-run` to see what goes. it deletes the cached files no project needs, including what a deleted project folder alone needed, and every unfinished download no running fetchloom holds. temporary files under the cache's `tmp/` folder keep their one-day grace period even then.
4. repeat this on each machine and cache that fetched the dataset, such as other computers, CI caches, container volumes and a lab's shared cache.

a cached file stays while another project using the same cache pins the same bytes, under any dataset name, since projects share files by SHA-256. `fetchloom cache` lists the projects that use the cache.

fetchloom cannot reach copies it did not make: backups, a mirror or shared drive you copied files to, a synced folder's trash or version history, and copies other programs made. it deletes a file the way `rm` does, without overwriting it, so snapshots, copy-on-write clones or the disk itself can keep the bytes, and fetchloom does not verify erasure. when an agreement requires that, follow your institution's procedure for the disk. the git history of `data.toml` and `data.lock` keeps the dataset's reference, file names, sizes and SHA-256, not its bytes.

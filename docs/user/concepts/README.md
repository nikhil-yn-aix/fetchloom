# How fetchloom works

fetchloom turns the list of datasets in `data.toml` into pinned files that projects reuse from one cache. to pin a dataset is to record its exact files and their SHA-256 in `data.lock`.

```sh
fetchloom add zenodo:3608990
fetchloom sync
```

| part | purpose |
|---|---|
| `data.toml` | name the datasets and selection rules you want |
| `data.lock` | record the exact files, their sizes and SHA-256 |
| the cache folder | keep each file once, for reuse across projects |
| `sync` | fill `data/` from `data.lock` |

# lock

`lock` records exact input files without placing them in `data/`.

```sh
fetchloom lock
fetchloom lock --check
fetchloom lock --upgrade iris flowers
fetchloom lock --upgrade
```

## usage

```sh
fetchloom lock [options]
```

| option | effect |
|---|---|
| `--check` | check manifest membership and spec hashes without changing anything |
| `--upgrade [name ...]` | resolve named datasets again; with no names, consider every dataset |

`--check` conflicts with `--upgrade`. [global options](README.md#global-options) also apply.

## what happens

1. read `data.toml` and the existing lock, if present.
2. keep entries whose specs are unchanged, unless an upgrade was requested.
3. resolve changed entries and learn file sizes, hashes and archive trees. this may download data into the cache.
4. write `data.lock` without placing files or registering the project with the cache.

already pinned ordinary files need not be cached. a source that supplies SHA-256 and size can allow locking without a download. an extracted archive needs its tree hash.

stdout excerpt from `fetchloom --help`:

```text
  lock    update data.lock from data.toml without placing files in the data folder
```

`lock --check` is silent on success at normal verbosity. it checks the manifest against the lock, not local file contents.

## upgrading

`--upgrade` revalidates unchecksummed URLs. a specifically versioned reference still names that version. edit `ref` if you want another explicit version.

filter or extraction changes resolve the existing `resolved` reference. changing `ref`, deleting the lock or requesting an upgrade resolves the manifest reference instead. [spec hashes](../reference/data-lock.md#the-spec-hash) defines these rules.

after an upgrade, run `fetchloom sync` to place the new inputs and commit `data.lock`. unchanged resolved references and file lists retain their previous `locked` timestamp.

## exit codes

0 means pins were updated or a check passed. a stale lock under `--check` exits 1 with `lock.outdated`. incompatible options exit 2. resolution, download and local failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| `data/` has old files | lock does not place files | run `fetchloom sync` |
| an explicit version did not advance | the reference still names that version | edit `ref` deliberately, then lock |
| `--check` passes despite edited data | it checks specs, not contents | use `status`; use `cache verify` for cached objects |
| rebuilding a broken lock changes inputs | fresh resolution can choose newer data | restore the original lock from git when those pins matter |

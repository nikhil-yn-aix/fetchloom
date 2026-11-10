# lock

`lock` records exact input files without placing them in `data/`.

```sh
fetchloom lock
fetchloom lock --check
fetchloom lock --upgrade iris flowers
fetchloom lock --upgrade
fetchloom lock --upgrade --dry-run
```

## usage

```sh
fetchloom lock [options]
```

| option | effect |
|---|---|
| `--check` | check manifest membership, spec hashes and `extract` without changing anything |
| `--upgrade [name ...]` | resolve named datasets again; with no names, consider every dataset |
| `-n`, `--dry-run` | list what locking would change without writing `data.lock` or downloading |

`--check` conflicts with `--upgrade`. [global options](README.md#global-options) also apply.

## what happens

1. read `data.toml` and the existing lock, if present.
2. keep entries whose specs and `extract` are unchanged, unless an upgrade was requested.
3. resolve changed entries and learn file sizes, hashes and archive trees. this may download data into the cache.
4. write `data.lock` without placing files or registering the project with the cache.

each dataset pinned for the first time gets a `Pinned` line of its own, with what it resolved to, its title when the source gives one, its file count and its size, so a typo in an id that pins an unrelated record stands out. up to 20 first pins get a line each; the rest, and changed pins of datasets locked before, are counted in one `Locked` line.

with `--json`, `result.datasets` names the datasets whose pins changed, as `sync` does: pinned for the first time, pinned to other data, or no longer in `data.lock`.

already pinned ordinary files need not be cached. a source that supplies SHA-256 and size can allow locking without a download. an extracted archive needs its tree hash.

`lock --check` is silent on success at normal verbosity. it checks the manifest against the lock, not local file contents. an empty manifest passes even without a lock; `sync --locked` instead requires `data.lock` to exist.

## upgrading

`--upgrade` revalidates unchecksummed URLs and files whose source now states an MD5 that `data.lock` lacks. a specifically versioned reference still names that version. edit `ref` if you want another explicit version.

filter changes resolve the existing `resolved` reference. an `extract` change alone keeps the pinned files and lists nothing again; it needs the network only for archives the cache lacks. changing `ref`, deleting the lock or requesting an upgrade resolves the manifest reference instead. [spec hashes](../reference/data-lock.md#the-spec-hash) defines these rules.

after an upgrade, run `fetchloom sync` to place the new inputs and commit `data.lock`. unchanged resolved references and file lists retain their previous `locked` timestamp.

## dry run

`lock --dry-run`, or `-n`, lists what `lock` would change without writing `data.lock` or the cache and without downloading files. with `--upgrade`, it shows which datasets have newer versions. it still resolves entries, so a source may be asked what a link holds, and it takes no project hold. its lines are those of a real lock, with `Would pin`, `Would lock`, `Would drop` and `Would upgrade` as verbs. `Would fetch` names each file a real lock would download first to learn its hash; a file no checksum pins is also listed there under `--upgrade`, since only its bytes tell whether it changed, and until then the dry run counts it as unchanged. `Would open` names each archive it would unpack to learn what it holds. `--check` already changes nothing; `--check --dry-run` is a check. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## exit codes

0 means pins were updated or a check passed. a stale lock under `--check` exits 1 with `lock.outdated`. incompatible options exit 2. resolution, download and local failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| `data/` has old files | lock does not place files | run `fetchloom sync` |
| an explicit version did not advance | the reference still names that version | edit `ref` deliberately, then lock |
| `--check` passes despite edited data | it checks specs, not contents | use `status`; use `cache verify` for cached objects |
| rebuilding a broken lock changes inputs | fresh resolution can choose newer data | restore the original lock from git when those pins matter |
| `data.toml has an unfinished git merge conflict`, or the same for `data.lock` | a git merge left both sides between its conflict lines | keep the datasets from both sides, delete the `<<<<<<<`, `=======` and `>>>>>>>` lines, then run `fetchloom lock`; [a git merge conflict](../reference/data-lock.md#a-git-merge-conflict) has the details |

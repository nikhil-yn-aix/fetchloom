# lock

`lock` records exact input files without placing them in `data/`, and merges `data.toml` and `data.lock` after a git merge.

```sh
fetchloom lock
fetchloom lock --check
fetchloom lock --upgrade iris flowers
fetchloom lock --upgrade
fetchloom lock --upgrade --dry-run
fetchloom lock --ours iris --theirs penguins
```

## usage

```sh
fetchloom lock [options]
```

| option | effect |
|---|---|
| `--check` | check manifest membership, spec hashes and `extract`, without changing anything |
| `--upgrade [name ...]` | resolve named datasets again; with no names, consider every dataset |
| `-n`, `--dry-run` | list what locking would change without writing `data.lock` or downloading |
| `--ours <name>` | after a git merge or rebase, keep our side, as git names it, of a dataset the merge needs a choice for; repeat it for more |
| `--theirs <name>` | the same, keeping their side |

`--check` conflicts with `--upgrade`, and `--ours` and `--theirs` with both. [global options](global-options.md) also apply.

## what happens

1. read `data.toml` and the existing lock, if present.
2. keep entries whose specs and `extract` are unchanged, unless an upgrade was requested.
3. resolve changed entries and learn file sizes, hashes and archive trees. this may download data into the cache.
4. write `data.lock` without placing files or registering the project with the cache.

as in `sync`, each dataset pinned for the first time gets a `Pinned` line with what it resolved to, its title, file count and size, so a typo in an id that pins an unrelated record stands out. up to 20 get a line each; the rest, and changed pins, are counted in one `Locked` line. with `--json`, `result.datasets` names the datasets whose pins changed.

files already pinned need not be cached. a source that states a SHA-256 and size lets a file be locked without a download, but an archive that unpacks needs its tree hash, so it is downloaded.

a passing `lock --check` ends with `Ok data.lock matches data.toml` on stderr, and with `-q` prints nothing. it checks data.toml against the lock, not the files in the data folder, nor any file's link: a link edited by hand that keeps the file's SHA-256 still passes, since data.toml asks for the same, and no source is asked whether it lists that link. [`fetchloom diff`](diff.md) names each file at another link, with the host before and after, for a review of the change. an empty data.toml passes even without a lock, while `sync --locked` needs `data.lock` to exist. a dataset whose file list `data.lock.d` lacks, as in a clone of a commit that left it out, fails it with `project.invalid`, exit 50, once nothing else is out of date, as it does `sync --locked`, so a ci run catches the missing `git add data.lock.d`; [big datasets](../reference/data-lock.md#big-datasets) says what every other command does then.

## upgrading

`--upgrade` asks each source again for the latest version of its reference, and checks again each file no checksum pins, such as a plain link's, and each file whose source now states an MD5 that data.lock lacks. a bucket object whose listing gives the ETag and size this machine recorded is not downloaded again, as [buckets](../sources/buckets.md#pins) explains. a dataset that resolves again to the revision data.lock pins, where nobody can change that revision's bytes, keeps its pins without asking about any file: a hugging face commit, a version of a kaggle dataset or an openneuro snapshot. a reference that names a version stays on it; edit `ref` to choose another.

an [import](add.md#files-on-this-machine) has no source with a newer version: `--upgrade <name>` naming one exits 10 with `reference.unresolved`, and `--upgrade` alone keeps it as data.lock pins it and says so with a `Skipped` line.

a dataset that [waits for its file list](../reference/data-lock.md#big-datasets) is not asked either: `--upgrade <name>` naming one stops with `project.invalid`, exit 50, and `--upgrade` alone keeps it and, when nothing else changed, ends `Ok data.lock is already up to date, but iris waits for its file list and was not asked`.

a change of `select` or `exclude` lists the version data.lock already resolved, not a newer one. an `extract` change alone keeps the pinned files and lists nothing again; it needs the network only for archives the cache lacks. changing `ref`, deleting the lock or an upgrade resolves the reference in data.toml anew. [the spec hash](../reference/data-lock.md#the-spec-hash) defines these rules.

after an upgrade, run `fetchloom sync` to place the new inputs, and commit `data.lock`. a pin that did not change keeps its `locked` time.

## dry run

`lock --dry-run`, or `-n`, lists what `lock` would change without writing `data.lock` or the cache and without downloading files; with `--upgrade`, it shows which datasets have newer versions. it still resolves entries, so a source may be asked what a link holds. its lines are those of a real lock, with `Would pin`, `Would lock`, `Would remove` and `Would upgrade`. `Would fetch` names each file a real lock would download to learn its hash. under `--upgrade` that includes a file no checksum pins, which the dry run counts as unchanged, but not one whose dataset resolves again to a revision whose bytes cannot change, which the real lock does not ask about either. `Would open` names each archive it would unpack. `--check --dry-run` is a check. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## exit codes

| exit | when |
|---|---|
| 0 | pins were updated, a merge was written or a check passed |
| 1 | `lock.outdated`: a stale lock under `--check`; `unconfirmed`: a merge that needs a choice |
| 2 | incompatible options, or `--ours` and `--theirs` without conflict lines or naming a dataset that needs no choice |
| 50 | `project.invalid`: a dataset whose file list `data.lock.d` lacks, under `--check` or named by `--upgrade`; conflict lines out of git's order, a side that does not read, a pin read from the conflict lines that does not read whole, or a `[project]` part that differs on both sides |

resolution, download and local failures use the [shared exit codes](../reference/exit-codes.md).

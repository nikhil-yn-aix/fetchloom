# concepts

fetchloom turns a dataset shopping list into pinned files that projects can reuse from one cache.

```sh
fetchloom add zenodo:3608990
fetchloom sync
```

| part | analogy | purpose |
|---|---|---|
| `data.toml` | shopping list | name the datasets and selection rules you want |
| `data.lock` | receipt | record the exact files, sizes and fingerprints |
| shared cache | pantry | keep bytes for reuse across projects |
| `sync` | restocking | fill `data/` from the receipt |

## manifest and lock

`data.toml` describes intent. each dataset has a reference and can select files, exclude paths, keep archives packed, request writable copies or choose a destination. see the [manifest reference](reference/data-toml.md).

`data.lock` records selected source files by SHA-256 and size, plus stated MD5 when available. source metadata and resolved versions make the origin inspectable. archive extraction adds a hash of the unpacked file tree.

downloads must match their locked size and hashes. cached objects are not rehashed on every use. file metadata and saved state avoid unnecessary reads; [cache verify](commands/cache.md#verify) checks cached bytes explicitly.

source checksum support covers MD5 and SHA-256. a known SHA-256 and size can allow a file to be locked without downloading it. hashes pin bytes after resolution; a source reference is not always immutable. see [lock rules](reference/data-lock.md#the-spec-hash).

`sync` updates missing or stale locks first unless `--locked` requires an up-to-date lock. unchanged entries retain their pins. `lock --check` checks manifest membership and specs without changing files, while `lock --upgrade` deliberately asks sources again.

## cache

the cache is local and content-addressed: objects are named by SHA-256. projects reuse the same bytes rather than downloading a separate copy for each project.

one user's processes can share it. on shared machines, give each user their own cache. a group-writable cache is suitable only among trusted users, with permissions managed through umask or ACLs; fetchloom has no cross-user administration or group-permission layer. use `copy = true` for students or jobs that edit inputs.

successful project sync registers its root for garbage collection. `get` and standalone `lock` do not. [cache commands](commands/cache.md) covers retention, verification and cleanup; [cache folder](reference/cache-folder.md) covers locations and records.

## placement

| method | how files land | writable input? |
|---|---|---|
| reflink | clone file data using filesystem copy-on-write support | yes when `copy = true` |
| hardlink | share the cache object's file | no |
| copy | write independent bytes | yes when `copy = true` |

placement tries reflink, hardlink, then copy where supported. it does not create symlinks. recognized synced folders skip hardlinks but may use reflinks.

files are read-only by default. for an analysis that writes into its inputs:

```toml
[datasets.iris]
ref = "zenodo:3608990"
copy = true
```

then run `fetchloom sync`. it replaces unchanged read-only files with writable reflinks or copies, never hardlinks. edited files stay. turning `copy` off leaves existing writable files as they are.

keep results outside managed input paths. do not make a shared hardlink writable to edit it: every project using that link shares the change. sync keeps a visibly changed hardlink as an edit with reason `shared`; `sync --force` restores it. [shared edits](commands/sync.md#local-changes) explains detection and repair, including windows aliases whose directory listings can hide a change.

sync preserves local conflicts unless forced. status differences and kept conflicts still exit 0. [scripting](scripting.md) shows how to inspect them.

## archives

extraction is enabled for `.zip`, `.tar`, `.tar.gz`, `.tar.xz`, `.tar.zst`, `.tar.bz2`, `.tgz`, `.txz`, `.tzst`, `.tbz` and `.tbz2`, ignoring case. the bytes choose the decoder.

other names stay intact, including `.fastq.gz`, `.xlsx`, `.npz`, `.pt` and extensionless downloads. `.7z`, `.rar` and recognized names containing unsupported bytes stay packed with a warning. unsafe or damaged supported archives fail extraction. nested archives are not unpacked recursively.

use `--no-extract` with `add` or `get`, or `extract = false` in an existing manifest entry, to keep packed bytes. select and exclude patterns match the listed archive, not its members.

paths below are relative to the dataset or `get` folder:

| selected files | members in `train.zip` | placed paths |
|---|---|---|
| only `train.zip` | `train/a.csv`, `train/b.csv` | `a.csv`, `b.csv` |
| only `raw/train.zip` | `train/a.csv` | `raw/a.csv` |
| `train.zip` and another file | `train/a.csv`, `train/b.csv` | `train/a.csv`, `train/b.csv` |
| `train.zip` and another file | only `a.csv` | `a.csv` |
| `train.zip` and another file | `a.csv`, `b.csv` | `train/a.csv`, `train/b.csv` |

a sole archive loses one shared top folder. beside other selected files, a shared top folder or lone top-level file keeps its name; otherwise members go under the archive's stem. the archive's parent path is retained. colliding destinations fail.

an archive with no regular files creates no dataset folder in a project. `get` still creates an empty output folder. members enter the cache before placement. packed bytes can then be removed unless a registered project needs them packed.

[tree hashes](reference/data-lock.md#the-tree-hash) records archive member paths before placement strips a folder. empty directories are not placed.

## offline

```sh
fetchloom sync --offline
fetchloom --offline path iris
```

offline mode prevents fetchloom from making HTTP or DNS requests. a locked project can use unchanged placed files, cached objects and unpack records.

search and entries needing resolution fail offline, even when you already know a source id or plain URL. repository listings are not cached for offline resolution. offline `get` of a direct URL needs its URL record and cached bytes. an unpinned new link reports "offline, and a new link must be downloaded once to learn its fingerprint". the help names the flag or environment setting to change, followed by "to pin it. `fetchloom get <link> --offline` reuses a file fetched before".

prepare the project online before disconnecting and keep its manifest and lock unchanged. `FETCHLOOM_OFFLINE=1` also enables offline mode. to reconnect, remove `--offline` and unset that variable if it is `1`.

## recovery

if a locked download returns 404 or 410, fetchloom asks its recorded source for a new URL and checks the new download against the original size and hashes. it does not rewrite the lock. an unchanged URL or another kind of failure cannot recover this way.

successful downloads stay cached when another file fails. retries can reuse saved partial bytes when hashes or server validators allow it. unsupported ranges, changed validators or damaged saved bytes can require a restart.

transfers retry transient failures and respect announced rate limits within bounded waits. several files download at once. eligible files of at least 64 MiB can split into parallel range requests after one stream stays below 1 MB/s over 2 seconds, with at least 16 MiB left. a configured rate cap prevents splitting. [environment settings](reference/environment.md) controls network work and rate limits.

## approval

```sh
fetchloom sync --yes
```

approval is required above 1 GiB, not at exactly 1 GiB.

| download | bytes checked |
|---|---|
| `get` | known bytes still needing download; cached checksummed files and unpacked archives do not count |
| project commands | known bytes still missing from the cache |
| unknown source size | the server's length against the remaining download, or received bytes after a file passes 1 GiB |

unknown sizes are checked per file, with at most one such question per batch. offline runs never ask for download approval.

`--yes` or `-y` approves downloads, never a [dataset search choice](commands/search.md#choosing-a-dataset). questions default to no and require interactive stdin and stderr. pipes, CI and JSON mode cannot answer; an unapproved download fails with exit 1 and `unconfirmed`.

## interruptions

at a download question, ctrl-c stops with exit 130. during other work, the first ctrl-c requests a stop and preserves resumable bytes before exiting 130. cache scans check between entries, and verification checks between hash reads. a second ctrl-c exits immediately with code 130.

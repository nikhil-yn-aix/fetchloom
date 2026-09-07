# Reference

Everything the binary accepts. For what each one promises, see [contracts.md](contracts.md).

Anything marked **not built** is written down and not in the binary. There is no state where a flag exists and cannot act.

## Commands

| Command | Does |
|---|---|
| `get <ref>` | Resolve, transfer, verify, unpack, record |
| `get` | The same, for every dataset the project file names |
| `probe <ref>` | What the source states about an object, moving none of it |
| `list <ref>` | What a container holds, moving as few bytes as the format allows |
| `where <ref>` | The library path a dataset lands at, fetching nothing |
| `library <subcommand>` | Inspect and change what the library holds |
| `init <url\|dir>` | Write a manifest for data that has none |
| `plan <ref>` | Report what a run would do, move no bytes |
| `apply <plan>` | Execute a plan, possibly made elsewhere |
| `repair <ref>` | Refetch only the damaged ranges of a cached object |
| `verify <path>` | Recheck a directory against the record of what was written |
| `status <path>` | Say which entries differ from the record, one line each |
| `diff <path>` | The same states, with the digest and the length on each side |
| `revert <path> [entry...]` | Put back what the record states, from the cache |
| `promote <path>` | Make the directory as it stands a dataset of its own |
| `watch <events>` | Render a run's event stream, live or after the fact |
| `cache <subcommand>` | Inspect and change what is kept between runs |
| `doctor` | Check this machine, change nothing |
| `why <ref>` | Explain a reference, its source, and its trust |
| `explain [key]` | Every setting and where its value came from |
| `completions <shell>` | Print a completion script |

### cache subcommands

| Subcommand | Does |
|---|---|
| `status` | Objects, bytes, partials, pins, quarantined |
| `ls` | List objects by digest |
| `verify` | Reread and rehash everything, quarantine each mismatch |
| `repair` | Rebuild derived data from what the cache already holds |
| `prune` | Remove what nothing refers to |
| `compact` | Rewrite packs against a dictionary trained over what each one holds |
| `clear` | Remove every object, after confirming |
| `pin <digest>` | Keep an object from ever being pruned |
| `unpin <digest>` | Remove that mark |
| `export <bundle>` | Write every object into a bundle |
| `import <bundle>` | Read a bundle in |

### library subcommands

| Subcommand | Does |
|---|---|
| `ls` | Every entry the library holds, and the bytes they take |
| `rm <path>` | Remove one entry, and the record of the run that wrote it |

## The datasets a project file names

`fetchloom.toml` takes a `datasets` table, and `get` with no reference fetches every entry in it. An entry is a reference, or a table stating `ref` and any of `output`, `select`, `exclude` and `layout`. A string is never a table and a table always states `ref`, so one shape never means the other.

```toml
[datasets]
imagenet = "acme/imagenet@2012"
eeg = { ref = "https://lab.edu/eeg.yaml", output = "data/eeg" }
corpus = { ref = "https://lab.edu/corpus.zip", select = ["train/*"] }
```

Every path in that file resolves against the directory holding it, not against the directory you are standing in, and the lock is written beside it. A run from four directories down writes what a run from the top writes.

`get --locked` with no reference is the reproducible install: every dataset in the table, pinned exactly, refusing if resolution differs.

The whole table is read and checked before the first dataset is fetched. Two entries that would write to one destination fail naming both, an unknown key inside an entry is an error as everywhere else, and a `layout` no run can take fails with nothing fetched rather than after the entries before it landed. A dataset the source fails to give does not stop the ones after it, and the run exits with the first failure's code. `--output`, `--select`, `--exclude`, `--layout` and `--library` on a project run are refused, because one destination and one selection cannot describe several datasets; the table states each.

## The library

One directory holding materialized datasets, separate from the cache, so a script does not carry a path and two projects do not fetch the same bytes twice. It sits at the platform's data location by default: `%LOCALAPPDATA%\Fetchloom\Library` on Windows, `$XDG_DATA_HOME/fetchloom/library` or `~/.local/share/fetchloom/library` on Linux. `--library-dir`, `FETCHLOOM_LIBRARY_DIR` and `library = { dir = "..." }` move it, in that order of precedence.

An entry's path is `<library>/<name>/<identity>`, decided by what the reference resolves to and by nothing else, so `where` answers without an index to consult and two versions of one dataset sit beside each other. The name component is a convenience and the identity component is the truth. A name is sanitized deterministically: every byte that is not a letter, a digit, a hyphen, an underscore or a dot becomes an underscore, trailing dots and spaces are dropped, and a name that would be a Windows device — `CON`, `PRN`, `AUX`, `NUL`, `COM1` through `COM9`, `LPT1` through `LPT9`, with or without an extension — is prefixed with `_`.

A library file is never a hard link to a cache object. It is a copy-on-write clone where the filesystem offers one and a plain copy everywhere else, with a `degrade` when the clone is refused, because one in-place edit through a hard link would corrupt the content addressed store for every dataset sharing that object.

Nothing is ever removed from the library on its own. It grows until `library rm` removes an entry, which removes the tree and the record of the run that wrote it. `library ls` states what it holds and what that takes.

`--library` and `--output` together are an error. Two destinations is not a run.

## What probe and list report

`probe <ref>` resolves, asks the source, and moves no payload bytes: the resolved location, the size, every digest the source states with its algorithm, the trust class a fetch would land in, whether the source serves ranges, and whether the cache already holds it. Anything unstated is unknown, and unknown is an answer that exits 0. Under `--offline` it answers from the cache, or fails with `policy.offline`.

`list <ref>` enumerates what a container holds, moving as few bytes as the format allows.

| What is being listed | What it costs |
|---|---|
| An object the cache already holds | The cache read, and no request |
| A directory or a prefix | The walk a run would do, or the listing the source already offers |
| A zip over a source that serves ranges | Its index, and one small read at the head of each member. The archive is never downloaded |
| A zip over a source that refuses them | The whole object, with a `degrade` first |
| A tar under any compression | The whole object, with a `degrade` first, because a tar states no index |

Where the cheap path is gone, the run says so before spending the bandwidth and keeps what it read in the cache, so asking twice costs once. It proceeds rather than refusing, because the question was what is inside and no flag exists to answer it a second way.

## What status and diff report

One state per entry, decided against the record. An unchanged entry is not printed, so a directory that is exactly what the run left prints nothing at all.

| State | Meaning |
|---|---|
| `unchanged` | The destination holds what the record states |
| `modified` | It holds something else |
| `deleted` | The record states it and the destination does not hold it |
| `added` | The destination holds it and the record does not state it |

There is no fifth state. `diff` prints the same states and adds the digest and the length the record states and the destination holds. Neither ever compares the inside of a file.

## Reference forms

| Form | Example |
|---|---|
| Bare name | `silesia` |
| Namespaced with release | `acme/imagenet@2012` |
| Local manifest | `./data.yaml` |
| Remote manifest | `https://lab.edu/eeg.yaml` |
| Direct file | `https://host/x.tar.zst` |
| Local file or directory | `file:///data/raw` |
| Object store prefix | `https://s3.amazonaws.com/bucket/prefix/` |
| Provider | `hf:datasets/org/name@rev`, `zenodo:10.5281/zenodo.1234567` |
| Kaggle dataset | `kaggle:uciml/iris` |
| OpenML dataset | `openml:61` |
| GitHub release | `github:owner/repo`, `github:owner/repo@v1.2.0` |
| Figshare article | `figshare:1234567` |
| CKAN dataset | `ckan:demo.ckan.org/a-dataset` |
| Dataverse dataset | `dataverse:dataverse.harvard.edu/doi:10.7910/DVN/OMV93V` |
| DOI | `doi:10.7910/DVN/OMV93V` |
| FTP file or directory | `ftp://ftp.ncbi.nlm.nih.gov/pub/README`, `ftp://ftp.ebi.ac.uk/pub/` |
| FTPS file or directory | `ftps://host/pub/reads.fastq.gz` |
| Metadata document | `croissant:https://host/metadata.json` |
| Content address | `blake3:<hex>` |

Resolution order: explicit scheme, then a local path if it exists, then each entry in `sources` in order. A name matching none of the configured sources fails. It is never guessed at.

A name with no `sources` configured is searched for instead, across every registry that offers search, in parallel: Hugging Face, Kaggle, OpenML, Zenodo, Figshare, CKAN at data.humdata.org, Dataverse at dataverse.harvard.edu and DataCite. Three outcomes and no fourth.

| Outcome | What happens |
|---|---|
| One record carries the name | The run proceeds, prints what the name resolved to, and records the resolved reference in the lock |
| Several records carry it | Every one is printed with its size, where it came from and what it states about its bytes, the run refuses, and the exact command for each is given |
| None carries it | The run fails, naming the nearest names it did find and the command for each |

A name never enters a lock. What it resolved to does, so the first run searches and every run after is exact.

Matching folds case and drops anything that is not a letter or a digit, so `HAM-10000` and `ham10000` are the same name. A record whose name is within three edits of the term is near enough to suggest and never near enough to resolve to.

## Global flags

Available on every command.

| Flag | Default | Meaning |
|---|---|---|
| `--config <path>` | discovered | Use this configuration file only |
| `--no-config` | off | Ignore every configuration file |
| `--cache-dir <path>` | platform default | Where the cache lives |
| `--library-dir <path>` | platform default | Where the library lives |
| `--offline` | off | Forbid all network activity |
| `--json` | off | Machine readable result on stdout |
| `--events <path\|->` | off | Write the event stream here |
| `--quiet` | off | No progress |
| `--verbose` | off | Raise log level one step, repeatable |
| `--color <auto\|always\|never>` | auto | When output is colored |
| `--display <plain\|live\|none>` | plain | How progress is presented |
| `--no-animation` | off | Do not redraw |
| `--no-hints` | off | Never print a hint |
| `--yes` | off | Answer every confirmation with yes |
| `--threads <n>` | detected | Ceiling on threads for processor work |
| `--compress <auto|none|zstd:1..19>` | auto | How cached objects are stored |

## Flags for get, plan and apply

| Flag | Default | Meaning |
|---|---|---|
| `--output <path>` | `./<name>` | Destination directory |
| `--library` | off | Materialize into the library instead, at the path `where` names |
| `--select <glob>` | all | Include members, repeatable |
| `--exclude <glob>` | none | Exclude members, applied after includes |
| `--layout <keep\|flatten:n>` | keep | Drop the first n path components |
| `--lock <path>` | `./fetchloom.lock` | Lock file location |
| `--locked` | off | Fail if resolution differs from the lock |
| `--no-cache` | off | Keep nothing from this run |
| `--verify <always\|fingerprint\|never>` | fingerprint | What a cache hit is checked against |
| `--force` | off | Overwrite modified entries, remove foreign ones |
| `--adopt` | off | Accept the destination as it stands |
| `--no-extract` | off | Keep a recognized archive as a file |
| `--concurrency <n>` | measured | Transfers in flight across all hosts |
| `--per-host <n>` | measured | Transfers in flight for one host |
| `--bandwidth <rate>` | unlimited | Ceiling on transfer rate |
| `--retries <n>` | 5 | Attempts per transient failure |
| `--timeout <duration>` | 30s | Idle timeout inside a connection |
| `--durability <strict\|normal\|fast>` | normal | How far a write is pushed before publication |
| `--io <auto\|buffered\|uncached>` | auto | Write path |
| `--aggressive` | off | Raise politeness ceilings, prints a warning |
| `--deterministic-io` | off | Disable adaptation, for benchmarking |

`repair` takes the transfer half of this list and refuses the rest, because it restores bytes in the cache and materializes nothing. `plan` refuses `--force` and `--adopt`, because it writes to no destination.

### Flags for promote

| Flag | Default | Meaning |
|---|---|---|
| `--output <path>` | stdout | Write the manifest to a file |
| `--force` | off | Overwrite the file `--output` names |
| `--lock <path>` | `./fetchloom.lock` | Where the lock is written |

`status`, `diff` and `revert` take the global flags and `--verify <always|fingerprint|never>`, which decides whether an entry is answered by its recorded fingerprint or by reading its bytes. Nothing else.

### Flags for init

| Flag | Default | Meaning |
|---|---|---|
| `--output <path>` | stdout | Write the manifest to a file |
| `--force` | off | Overwrite the file `--output` names |

## Environment

| Variable | Effect |
|---|---|
| `FETCHLOOM_CACHE_DIR` | Cache location |
| `FETCHLOOM_LIBRARY_DIR` | Library location |
| `FETCHLOOM_CONFIG` | Configuration file path |
| `FETCHLOOM_OFFLINE` | Offline when set to `1` |
| `FETCHLOOM_CONCURRENCY` | Global concurrency |
| `FETCHLOOM_PER_HOST` | Per host concurrency |
| `FETCHLOOM_THREADS` | Processor thread ceiling |
| `FETCHLOOM_COMPRESS` | How cached objects are stored |
| `FETCHLOOM_BANDWIDTH` | Bandwidth ceiling |
| `FETCHLOOM_LOG` | `error`, `info`, or `debug` |
| `FETCHLOOM_TOKEN_<HOST>` | Bearer credential for that host |
| `FETCHLOOM_ACCESS_KEY_<HOST>` | Access key of a signing credential |
| `FETCHLOOM_SECRET_KEY_<HOST>` | Secret key of the same credential |
| `FETCHLOOM_SESSION_TOKEN_<HOST>` | Session token, when it has one |
| `FETCHLOOM_REGION_<HOST>` | Region the credential signs for |
| `KAGGLE_API_TOKEN` | Kaggle's own variable, read when `FETCHLOOM_TOKEN_WWW_KAGGLE_COM` is unset |
| `GITHUB_TOKEN` | GitHub's own variable, read when `FETCHLOOM_TOKEN_API_GITHUB_COM` is unset |
| `AWS_ACCESS_KEY_ID` and friends | Read only by the provider native tier |
| `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY` | Proxy policy |
| `XDG_CACHE_HOME` | Cache root on Linux |
| `NO_COLOR` | Disable color |

## Provider credentials

What each provider needs, and what it states about the bytes it serves.

| Provider | Credential | Where it is read from | Digest it states |
|---|---|---|---|
| Hugging Face | Optional bearer | `FETCHLOOM_TOKEN_HUGGINGFACE_CO` | None |
| Zenodo | Optional bearer | `FETCHLOOM_TOKEN_ZENODO_ORG` | None |
| Kaggle | Required for a private or competition dataset | `FETCHLOOM_TOKEN_WWW_KAGGLE_COM`, then `KAGGLE_API_TOKEN` | None |
| GitHub releases | Required for a private repository, optional to raise the rate limit | `FETCHLOOM_TOKEN_API_GITHUB_COM`, then `GITHUB_TOKEN` | SHA-256, on every asset GitHub has digested |
| OpenML | None | | MD5 only, which is not carried |
| Figshare | None | | MD5 only, which is not carried |
| CKAN | Not built. A CKAN install that refuses an anonymous request fails naming the status | | `hash`, carried only when it is written `sha256:<hex>` |
| Dataverse | Not built. Dataverse authenticates with an `X-Dataverse-key` header rather than `Authorization`, and the credential seam has no per-adapter header name | | The `checksum` the install records, carried only when its `type` is SHA-256 |
| FTP, FTPS | Anonymous by default. A bearer credential written `user:password` logs in as that user, and is refused unless the control connection is secured | `FETCHLOOM_TOKEN_<HOST>` | None. FTP states no checksum, so a first fetch is `tofu` |

A variable named for the host is read before the provider's own, and holds the whole `Authorization` header rather than a bare token, which is how a header other than `Bearer` is sent. The provider's own variable holds the bare token the provider prints, and is sent as `Bearer` followed by it.

An MD5 is never carried as a digest claim. It is not a digest this build computes, and a run that cannot recompute it cannot verify against it, so a provider that states only an MD5 leaves the trust class at `tofu` rather than implying more evidence than exists.

## Configuration keys

TOML. Manifests take three syntaxes because strangers write them. Configuration takes one because you write it, and one format means one parser and one set of error messages.

| Key | Meaning |
|---|---|
| `sources` | Ordered base locations a bare name resolves against |
| `cache` | Cache location, as `cache = { dir = "..." }` |
| `library` | Library location, as `library = { dir = "..." }` |
| `datasets` | What `get` with no reference fetches |
| `offline` | Forbid network activity |
| `concurrency`, `per_host` | In flight ceilings |
| `bandwidth` | Rate ceiling |
| `threads` | Processor thread ceiling |
| `retries`, `timeout` | Retry policy |
| `verify` | Cache hit verification policy |
| `durability`, `io` | Write path |
| `compress` | How cached objects are stored |
| `log` | Log level |
| `color`, `display`, `hints` | Presentation |

Unknown keys are an error. The `x-` prefix is reserved and refused.

## Log levels

A level decides which of the events the run already emits reach stderr, each as one JSON object. It never decides which events exist, and the stream `--events` writes is identical at every level.

| Level | Rendered |
|---|---|
| `error` | `error` and `degrade` |
| `info` | those, plus `run.start`, `run.end`, and the result. Default |
| `debug` | every event, one line each |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 2 | You asked for something that is not a command |
| 10 | The reference could not be resolved |
| 20 | The network failed after every retry |
| 30 | The bytes are not what they were supposed to be |
| 40 | Policy blocked the run |
| 50 | Not enough disk, or a resource limit exceeded |
| 60 | The destination is in the way |
| 70 | The archive contains something unsafe |
| 80 | The cache could not be used |
| 130 | Cancelled |

## Error kinds

| Kind | Exit | What to do |
|---|---|---|
| `reference.unresolved` | 10 | Check the path or URL. The message says which |
| `manifest.invalid` | 10 | The message names the key and the line |
| `alias.unstable` | 10 | Run once without `--locked` to record what it resolves to now |
| `network.timeout` | 20 | The source went quiet longer than the idle timeout |
| `network.refused` | 20 | The connection did not complete |
| `network.status` | 20 | The source answered with a status the run cannot use |
| `network.tls` | 20 | The certificate could not be verified |
| `source.unsupported_range` | 20 | A range was asked for and the whole object was served |
| `source.identity_changed` | 20 | A source contradicted an immutable identity it stated |
| `integrity.mismatch` | 30 | For a cached object, `repair <ref>`. For a directory, fetch again |
| `integrity.truncated` | 30 | Run again, a resume picks up from what arrived |
| `integrity.range_mismatch` | 30 | Run again, the partial is discarded first |
| `policy.offline` | 40 | Drop `--offline`, or plan elsewhere and carry a bundle |
| `policy.terms_required` | 40 | Run again with `--yes` |
| `policy.trust_refused` | 40 | Run once without `--locked` |
| `policy.credential_missing` | 40 | Follow the numbered steps the run printed |
| `policy.credential_invalid` | 40 | Renew it or widen its scope |
| `resource.disk` | 50 | A volume has no room for what the run needs |
| `resource.limit` | 50 | A document or a pool exceeded a bound |
| `destination.conflict` | 60 | You and upstream changed one entry. Upstream's is beside yours as `<name>.upstream` |
| `destination.modified` | 60 | `--force` to overwrite, `--adopt` to accept |
| `destination.foreign` | 60 | `--force` to remove, `--adopt` to accept |
| `destination.unrepresentable` | 60 | The message names the member and what the volume said |
| `destination.cross_volume` | 60 | Put the destination on the volume the staging is on |
| `archive.unsafe_path` | 70 | A member path is unsafe, and the message says how |
| `archive.link_escape` | 70 | A link resolves outside the destination |
| `archive.collision` | 70 | Two members land on one path |
| `archive.bomb` | 70 | More entries, bytes, or ratio than the limits allow |
| `archive.unsupported` | 70 | A format or entry type this build does not carry |
| `cache.locked` | 80 | Another process holds it. Run again |
| `cache.corrupt` | 80 | `cache verify`, then `repair <ref>` |
| `cache.format_mismatch` | 80 | `cache clear` |
| `cache.cross_volume` | 80 | Put the cache on one volume |
| `cache.locking_unsupported` | 80 | Put the cache somewhere local |

## Events

Newline delimited JSON. Every event carries a sequence number, a timestamp, the dataset, and where it applies the artifact.

```
run.start run.end
resolve.start resolve.alias resolve.end
plan.ready
cache.hit cache.miss cache.wait
credential.required credential.offer credential.declined
listing.start listing.skipped listing.end
source.probe source.selected source.failover
transfer.start transfer.progress transfer.retry transfer.resume transfer.end
verify.start verify.range verify.mismatch verify.end
extract.start extract.reject extract.end
publish.commit
reconcile.outcome
merge.resolution
degrade
error
```

`degrade` fires whenever any capability, optimization or trust level came out lower than what was asked for, and names what was requested, what was used, and why.

## Archive formats

| Name | What it is |
|---|---|
| `tar` | A POSIX ustar stream |
| `tar+gzip`, `tar+zstd`, `tar+xz`, `tar+bzip2` | A tar inside that compression |
| `zip` | A zip container, store and deflate only |
| `gzip`, `zstd`, `xz`, `bzip2` | One compressed object, materialized as one file |

The name and the bytes must agree. A file whose name ends in `tar` and whose header says otherwise fails with `archive.unsupported` naming both.

## Metadata formats init can absorb

Checksum sidecars, Croissant, Frictionless data packages, pooch registries, BagIt.

Torrent and DVC are refused by name. A torrent states SHA-1 over pieces that span file boundaries and names a swarm rather than a location. A DVC file states MD5 or an ETag, and an ETag is supporting evidence, never content identity.

## Limits

All configurable. None may be raised past a ceiling that would allow unbounded memory or disk use.

| Limit | Default |
|---|---|
| Archive entries | 1,000,000 |
| Expanded bytes | 1 TiB |
| Expansion ratio | 200 |
| Nesting depth | 64 |
| Redirects followed | 10 |
| Manifest size | 16 MiB |
| Manifest node count | 100,000 |
| Record size | 256 MiB |
| Record node count | 8,000,000 |
| Retry attempts | 5 |
| Retry ceiling | 60 s |
| Outboard threshold | 64 MiB |
| Outboard chunk group | 1 MiB |
| Pack threshold | 1 MiB |
| Compression frame | 1 MiB |
| Compression probe head | 1 MiB |
| Compression probe ratio | 1.10 |
| Compression probe strides | 1, 2, 4, 8 |
| Compaction level | 19 |
| Compaction dictionary | 16 KiB |
| Compaction minimum objects | 8 |
| Compaction minimum content | 128 KiB |
| Split threshold | 64 MiB |
| Repair spans | 64 |
| Repair whole refetch share | 50 percent |
| Listing entries | 500,000 |
| Listing size | 16 MiB |
| Connections to one host | 4 |
| Connect timeout | 10 s |
| Response header timeout | 30 s |
| Idle timeout inside a body | 30 s |
| Probed candidates | 4 |

## Not built

Written down, not in the binary. Each is refused as an unknown flag or command today rather than accepted and ignored.

| Thing | What it would do |
|---|---|
| `sftp://` | Deliberately not. An SSH stack, a host key policy, agent forwarding, four key formats and rekeying are a security surface the size of the rest of the tool, and belong to their own change with their own SECURITY.md section |
| Installer, signed releases | Distribution |

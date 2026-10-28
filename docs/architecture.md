# architecture

one rust crate builds `fetchloom` and `fl`. both call the same library entry point.

## modules

| module | owns |
|---|---|
| `cli` | argument parsing, settings, command orchestration and final reports |
| `cli/sync` | relocking, download selection, archive work and placement decisions |
| `project` | manifest, lock and state types, project discovery, metadata writes and project locking |
| `source` | reference parsing, DOI routing and source listings |
| `net` | HTTP, retries, partial-download streaming, credentials and address checks |
| `cache` | objects, partials, indexes, project registrations, gc and verification |
| `archive` | format readers, member checks, limits and staged member writes |
| `plan` | destination expansion and collision checks |
| `place` | reflinks, hardlinks, copies, directory checks and synced-folder detection |
| `output` | human rendering, progress, warnings and the JSON envelope |
| `digest`, `name`, `glob`, `units`, `stop`, `work` | hashes, paths, patterns, formatting, cancellation and counters |

there is no separate model crate or python wrapper. core types live with their owners. `net` writes through `cache`; `cli/sync/place` combines the plan, state and filesystem. dependencies such as `url::Url` cross module boundaries.

## the project loop

1. read the manifest, lock and local state. keep lock entries whose spec hashes match, unless an upgrade requests them again.
2. resolve changed entries and filter their source listings. known hashes may come from the source, the old lock or the MD5 index. download unknown hashes and unpack new archives to learn their tree hashes.
3. write `data.lock` if its contents changed. standalone `lock` does no placement and need not fill the cache with already pinned files.
4. expand the lock into destination targets. an archive without a usable member record is unpacked before its destinations can be planned. compare targets with state and disk metadata to choose keep, adopt, place, replace or leave.
5. fetch only the bytes needed by place and replace actions. a missing archive member requires the original archive again when it is not cached. check pinned size, SHA-256 and any stated MD5 on downloads.
6. remove unwanted unchanged tracked files, place the targets, and write state. drop unneeded packed objects and register the project for cache retention.

`add` first resolves its new reference, sometimes downloading a single file to learn its name. it writes the manifest before syncing and attempts to restore that manifest on sync failure. `remove` changes the manifest and lock, then removes unwanted tracked files without downloading.

planning produces target paths, not a serialized dry-run action plan. JSON contains the completed command's report. resolving, downloading unknown hashes and writing the lock can precede detection of a destination clash.

`get` builds temporary locked-file values and uses the same archive expansion, but writes no project metadata. `status`, `path` and cache commands have their own read paths. see [CLI](cli.md) for their effects.

## concurrency

the async command runtime uses up to four worker threads. file-download tasks are queued at up to twice `FETCHLOOM_JOBS`; network permits bound active metadata requests and downloads. each response streams through a 16-slot channel to a blocking writer that hashes and writes it. prefix hashing and final flushes also use blocking tasks.

archive extraction runs up to `min(4, ceil(logical CPUs / 4))` archives at once. each archive has up to eight writer threads. members up to 1 MiB travel to writers in memory; larger members spill to disk while being read. queues are bounded, but listings, plans and archive member records still grow with file count.

placement, project metadata and many cache-index operations run synchronously on command futures. large fallback copies overlap reading and writing with three reusable 1 MiB buffers. cache verification uses the logical CPU count for its worker count. `FETCHLOOM_JOBS` does not control those workers.

## sources

`Reference` is a closed enum. each source returns a listing of paths, optional sizes and checksums, download URLs, and available metadata. credentials are collected by the command context, not stored on each file.

DOIs use `https://doi.org/api/handles/<doi>`. the JSON handle's URL value selects a source. an unknown host or another DOI is refused; the landing page is not downloaded to discover its type.

source tests implement `Api` with in-memory recorded responses from `tests/fixtures/sources`. they are source-specific tests, not a shared conformance suite. current adapter coverage is in [scope](scope.md#sources), and recorded references are in [formats](formats.md#what-each-source-records).

| source | how | notes |
|---|---|---|
| URL | one GET | the first download defines the SHA-256 |
| zenodo | records API | MD5 per file, rate limit headers, old `/record/` URLs redirect |
| figshare | articles API | MD5, an id with a version pins it |
| dataverse | native API, `format=original` for tabular files | checksum type per file, MD5 on harvard |
| osf | files API, folders walked, `links.next` paging | SHA-256 and MD5 |
| hugging face | tree API at a commit, `/resolve/` for bytes | SHA-256 for large files, served through a CDN with range support. a branch is resolved to its commit before locking |
| kaggle | datasets API, then a redirect to signed storage | no checksum. a rate limit can look like a 404 |
| google drive | the `uc` endpoint, then the confirm form for large files | "too many users" is its own retry-later error. folders are read from the folder page |
| dropbox | the shared link with `dl=1` | a folder arrives as one zip, treated as an archive |

## the cache

objects are read-only files named by SHA-256. an ordinary download writes to a partial file, computes its hash, flushes and seals it, then publishes under that hash. the caller checks expected size and checksums after publication; a rejected transfer can leave an unreferenced object for gc.

publication normally uses a hardlink that fails if the destination exists. the fallback is an existence check followed by rename, which has a race on systems where rename replaces an existing destination. it is not a universal no-clobber guarantee.

partials use OS file locks. a competing process waits for the owner; a filesystem that refuses locking gets private files under `tmp/` instead, losing cross-run resumption. checkpoints contain the received prefix's hash and are saved about once a second. resuming rehashes that prefix before appending.

cache lookup checks file type and known size, not content. `cache verify` rehashes objects. the [layout and records](formats.md#the-cache) separate objects from URL history, MD5 mappings, archive trees and project registrations.

successful project sync registers its root. gc follows registered locks to the objects they need, including archive members. an unreadable lock stops gc. unneeded objects become eligible after 24 hours by modification time. deleting a cache name does not remove a placed hardlink, reflink or copy.

## placing files

new placements try reflink, then hardlink, then copy. a `Placer` remembers unsupported methods for its destination tree during the run, not in a persistent per-volume map. `copy = true` still allows a writable reflink, but skips hardlinks.

reflinks have independent file contents after a write. hardlinks are the same file as the cached object. default placements are read-only; writable placement is opt-in. detection of onedrive, dropbox, icloud and macos file-provider folders skips hardlinks but still tries reflinks. fallback copies cost additional disk space.

never make a hardlinked input writable to edit it. that edits the cached object too. before a project sync, changed hardlinks recorded in that project's state cause the associated objects to be hashed and removed if damaged. another project's reuse is not protected by that audit. cache writers must be trusted.

`plan` rejects overlapping dataset folders, file/folder clashes and folded-name collisions. its fold is ASCII lowercase, or NFC followed by uppercase then lowercase for unicode. path components longer than 255 UTF-8 bytes are refused on every platform. windows additionally rejects device names, trailing dots or spaces, and forbidden characters.

manifest and locked paths reject traversal. the project validates the `data_dir` chain. placement checks child directories and refuses links or junctions; deletion rechecks its parent chain. these are path checks, not a sandbox against another process swapping directories during a run. scans skip links rather than following or reporting them.

metadata files use a sibling file, flush and rename. data-file replacement is different: the old file is removed first, and copies write directly to their destination. a hard interruption can leave a missing or partial destination. a later sync may preserve a partial copy as an edit, so inspect `status` before forcing replacement.

state stores hash, method, size and modification time, with no file id. an identical untracked file can be adopted and later removed as managed data. removing an edited unneeded file from state leaves its bytes as an extra file. [formats](formats.md#the-fetchloom-folder) records these fields and [CLI](cli.md#status-and-hand-edits) explains force.

## the network

reqwest uses tokio, HTTP/2 and rustls with the platform verifier. ring is selected as the crypto provider. the system proxy settings are enabled. the user agent is `fetchloom/<version>`.

| control | value |
|---|---|
| active network operations | 8 by default; `FETCHLOOM_JOBS` accepts 1 to 256 |
| per-host slots | 4, keyed by the initial hostname, including while following redirects |
| connect timeout | 45 s |
| send and response-header deadline | 75 s per hop, combining connect and response allowances |
| body idle timeout | 30 s between received chunks; no total download deadline |
| redirect limit | 10 per request attempt |
| metadata body limit | 64 MiB |
| transient retry budget | 5 attempts including the first |
| backoff | jittered exponential delay; `Retry-After` is a floor; above 60 s fails instead of waiting |
| download diversion pages | at most 3, each at most 4 MiB, when a source supplies a page handler |

retryable statuses are 408, 425, 429, 500, 502, 503 and 504, along with transient connection and timeout failures. exhausted retries report `network.unreachable`. range restarts and redirects are separate from the retry budget. a requested retry delay above 60 s produces `network.rate_limited` while retries remain; it does not trigger a mirror search. proxy preflight DNS lookup happens before the send deadline.

resumption uses a pinned SHA-256 or an `If-Range` validator: a strong `ETag`, else `Last-Modified`. unsuitable responses restart from zero. a resumed checksum mismatch gets one full restart before failing. unpinned `get` uses conditional requests for repeat downloads.

a locked 404 or 410 can trigger one source re-resolution per recorded reference in the batch. a new URL is matched by file path, or by a lone file in a lone-file dataset. its bytes must satisfy the old pin. other failures do not trigger this recovery.

### trust

trusted hosts come from plain URLs passed to `add`, from the written host of a `get` reference, and from URL or dataverse entries in `data.toml`. trust is host-wide, not limited to one path or port. other hosts have private addresses refused or filtered out of direct DNS results.

redirects are followed explicitly so every hop is checked. behind a proxy, untrusted names are looked up locally first and rejected if any returned address is private. an unresolvable name is left to the configured proxy. the proxy itself is trusted.

source credentials attach only to their configured host over HTTPS on port 443. once a redirect leaves the original origin, no later hop receives those credentials. offline mode stops requests before sending and stops the resolver before lookup.

project references reject recognized secret query keys on resolution. URL display masking is a wrapper used at call sites, not an invariant of `url::Url`. one-off URL records and successful `get` JSON retain raw URLs. do not share those records when fetching a signed link.

a shared writable cache trusts every writer. partial-download handles refuse links and special files; on unix, a resumed partial must belong to the current user and have one hardlink. cache subdirectories are checked when created or first used. these checks do not authenticate object contents or isolate users from one another.

## archives

the CLI chooses candidates by filename; byte inspection chooses the decoder. names and placement rules are in [CLI](cli.md#archives). zip uses its central directory to check entries first; tar is read sequentially. both feed the same name and byte guard.

| limit | counts |
|---|---|
| 100,000 listed files per dataset | before selection; OSF counts files and folders together |
| 1,000,000 archive entries | files and directories exposed by the reader |
| 1 TiB unpacked | actual member bytes across one archive |
| 200:1 expansion | bytes produced against compressed bytes read; checked once output reaches 4 MiB |
| 16 MiB tar metadata per member | headers, long names and pax records read outside the member body |

names become NFC with leading `./` removed. windows-made zip separators become `/`. absolute paths, traversal, control characters, duplicate files, folded-name clashes and file/folder clashes fail. repeated identical tar directory declarations are allowed; duplicate zip directory names are rejected by the zip reader.

links, devices, fifos, sparse tar files, encrypted zip entries and unsupported zip compression methods are refused. windows-specific name restrictions apply on windows. file permissions and timestamps from the archive are not restored.

members are written and sealed in staging files. none is published until the archive passes its checks. a publication IO failure can leave already published members for reuse or gc. the downloaded packed object can also remain after a refused extraction.

successful unpacking records a sorted member tree under the archive hash. a locked tree hash is compared before placement. the packed object is dropped when no registered project or current lock needs it, subject to deletion succeeding. rebuilding a missing member object requires the original archive again.

## project coordination and observability

commands that modify the project acquire `.fetchloom/lock`. `status` and a ready `path` read without it. `path` rereads after waiting to sync. standalone `lock` currently reads its manifest and old lock before acquiring the lock. on a filesystem without working file locks, serialize modifying commands yourself.

human steps are printed directly. warnings are collected, and the JSON document combines the final result, warnings, one optional error and counters. it is not a rendering of the progress-event stream.

`-v` adds details and total counters; `-vv` enables debug events. `FETCHLOOM_TRACE` records instrumented spans and events, including source resolution, rather than a complete phase profile. there is no custom panic report. [formats](formats.md#the-json-document) defines error kinds and counters; [performance](performance.md) holds measurements.

## adding a source

add a module and a `Reference` variant, then register parsing, pasted-link routing, resolution and display in `source.rs`. add credentials or a download-page handler when needed. source fixtures test listing behavior; CLI tests cover transfer and placement. one failing source can stop resolution of a project, so module separation does not mean independent success within a run.

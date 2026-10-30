# architecture

one rust crate builds `fetchloom` and `fl`. both call the same library entry point.

## modules

| module | owns |
|---|---|
| `cli` | argument parsing, settings, command orchestration and final reports |
| `cli/sync` | relocking, download selection, archive work and placement decisions |
| `project` | manifest, lock and state types, project discovery, metadata writes and project locking |
| `source` | reference parsing, DOI routing and source listings |
| `net` | HTTP, retries, rate limits, partial-download streaming, split downloads, credentials and address checks |
| `cache` | objects, partials, indexes, project registrations, gc and verification |
| `archive` | format readers, member checks, limits and staged member writes |
| `threads` | the optional local worker ceiling and archive worker budgets |
| `plan` | destination expansion and collision checks |
| `place` | reflinks, hardlinks, copies, directory checks and synced-folder detection |
| `output` | human rendering, progress, warnings and the JSON envelope |
| `digest`, `name`, `glob`, `units`, `stop`, `work` | hashes, paths, patterns, formatting, cancellation and counters |

there is no separate model crate. the python module in `python/fetchloom` has no bindings: it runs the installed `fetchloom` with `--json`. core types live with their owners. `net` writes through `cache`; `cli/sync/place` combines the plan, state and filesystem. dependencies such as `url::Url` cross module boundaries.

## the project loop

1. read the manifest, lock and local state. keep lock entries whose spec hashes match, unless an upgrade requests them again.
2. resolve changed entries and filter their source listings. known hashes may come from the source, the old lock or the MD5 index. download unknown hashes and unpack new archives to learn their tree hashes.
3. write `data.lock` if its contents changed. standalone `lock` does no placement and need not fill the cache with already pinned files.
4. expand the lock into destination targets. an archive without a usable member record is unpacked before its destinations can be planned. compare targets with state and disk metadata to choose keep, adopt, place, replace or leave.
5. fetch only the bytes needed by place and replace actions. a missing archive member requires the original archive again when it is not cached. check pinned size, SHA-256 and any stated MD5 on downloads.
6. remove unwanted unchanged tracked files, place the targets, and write state if it changed or settles files hashed for being as new as the old state. drop unneeded packed objects and register the project for cache retention.

the state also records the SHA-256 of `data.toml` and `data.lock` as `sync` leaves them, and whether every file they ask for ended up in place. when the next `sync` reads both byte for byte the same after such a complete run, the state records exactly the files they ask for. if the scan in step 1 then finds each of them as recorded, with nothing hashed as racy or put in doubt by a mark, the run is done: it reads no archive member records, does not relock and plans nothing. the scan still runs, so an edit still sends the run through every step. a run that left a file out, kept an edited one or failed records itself as incomplete, and other commands that change the state drop the record.

`add` first resolves its new reference, sometimes downloading a single file to learn its name. it writes the manifest before syncing and attempts to restore that manifest on sync failure. `remove` changes the manifest and lock, then removes unwanted tracked files without downloading.

planning produces target paths, not a serialized dry-run action plan. JSON contains the completed command's report. resolving, downloading unknown hashes and writing the lock can precede detection of a destination clash.

`get` builds temporary locked-file values and uses the same archive expansion, but writes no project metadata. `status`, `path` and cache commands have their own read paths. see [CLI](cli.md) for their effects.

## concurrency

the worker counts below are the defaults. the command context reads `FETCHLOOM_THREADS` once and passes its ceiling to each local worker group and the async runtime. unset or empty preserves those counts. the cap does not bound the process's total OS threads: blocking network writers, DNS helpers and progress threads are separate.

the async command runtime uses up to four worker threads. file-download tasks are queued at up to twice `FETCHLOOM_JOBS`; network permits bound active metadata requests and downloads. each response streams through a 16-slot channel to a blocking writer that hashes and writes it. the streams of a split download share one writer, which puts each piece at its offset and hashes the contiguous prefix as it grows; bytes that arrived past the prefix are read back and hashed once the prefix reaches them. prefix hashing and final flushes also use blocking tasks.

archive extraction runs up to `min(4, ceil(logical CPUs / 4))` archives at once. each archive has up to eight writer threads, no more than the logical CPU count. with a thread cap, active archive readers share that budget with their writers; a reader with no writer budget stores and publishes members itself. members up to 1 MiB travel to writers in memory; larger members spill to disk while being read. queues are bounded, but listings, plans and archive member records still grow with file count.

placement, project metadata and many cache-index operations run synchronously on command futures. placing files and looking up their objects use up to eight threads, at most one per 64 files and no more than the logical CPU count. the same worker ceiling applies when hashing files whose metadata cannot establish that they are unchanged. the first file is placed alone, so the others start knowing which methods the filesystem refuses. off linux, uncapped large fallback copies overlap reading and writing with three reusable 1 MiB buffers. with a thread cap, each placement worker copies without a reader thread of its own. linux copies inside the kernel with `copy_file_range`. cache verification uses the logical CPU count for its worker count. `FETCHLOOM_JOBS` does not control those workers.

## sources

`Reference` is a closed enum. each source returns a listing of paths, optional sizes and checksums, download URLs, and available metadata. a listing also keeps paths it cannot fetch, with their reasons, so selection can explicitly leave them out. credentials are collected by the command context, not stored on each file.

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
| kaggle | API view for the newest version, list in pages of 200, then each file on its own with `raw=true`, redirected to signed storage | no checksum. public datasets need no token; a 403 can mean private or missing. competitions are refused |
| google drive | direct downloads from `drive.usercontent.google.com`, with a confirmation-page handler | quota pages ask you to retry later. `open?id=` follows drive's redirect; folders are walked through `embeddedfolderview` |
| dropbox | the shared link with `dl=1` | a folder arrives as one zip, treated as an archive |

## the cache

objects are read-only files named by SHA-256. an ordinary download writes to a partial file, computes its hash, flushes and seals it, then publishes under that hash. the caller checks expected size and checksums after publication; a rejected transfer can leave an unreferenced object for gc.

publication normally uses a hardlink that fails if the destination exists. the fallback is an existence check followed by rename, which has a race on systems where rename replaces an existing destination. it is not a universal no-clobber guarantee. a rename that fails because another writer published the object since the check counts the bytes as already cached. a damaged object is marked as changed and stays at its name until verified new bytes replace it with an atomic rename. concurrent repairs never remove the name between checking and publishing. when windows refuses a read-only destination, the cache clears its flag through an unfollowed handle, renames the new file over it, and restores the old file's flag through that handle, including any placed hardlinks. a refused replacement leaves the old name in place.

partials use OS file locks. a competing process waits for the owner; a filesystem that refuses locking gets private files under `tmp/` instead, losing cross-run resumption. checkpoints contain the received prefix's hash and, for a split download, each range finished past it with its own hash. they are saved about once a second. resuming rehashes that prefix before appending, keeps the ranges whose bytes still match and fetches the rest.

cache lookup checks file type and known size. writable objects and objects no newer than their change marks are hashed too, so retained damaged bytes are cache misses. `cache verify` rehashes every object and marks damage without deleting it. the [layout and records](formats.md#the-cache) separate objects from URL history, MD5 mappings, archive trees, change marks and project registrations.

successful project sync registers its root. gc follows registered locks to the objects they need, including archive members. an unreadable lock stops gc. unneeded objects become eligible after 24 hours by modification time. deleting a cache name does not remove a placed hardlink, reflink or copy.

FAT32 and exFAT cannot hardlink, and windows refuses to remove a read-only file there. when a removal is refused, the cache learns whether its volume can hardlink by trying once in `tmp/`. on a volume that cannot, the file has no other name, so the cache clears its read-only flag and removes it, and sets the flag again if the removal still fails. on a volume that can hardlink the flag stays: it belongs to every name of the file, including a hardlink a project placed.

## placing files

new placements try reflink, then hardlink, then copy. a `Placer` remembers unsupported methods for its destination tree during the run, not in a persistent per-volume map. a refused hardlink counts for the rest of the run only when the destination volume also refuses to link a file of fetchloom's own, tested once a run in the same place removals test; a file refused alone, as linux's `protected_hardlinks` refuses another user's object, is copied by itself. `copy = true` still allows a writable reflink, but skips hardlinks.

reflinks have independent file contents after a write. hardlinks are the same file as the cached object. default placements are read-only; writable placement is opt-in. a copy whose permissions already match the requested mode needs no permission change; failures to change its mode still stop placement. detection of onedrive, dropbox and icloud folders skips hardlinks but still tries reflinks. fallback copies cost additional disk space. placement summaries count links and copies, including zero-byte copies; their byte total counts only copied payloads. when a drive refuses both reflinks and hardlinks, `sync` and `get` say that is why their files were copied. a cache on another drive is named as the cause only when the destination drive makes links itself.

removing or replacing a placed file follows [the cache's rule](#the-cache) for read-only files, judged on the volume that holds the files and tested there at most once a run: in `.fetchloom` for a project, and with a hidden file in the folder it fills for `get`. a file the state records as a hardlink never has its flag cleared, so on a volume that refuses to remove read-only files, removing it fails.

never make a hardlinked input writable to edit it. that edits the cached object too. before a project sync, changed hardlinks recorded in that project's state cause the associated objects to be hashed and marked if damaged. an object found damaged that way, or by `cache verify`, also gets a mark in the cache. cache writers must be trusted.

the mark reaches other projects. an object no newer than its mark, or whose read-only flag was cleared, is hashed before sync or `get` places anything from it. changed bytes are marked and fetched again; a failure to write the mark stops the run. an intact writable object is sealed read-only again before placement. a hardlink placed, adopted or last hashed before the mark, whose size and time still match its record, is hashed too, since it may name an older copy of the object than the one that changed. a read failure stops `sync` or `status` without replacing the file. one holding other bytes is listed in `status` as a changed linked file, and the next sync puts the locked bytes back. one still holding the locked bytes is kept, and the state records the start of the scan that checked it, so it is hashed once. using the scan's start keeps marks written while files are being placed visible to the next run. on windows a changed link is the usual case: a folder listing keeps the old size and time of a hardlink changed through another name, and `sync --force` reads every hardlink through its own name. on linux, an edit that changes the size or time shows through every name of the file, so each project holding the link lists the file as edited and keeps it until `sync --force`.

`plan` rejects overlapping dataset folders, file/folder clashes and folded-name collisions. its fold is ASCII lowercase, or NFC followed by uppercase then lowercase for unicode. path components longer than 255 UTF-8 bytes are refused on every platform. windows additionally rejects device names, trailing dots or spaces, and forbidden characters.

when a recorded path is absent from a listing, the scan looks for exactly one listed name with the same fold. it uses the recorded spelling only if `symlink_metadata` on that spelling finds a regular file with the listed size and modification time. on unix, the device and inode must match too. distinct files on a case-sensitive filesystem and ambiguous folded names stay separate. a changed lock path or `data_dir` spelling carries the verified record to the wanted name before placement or removal, preserving edits and hardlink doubts. names already present need no folding or extra metadata reads.

manifest and locked paths reject traversal. the project validates the `data_dir` chain. placement checks child directories and refuses links or junctions; a project's data folder is checked again, while the folder `get` writes into may itself be a link. deletion rechecks its parent chain the same way. these are path checks, not a sandbox against another process swapping directories during a run. scans skip links rather than following or reporting them.

metadata files stage in `.fetchloom/` when it exists, otherwise beside their destination; they are flushed and renamed into place. data-file replacement removes the old file first. copies are written to hidden `.fetchloom-<16 hex digits>.copy` files in the destination folder, then published without overwriting an existing name. a hard interruption can leave a missing destination or a hidden copy temporary, but never partial bytes at the destination name. `get` ignores these copy temporaries on its next run. an interruption after the new file is placed and before the state is written leaves the locked bytes recorded as the old file. a changed file of the locked size is therefore hashed, and one that holds the locked bytes is adopted with its new size and time. if `copy = true` asks for a writable file, a read-only match is replaced instead.

sync reads a file's size and modification time again right before removing or replacing it, and keeps the file when either changed since the scan. an edit made while downloads ran therefore stays. state records each placed file's size and time as read right after that file is placed, so an edit made while later files are placed shows as an edit too.

state stores hash, method, size, modification time, whether the file was placed writable, and when fetchloom last knew it held its locked bytes, with no file id. a file recorded no older than the state file itself is hashed before its size and time are trusted, as git does with racily clean index entries; on a clock as coarse as FAT's, an edit in the tick a file was placed leaves both unchanged. an identical untracked file can be adopted and later removed as managed data. an adopted file that is the cached object under another name, as a run cut off before its state write leaves one, is recorded as a hardlink, so the audit covers it. removing an edited unneeded file from state leaves its bytes as an extra file. [formats](formats.md#the-fetchloom-folder) records these fields and [CLI](cli.md#status-and-hand-edits) explains force.

## the network

reqwest uses tokio, HTTP/2 and rustls with the platform verifier. ring is selected as the crypto provider. the system proxy settings are enabled. the user agent is `fetchloom/<version>`.

| control | value |
|---|---|
| active network operations | 8 by default; `FETCHLOOM_JOBS` accepts 1 to 256 |
| download bandwidth | uncapped by default; `FETCHLOOM_RATE` sets one shared byte budget for all response bodies, split streams included |
| per-host slots | 4, keyed by the initial hostname, including while following redirects |
| connect timeout | 45 s |
| send and response-header deadline | 75 s per hop, combining connect and response allowances |
| body idle timeout | 30 s between received chunks; no total download deadline |
| redirect limit | 10 per request attempt |
| metadata body limit | 64 MiB |
| transient retry budget | 5 attempts including the first |
| backoff | jittered exponential delay; `Retry-After` is a floor; above 60 s fails instead of waiting |
| rate limit waits | until the host's limit resets, when that is within 120 s; at most 10 for one request before they count as attempts |
| split downloads | files of at least 64 MiB, once their one stream moved less than 1 MB/s over 2 s; then up to 8 streams within the host's free slots. `FETCHLOOM_SPLIT=0` never splits, and `FETCHLOOM_SPLIT=1` splits from the first answer. under `FETCHLOOM_RATE` a stream is never judged slow, since more streams would share the same cap |
| download diversion pages | at most 3, each at most 4 MiB, when a source supplies a page handler |

retryable statuses are 408, 425, 429, 500, 502, 503 and 504, along with transient connection and timeout failures. exhausted retries report `network.unreachable`. range restarts and redirects are separate from the retry budget. a requested retry delay above 60 s produces `network.rate_limited` while retries remain; it does not trigger a mirror search. proxy preflight DNS lookup happens before the send deadline.

### bandwidth cap

`net/rate` owns the shared token bucket. the command context passes the same bucket to every network client it creates, so retries, metadata requests and split streams spend one budget. the bucket starts full and holds one second's bytes or 1 MiB, whichever is larger. each received chunk waits for its tokens before reaching its writer or in-memory body. waits use tokio time and select on the stop signal; they do not count toward the server's body idle timeout. a queued stream keeps its turn while sleeping, so other streams cannot take its refill. an unset cap adds only an optional-value check on the chunk path, with no locks or atomics.

### rate limits

every answer may state the limits of its host: `X-RateLimit-Limit`, `-Remaining` and `-Reset` as zenodo sends them, the same names without `X-`, or `RateLimit` with `RateLimit-Policy` as hugging face sends them, where each named policy is a limit of its own. a reset is read as seconds since 1970 against the answer's `Date`, or as seconds from the answer when it is small. each hop of every request, ranges included, first checks its host's limits: while one has no more requests left than the requests on their way to that host, no new request starts until it resets, and one line says why, such as `Waiting 40 s for zenodo.org, whose limit of 133 requests is used up`. a wait that ends within 10 s of the one a line told of passes without another line, such as the few seconds zenodo's 429s ask for just after its limit resets. requests on their way may already be in the remainder a host stated, so a window can go unused by at most that many requests. answers from one window keep their lowest count, and an answer from an earlier window changes nothing. a limit that resets more than 120 s away is left for the host to enforce.

a 429 uses up the limits it names, or all it states, for at least its `Retry-After`. when that resets within 120 s, the next try waits for the reset and is not one of the request's five attempts; after 10 such waits for one request they count again. a 429 that resets later fails as `network.rate_limited` at once. `Retry-After` on any other answer, as zenodo sends it on every success, changes nothing.

### split downloads

a download of at least 64 MiB may run over several streams when the server answers ranges, by a `206` or `Accept-Ranges: bytes`, and something proves the pieces belong to one version: a pinned SHA-256, or a validator for `If-Range`. it still starts as one stream, written in order like any other, and measures what that stream moves in each 2 s window. once a window moved less than 1 MB/s while at least 16 MiB are left, the stream hands over: its writer, and the rest of its answer, read exactly up to the bytes the writer holds, go on as the first stream of the split. a stream that keeps up never enters the split path: in the lab, splitting from the first answer made a 1 GiB file 17% slower on lan, where one stream moved about 180 MiB/s. `FETCHLOOM_SPLIT=0` keeps every file to one stream, and `FETCHLOOM_SPLIT=1` splits from the first answer, to measure the split path. a resumed download whose checkpoint holds ranges past its prefix goes to the split path at once, since only it fills the gaps between them.

the split path adds a second stream at once when the one stream's rate is known, and otherwise after one window. every 250 ms it checks its rate over 2 s windows, and adds a stream while each added stream raised the total by at least 15%, up to 8 and only when the host and the overall limit have a slot that nothing else waits for. added streams use HTTP/1.1 connections of their own, since streams of one HTTP/2 connection share its window, and each asks for its range with `If-Range`. a stream that finishes takes the upper half of the largest range left when that half is at least 8 MiB. a `200` answer to a range, or three failed streams, stop the splitting for that file, and the rest comes over one stream. a range answer from another version restarts the download. a split download whose hash does not match is restarted once as one stream before it fails, as a resumed one is.

OSF file downloads retry a 403 XML answer only when it explicitly says an `X-Amz` storage request expired. the next attempt starts at the stable OSF URL to obtain a new storage link, within the same retry budget. other 403 answers remain access errors.

resumption uses a pinned SHA-256 or an `If-Range` validator: a strong `ETag`, else `Last-Modified` when the server sent no `ETag`. unsuitable responses restart from zero, and so does a range answer that states another `ETag` or `Last-Modified` than the saved bytes came with, since a server that ignores `If-Range` sends the rest of its current version. a resumed checksum mismatch gets one full restart before failing. unpinned `get` uses conditional requests for repeat downloads.

a locked 404 or 410 can trigger one source re-resolution per recorded reference in the batch. a new URL is matched by file path, or by a lone file in a lone-file dataset. its bytes must satisfy the old pin. other failures do not trigger this recovery.

### trust

trusted hosts come from plain URLs passed to `add`, from the written host of a `get` reference, and from URL or dataverse entries in `data.toml`. trust is host-wide, not limited to one path or port. other hosts have private addresses refused or filtered out of direct DNS results.

redirects are followed explicitly so every hop is checked. behind a proxy, untrusted names are looked up locally first and rejected if any returned address is private. an unresolvable name is left to the configured proxy. the proxy itself is trusted.

source credentials attach only to their configured host over HTTPS on port 443. once a redirect leaves the original origin, no later hop receives those credentials. offline mode stops requests before sending and stops the resolver before lookup.

project references reject recognized secret query keys on resolution. URL display masking is a wrapper used at call sites, not an invariant of `url::Url`. one-off URL records retain raw URLs; successful `get` JSON masks recognized query secrets. do not share local URL records when fetching a signed link.

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

members are written and sealed in a per-archive staging folder. the writer holds an OS lock until every member is published or discarded. the next unpack or gc takes unheld staging locks and removes those folders immediately, while held locks protect live runs regardless of age. if locking is unavailable, staging stays until its folder and files have gone 24 hours without a write. none of the members is published until the archive passes its checks. a publication IO failure can leave already published members for reuse or gc. the downloaded packed object can also remain after a refused extraction.

unpacking records a sorted member tree under the archive hash, unless data.lock pins another tree. a locked tree hash is compared before placement. an archive that unpacks to another tree than the lock pins is hashed first: when its bytes changed in the cache, as a plain tar's can without any error, it is marked and downloaded again to replace the damaged bytes, and only an intact archive fails with advice to pin what this version unpacks. the packed object is dropped when no registered project or current lock needs it, subject to deletion succeeding. rebuilding a missing member object requires the original archive again.

## project coordination and observability

commands that modify the project acquire the OS lock on `.fetchloom/lock`, then claim `.fetchloom/owner`. windows and WSL honor the same owner record even though their OS locks do not see each other. waiting commands poll every 200 ms, report the holder once and accept ctrl-c. the guard removes the owner before releasing the OS lock, including on error. [formats](formats.md#the-fetchloom-folder) defines the owner record and recovery rules.

`status`, a ready `path` and `lock --check` read without either lock, since they change nothing. `path` rereads after waiting to sync.

human steps are printed directly. warnings are collected, and the JSON document combines the final result, warnings, one optional error and counters. it is not a rendering of the progress-event stream.

`-v` adds details and total counters; `-vv` enables debug events. `FETCHLOOM_TRACE` records instrumented spans and events: the command, source resolution, and the phases of a project sync from reading the manifest, lock and state through scanning, relocking, planning and placing. downloads and unpacking have no spans of their own. there is no custom panic report. [formats](formats.md#the-json-document) defines error kinds and counters; [performance](performance.md) holds measurements.

## adding a source

add a module and a `Reference` variant, then register parsing, pasted-link routing, resolution and display in `source.rs`. add credentials or source-specific download response handling when needed. source fixtures test listing behavior; CLI tests cover transfer and placement. one failing source can stop resolution of a project, so module separation does not mean independent success within a run.

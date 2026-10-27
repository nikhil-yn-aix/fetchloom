# architecture

one binary. a pure core decides what to do, and a thin shell does the IO.

## the path every command takes

```
data.toml -> resolve -> lock -> plan -> fetch -> unpack -> place -> report
```

1. read. `data.toml`, `data.lock` and the project state become plain types.
2. resolve. for each dataset whose entry changed, its source turns the reference into a listing: every file's path, size, stated checksum and URL, plus title, version, license and DOI. sources never download file bodies.
3. lock. the filtered listing becomes `data.lock` entries. a file whose SHA-256 is still unknown is fetched first, because its hash comes from its bytes.
4. plan. the lock is compared with the cache and with `data/`, and the result is a list of actions: fetch, unpack, place, replace, skip, report. every destination path in the project goes into one map first, and two files on one path, a file where a folder must be, or two datasets with overlapping paths are refused, comparing names the way a case-insensitive disk would. the plan is a plain value, so tests and `--json` can inspect it before anything happens.
5. fetch. the network layer streams each missing file into the cache, hashing as it writes.
6. unpack. archives unpack member by member into the cache.
7. place. files appear under `data/` by reflink, else hardlink, else copy.
8. report. one stream of events feeds both the human renderer and the JSON renderer.

## modules

| module | owns | never touches |
|---|---|---|
| `model` | references, digests, dataset paths, the `data.toml` and `data.lock` types, filters, the plan | files, sockets, clocks |
| `source` | one module per source, the DOI router, listings | the cache, `data/` |
| `net` | the HTTP client, retries, resume, rate limits, credentials, address rules, offline mode | where the bytes end up |
| `cache` | objects, partial downloads, the stated checksum index, unpack records, project registrations, gc and verify | HTTP, sources |
| `archive` | reading zip and tar, every safety limit | the network |
| `project` | finding the project, editing `data.toml`, writing `data.lock`, the state file, the project lock, placing and removing files under `data/` | HTTP |
| `cli` | arguments, environment, the order of work in each command, rendering, prompts, exit codes | protocol details |

dependencies point inward. `cli` uses every module, `model` uses none, and no module reaches into another's internals. the python package is a few lines that run the binary with `--json`.

## concurrency

network work runs as tokio tasks. disk work (writing, hashing, unpacking, placing) runs on a bounded pool of blocking threads, fed through bounded channels, so disk calls never block network tasks and memory stays flat. async file IO is slower than plain threads for this kind of load.

## sources

the set of sources is closed and lives in this repository, so a source is an enum variant with its own module, not a trait object or a plugin.

- a listing holds each file's path, its size and checksum when stated, a stable URL and any headers that URL needs.
- every source passes the same conformance cases against recorded responses kept beside its tests.
- a pasted link is matched by host: google drive, dropbox, zenodo, figshare, hugging face, kaggle, osf, dataverse, doi.org, or a plain URL.
- a DOI is resolved at doi.org without following the redirect. the host it lands on picks the source. a DOI that lands on an unknown host fails and names that host.
- what each source writes into `data.lock` is listed in [formats](formats.md#what-each-source-records).

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

- objects are named by SHA-256, one read-only file each, under a two-character fan-out.
- writes need no locks. a download streams into `tmp/`, is hashed, flushed to disk, sealed read-only, then put in place by an operation that fails when the name already exists. of two processes racing for one object, exactly one places it and the other finds it there. this stays safe on NFS, lustre and GPFS, where file locks cannot be trusted.
- flushes run in parallel batches, never one blocking flush at a time. before an object is used, its size is checked against `data.lock`, which catches a file cut short by a crash. `cache verify` hashes every object again.
- an unfinished download lives in `partial/` beside a small record of its URL, validators and bytes received. one process owns it at a time, claimed by rename, and a later run continues it.
- every sync registers its project under `projects/`. `cache gc` keeps every object that a registered project's `data.lock` needs, following archives to the members they unpacked into. it removes the rest only when older than a day, and never touches an unfinished download younger than a day.
- removing an object never breaks a project. placed files are links or copies, so they keep their bytes when the cache's name for them goes. at worst, a later project downloads that object again.
- `CACHEDIR.TAG` tells backup tools to skip the folder.

## placing files

- the first placement from the cache into a folder tries reflink, then hardlink, then copy, and remembers what worked for that volume for the rest of the run.
- a reflink shares blocks but is its own file, so editing it is safe. APFS, btrfs, XFS, ReFS and dev drives support it.
- a hardlink costs no space and no extra inode, but it is the cached file itself, so objects are read-only. a file can carry about 1,000 hardlinks on NTFS and 65,000 on ext4. past that, the file falls back to the next method.
- when a placed file turns out to have changed, its object is hashed before anything uses it again. an object that changed is deleted and fetched again.
- inside a folder that onedrive, dropbox or icloud drive syncs, files are copied, never hardlinked, because those tools break hardlinks or upload every link as a full file.
- a copy is said out loud, with the bytes it cost and how to avoid it.
- a dataset with `copy = true` gets writable copies, for tools that write into their inputs.
- no symlinks. windows needs admin rights for them, and a cleared cache would leave them dangling.

## the network

- reqwest on tokio, with rustls verifying against the operating system's certificate store, so institutional certificates work. HTTP/2 and the system proxy are on.
- 8 downloads at a time, at most 4 per host.
- timeouts limit silence: 45 s to connect across all of a host's addresses, 30 s waiting for headers, 30 s between bytes. a slow 100 GB download finishes and a stalled one fails.
- retries cover connection errors, timeouts, 408, 429 and 5xx, with jittered backoff up to 60 s and at most 5 attempts. `Retry-After` is a floor. when a host asks for more than 60 s, the next known location is tried, or the error says how long the host asked for.
- resume is a range request from the last verified byte, used only when the file's identity is fixed by a pinned hash, an immutable version or a strong ETag. otherwise the download starts over.
- credentials attach only to their own scheme, host and port, only over https, and are dropped on any redirect to another origin.
- a URL that did not come from the command line may not reach a private, loopback or link-local address. the check runs on the address each connection actually uses, for the first request and every redirect, so a hostile `data.lock` cannot reach into a local network.
- URLs live in a type that hides tokens and signed query values whenever they are displayed or logged.
- offline mode is enforced inside the client, so no source can forget it.
- the user agent is `fetchloom/<version>`.

## archives

- zip, using the `zip` crate at 9.1.2 or later, and tar with gzip, xz, zstd or bzip2.
- one guard enforces every limit on the bytes actually written, never on sizes the archive declares: at most 1,000,000 members, 1 TiB unpacked, and a compression ratio of 200 to 1.
- an archive is refused as a whole for an absolute path, a `..` part, a symlink, a hardlink or special file, a duplicate name, two names that collide on a case-insensitive disk, a name the disk cannot store such as `CON` on windows, or a limit exceeded.
- each member becomes an object in the cache, and the archive's file list is recorded under `unpacked/`. the archive's own object is then deleted, unless a project on this machine keeps it packed.
- an archive unpacks into a folder named after it, beside where it would have been: `train.zip` becomes `train/`. when every member sits under one top folder, that folder is not repeated. a dataset that is a single archive unpacks straight into the dataset's folder.

## the project

- found by walking up from the working folder to the nearest `data.toml`, or from `-C`.
- `data/` must be a real folder inside the project. a symlink or junction anywhere on the way is refused, and placing or deleting never follows a link.
- `data.toml` is edited with `toml_edit`, so comments and order survive.
- `data.lock` is written in a fixed order by our own writer, so the same inputs always give the same bytes.
- each file is replaced atomically: written beside its target, then renamed over it. a command writes `data.toml` first, then `data.lock` once every file it names is in the cache, then the files under `data/`, then the state. each step is safe to repeat, so a run that stops anywhere is finished by running it again.
- `.fetchloom/state` records each placed file's size, modification time and file id, read back by listing folders.
- a command that changes the project holds `.fetchloom/lock`. `path` and `status` read without it when nothing needs syncing, so a thousand cluster jobs asking for the same path never queue. a `path` that must sync takes the lock and checks again.
- fetchloom deletes only files it placed that still match what it placed. a file someone changed or added is kept and listed, and a changed file is overwritten only with `--force`.

## errors

every error has a kind such as `network.timeout`, `integrity.mismatch` or `reference.unresolved`, one sentence saying what happened, and a help line with what to do next, as a command when there is one.

| exit | meaning |
|---|---|
| 0 | done |
| 1 | a `--check` found differences, or the run failed in a way listed in the output |
| 2 | the command line was wrong |
| 10 | a reference did not resolve |
| 20 | the network failed, or `--offline` needed something not in the cache |
| 30 | bytes did not match their hash |
| 40 | a credential is missing or refused, or terms must be accepted on a website |
| 50 | the local disk or the project is in a bad state |

## observability

- every command, request, cache lookup and placement runs inside a `tracing` span carrying its facts.
- `-v` adds detail per dataset and the counters from [performance](performance.md). `-vv` adds debug logs, filtered by `FETCHLOOM_LOG`. `FETCHLOOM_TRACE` writes a timeline that opens in perfetto.
- a panic prints the version, what was running and where to report it.

## platforms

- windows: no admin rights needed and no symlinks. objects carry the read-only attribute, and gc deletes them with the flag that ignores it, so other links to the same file keep it. paths windows cannot store are refused before anything is written. the cache defaults to `%LOCALAPPDATA%`, which onedrive does not sync. defender scans every new file, so a cache on a dev drive, or a defender exclusion, makes datasets of many small files much faster; `-v` says so when it applies.
- macos: reflink on APFS. icloud drive folders get copies and a warning, since "optimize mac storage" can remove local files.
- linux and clusters: safe on NFS, lustre and GPFS without file locks in the cache. in v0.1 a cache is shared safely by one user's processes; sharing one between people comes with the lab features.

## security

- `sync` never runs code from a project.
- tokens come only from environment variables and each source's own token file. they never sit in `data.toml` and never appear in output.
- archives, listings, lock files and links are hostile input until checked.

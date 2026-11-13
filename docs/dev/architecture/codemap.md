# code map

fetchloom is one rust crate whose files group into the layers the [module calls](README.md#module-calls) diagram draws. each row names what a file owns and the item to read first; `Type::name` is a method.

unit tests sit beside their module, in `tests.rs`, `*_tests.rs` or another file declared under `#[cfg(test)]`; this map lists only their shared helpers, under [tests](#tests). a new file under `src/` needs a row here, or the docs test `every_source_file_is_on_the_code_map` fails; it also fails on a row whose file is gone.

## entry

[CLI](cli.md#startup) covers what happens before a command runs.

| file | owns | start at |
|---|---|---|
| `src/main.rs` | the binary; linux musl builds also swap in mimalloc as the allocator | `main` |
| `src/lib.rs` | the module list, and `main`, which installs the bug-report panic hook and runs the cli | `main` |
| `build.rs` | the build script, which stamps the git commit `--version -v` reports into `FETCHLOOM_COMMIT` | `main` |

## cli

`cli::run` reads the command line and settings, then `execute` builds the runtime and hands the command to its file through `dispatch`; what that file returns becomes the `result` of `--json`. [CLI](cli.md) covers adding a command or an option.

| file | owns | start at |
|---|---|---|
| `src/cli.rs` | startup: settings, the cache, the runtime, dispatch and the exit code | `run`, `dispatch`, `Context` |
| `src/cli/args.rs` | the clap definition of every command and option | `Args`, `Command`, `Global` |
| `src/cli/usage.rs` | a command line clap cannot read, answered like any failure, with the command or option meant | `answer` |
| `src/cli/help.rs` | the ideas behind fetchloom or one command's help, as text, and `help --json`, which describes every command in one call | `show` |
| `src/cli/typed.rs` | commands as a person types them: words quoted for a shell, a suggested command checked against clap, this run's own line | `judged`, `command_line` |
| `src/cli/logs.rs` | the `-vv` and `FETCHLOOM_LOG` debug log, and the `FETCHLOOM_TRACE` timeline perfetto opens | `start` |
| `src/cli/project.rs` | `sync` and `lock`, and what project commands share: finding the project, taking its lock, running the sync loop | `sync`, `lock`, `sync_all` |
| `src/cli/project/conflicts.rs` | `lock` on files a git merge left with conflict lines, merged a dataset at a time | `merge` |
| `src/cli/add.rs` | `add`: every byte cached before data.toml or data.lock is written, and a later failure puts back what the add wrote | `add`, `sync_add` |
| `src/cli/add/list.rs` | `add -i` and several links in one add: a dataset per line, placed together | `add_list_held` |
| `src/cli/import.rs` | `add <path>`: files on this machine copied into the cache and pinned by their bytes, never linked, moved or changed | `import_held` |
| `src/cli/links.rs` | link lists for `add -i` and `get -i`: one link a line with an optional SHA-256, the whole list checked before any request | `read`, `Line` |
| `src/cli/remove.rs` | `remove`: a dataset out of data.toml and data.lock, and its unchanged files out of the data folder | `remove` |
| `src/cli/drop.rs` | `drop`: files left out for good, by patterns in data.toml, out of data.lock and, when unchanged, the data folder, asking no server | `run` |
| `src/cli/path.rs` | `path`: where a dataset or file is, syncing that dataset alone first when its files are missing | `path` |
| `src/cli/status.rs` | `status`: how the data folder differs from data.lock, hashing only files their size and time cannot vouch for | `run`, `Status` |
| `src/cli/status/ready.rs` | the readiness rule `status --check` and `sync --strict` share, and its `data.not_ready` error | `of_status` |
| `src/cli/list.rs` | `list`: each dataset's source, size and destination, from data.toml and data.lock alone | `run` |
| `src/cli/export.rs` | `export`: checksums of placed files or links of downloaded ones, for `sha256sum -c`, a link list or aria2c | `run` |
| `src/cli/push.rs` | `push`: pinned files copied from the cache into a folder mirror, with the file lists of their archives | `run` |
| `src/cli/build.rs` | `build`: recipes run in order, their results pinned and placed, data.lock written once after the last step | `run` |
| `src/cli/build/preview.rs` | `build --dry-run`: each step decided as the build would, running and fetching nothing | `preview` |
| `src/cli/build/room.rs` | the disk room the steps may take, said before the first runs, with a question below `build_floor` | `fits` |
| `src/cli/build/accept.rs` | `build --accept`: a kept rebuild pinned and placed, running nothing | `accept` |
| `src/cli/get.rs` | `get`: datasets or files into new folders, once, through the cache projects share; a failed link lets the rest finish | `run` |
| `src/cli/get/folder.rs` | the folder `get` fills, refused when it holds someone else's files | `check`, `put_all` |
| `src/cli/get/file.rs` | `get -o`: one file at exactly that path, never unpacked, never over another file without `--force` | `one` |
| `src/cli/get/preview.rs` | `get --dry-run`: what a get would fetch and where it would land, writing nothing, not even the cache folder | `run` |
| `src/cli/get/remembered.rs` | what an earlier `get` kept of a link only its source can list, which stands in for the listing offline | `keep`, `listing` |
| `src/cli/search.rs` | `search`, and the choice among candidates that `add` and `get` offer | `run`, `pick` |
| `src/cli/cache.rs` | `cache`, `cache gc` and `cache verify`, the projects that use the cache, and the weekly size reminder | `info`, `gc`, `verify` |
| `src/cli/cache/inputs.rs` | `cache gc --inputs`: raw data only built recipes read, freed where a folder mirror holds a copy | `gc` |
| `src/cli/auth.rs` | `auth`: signed-in accounts, signing in and out, and the offer to sign in after a refused download | `run`, `offer` |
| `src/cli/settings.rs` | `settings`: each value a run would use and where it came from, and warnings for misnamed or unread variables | `run` |
| `src/cli/completions.rs` | shell completion scripts, and the hidden `--names` call that completes dataset names from data.toml | `print`, `script` |
| `src/cli/fetch.rs` | many downloads at once, with bars or plain lines; the first failure stops the rest and keeps what arrived | `all`, `Job` |
| `src/cli/fetch/data_folder.rs` | pinned files the data folder already holds, cloned or copied into the cache before a mirror or source is asked | `DataFolder` |
| `src/cli/fetch/mirrors.rs` | pinned files and archive trees asked of a run's mirrors in order, each checked against its pin | `Mirrored` |
| `src/cli/fetch/again.rs` | a pinned file whose link died, looked up again through its source, since an id or DOI outlives its links | `recover` |

## cli/sync

`cli/project.rs` decides which steps a command takes and `Loop` runs them. [sync loop](sync-loop.md) gives their order and shortcuts, [placing](placing.md) the placement decisions.

| file | owns | start at |
|---|---|---|
| `src/cli/sync.rs` | the loop: resolve what data.toml changed, learn every file's hash, fetch what the new lock names, write data.lock, then place | `Loop`, `Loop::relock` |
| `src/cli/sync/draft.rs` | a dataset on its way into data.lock, with hashes no source states filled in from earlier locks or the bytes | `Draft` |
| `src/cli/sync/rename.rs` | a dataset renamed in data.toml, which takes over the old name's pin without asking the source | `carry` |
| `src/cli/sync/disk.rs` | the survey of the data folder, read once per command, and the objects behind hardlinks someone edited | `Survey`, `audit` |
| `src/cli/sync/place.rs` | the action for each destination: place, replace, remove, adopt, or leave alone a file someone changed | `Placing`, `decide` |
| `src/cli/sync/place/put.rs` | many files placed at once, with how each looked right after | `put_all` |
| `src/cli/sync/place/tell.rs` | the lines placement prints: costs, removals, and each file left alone or put back, with its reason | `tell` |
| `src/cli/sync/unpack.rs` | archives unpacked a few at a time, checked against data.lock, then dropped from the cache unless still needed | `unpack_all`, `drop_archives` |

## project

data.toml holds what a person asked for, data.lock what it resolved to, and `.fetchloom/state` what was placed; [project](project.md) says who may change each.

| file | owns | start at |
|---|---|---|
| `src/project.rs` | the project folder: finding it, the lock a command holds while it changes the project, and atomic writes | `Project`, `Held` |
| `src/project/manifest.rs` | data.toml: every check of its keys, and edits that keep comments, order and spacing | `Manifest`, `Entry`, `Origin` |
| `src/project/lock.rs` | data.lock: pins, their checks, and the fixed-order writer that gives the same bytes for the same inputs | `Lock`, `Locked`, `LockedFile` |
| `src/project/lock/canonical.rs` | a fast reader for data.lock as fetchloom writes it; any other TOML goes to the general reader and its exact errors | `read` |
| `src/project/lock/archives.rs` | the archives among a dataset's pins, a split archive with its parts | `pinned` |
| `src/project/lock/derived.rs` | a recipe's result as data.lock pins it: inputs, command, code, variables, key and tree | `Derived` |
| `src/project/lock/lists.rs` | the compressed file lists in `data.lock.d`, for sources past 50,000 files and every recipe result | `Listed`, `read` |
| `src/project/lock/merge.rs` | data.toml and data.lock merged a dataset at a time after a git merge, with the choices a person made | `merge` |
| `src/project/lock/merge/markers.rs` | each side, and the base, read back from git's conflict lines | `split` |
| `src/project/state.rs` | `.fetchloom/state`: each placed file's size and time, the last sync's inputs, and rebuilds that differ from their pin | `State`, `Placed` |
| `src/project/git.rs` | git run as a bounded reader with no shell: each side of an unfinished merge, and the files git tracks | `stages`, `known` |

## recipe and build

a recipe makes a dataset by running a command over other datasets, and only `fetchloom build` runs one. [recipes](recipes.md) covers the key, stale pins and each step.

| file | owns | start at |
|---|---|---|
| `src/recipe.rs` | `run` templates, the order of steps, and the key over everything a result depends on, which tells a stale pin | `Template`, `key`, `drift` |
| `src/recipe/code.rs` | a recipe's `code` hashed alike on every system: CRLF read as LF, a folder as its tree, links refused | `hash` |
| `src/build.rs` | the folder in the cache a step runs in, reading `in/<name>` and writing `out` | `Staging` |
| `src/build/step.rs` | one step run without a shell, its output in a private log, its whole process group ended on a stop | `run` |
| `src/build/step/program.rs` | a step's program found on windows as a shell there finds it, so `npm` reaches `npm.cmd` | `find` |
| `src/build/inputs.rs` | each input's private copy, made from the cache as a read-only reflink or copy, never a hardlink | `make` |
| `src/build/capture.rs` | what a step wrote, taken into the cache without following a link | `read`, `publish` |
| `src/build/compare.rs` | a rerun's files compared with the result data.lock pins | `compare` |

## source

most adapters have a type with `parse` for an id, `from_link` for a pasted link and `resolve`, which returns its `Listing`. [sources](sources.md) shows how to add one.

| file | owns | start at |
|---|---|---|
| `src/source.rs` | references and listings: which source a reference names, the checks every listing passes, and the guard paged listings share | `Reference::parse`, `Reference::resolve`, `Listing` |
| `src/source/search.rs` | `search` across public catalogs, and the public dataset one typo from a hugging face or kaggle id | `search`, `near` |
| `src/source/local.rs` | text that names something on this machine, never searched for or sent; the ref of imported files | `is_path`, `IMPORTED` |
| `src/source/doi.rs` | DOIs, routed by the host doi.org names, never by their prefix | `Doi::locate` |
| `src/source/zenodo.rs` | zenodo records, with the MD5 zenodo states for every file | `Record` |
| `src/source/figshare.rs` | figshare articles, optionally at a version | `Article` |
| `src/source/dataverse.rs` | datasets on any dataverse install, by host and persistent id | `Dataset` |
| `src/source/dataverse/version.rs` | published versions and the original files their checksums describe | `Version` |
| `src/source/dataverse/file.rs` | one file named by its page link, at a published version | `resolve` |
| `src/source/dryad.rs` | dryad datasets at one version; anyone may list them, but downloads need an API account | `Dataset` |
| `src/source/mendeley.rs` | mendeley data through the public API its own site reads; no key goes to mendeley | `Dataset` |
| `src/source/osf.rs` | OSF projects and components by their short id | `Node` |
| `src/source/osf/walk.rs` | OSF storage walked folder by folder, with the versions that fix each file's bytes | `files` |
| `src/source/hf.rs` | hugging face dataset, model and space repositories, downloaded through `/resolve/<commit>/` | `Repo` |
| `src/source/hf/listing.rs` | a repository listed at one commit, its access checked when it is gated | `Repo::resolve` |
| `src/source/hf/revision.rs` | what a revision points at: its commit, whether the repository is gated, and its tags | `Repo::revision` |
| `src/source/hf/tree.rs` | the files at one commit, read from the tree API page by page | `Repo::tree` |
| `src/source/kaggle.rs` | kaggle datasets by owner and name, and competitions by name, downloaded one file at a time | `Dataset`, `parse` |
| `src/source/kaggle/listing.rs` | a dataset at one version, its files read page by page | `Dataset::resolve` |
| `src/source/kaggle/competition.rs` | competition files, for a signed-in account that accepted the rules | `Competition` |
| `src/source/kaggle/token.rs` | kaggle's credentials, where the kaggle CLI keeps them | `find` |
| `src/source/ena.rs` | ENA runs by accession, one kind of read file per reference, FASTQ unless it names another | `Accession` |
| `src/source/geo.rs` | GEO series, samples and platforms from NCBI's folder indexes, which state no hashes | `Accession` |
| `src/source/dandi.rs` | DANDI dandisets at a published version or the draft, with the SHA-256 DANDI states | `Dandiset` |
| `src/source/openneuro.rs` | OpenNeuro snapshots through its GraphQL API, large files from its S3 bucket | `Dataset` |
| `src/source/github.rs` | github release files at a tag or the newest, with the SHA-256 github states | `Release` |
| `src/source/gitlab.rs` | gitlab.com release links, each downloaded from its own address | `Release` |
| `src/source/drive.rs` | google drive links: files from drive's download host, items shared with an account through its api | `Item`, `fetch_plan` |
| `src/source/drive/link.rs` | an open link, found to be a file or a folder by drive's redirect | `Item::opened` |
| `src/source/drive/folder.rs` | shared folders, read from the page drive embeds them in, one request per subfolder | `list` |
| `src/source/drive/page.rs` | the pages drive sends instead of a file: the virus-scan confirmation and its refusals | `divert` |
| `src/source/drive/html.rs` | the little HTML reading drive's pages need | `between`, `text` |
| `src/source/drive/private.rs` | items shared with an account, read through drive's api once a page refused them | `refused`, `list` |
| `src/source/dropbox.rs` | dropbox shared links, fetched with `dl=1`; a shared folder arrives as one zip | `Link` |
| `src/source/bucket.rs` | S3, S3-compatible, google cloud storage and azure buckets and prefixes, unsigned until a listed bucket refuses | `Bucket`, `Prefix` |
| `src/source/bucket/s3.rs` | S3's `ListObjectsV2`, following a bucket to its region | `list` |
| `src/source/bucket/gcs.rs` | google cloud storage's JSON object list | `list` |
| `src/source/bucket/azure.rs` | azure's `List Blobs`, with a SAS only for a container `azure_containers` lists | `list` |
| `src/source/bucket/signing.rs` | which S3 requests go signed, with which keys and region | `Signing` |
| `src/source/bucket/xml.rs` | a small XML reader for S3 answers that refuses document type declarations | `parse` |

## auth

a key lives where the source's own tools keep it, so one key serves both. `net` decides which requests carry one, as [network](network.md#trust-boundaries) describes.

| file | owns | start at |
|---|---|---|
| `src/auth.rs` | accounts at the sources: which key each takes, where it is kept, how to check it and how to get one | `Account`, `credentials` |
| `src/auth/store.rs` | keys for sources whose tools keep no file: the system's credential store, or a private file the person agreed to | `read`, `save` |
| `src/auth/netrc.rs` | `.netrc`, which curl, wget and python's requests also read | `read`, `save` |
| `src/auth/aws.rs` | AWS keys from the environment or the chosen profile, sent to an S3-compatible endpoint only where the AWS settings say | `find` |
| `src/auth/aws/program.rs` | a profile's `credential_process` or the AWS CLI, run by full path without a shell | `Program` |
| `src/auth/azure.rs` | azure storage's SAS, account key or sign-in token, where the az cli reads them | `find` |
| `src/auth/google.rs` | google's application default credentials, where google's own libraries look | `find` |
| `src/auth/process.rs` | a program that prints a credential: no shell, stdout read up to 64 KiB, stderr never | `printed` |
| `src/auth/lease.rs` | a credential that expires, fetched again by one caller while the others wait | `Lease` |

## net

[network](network.md) explains the download pipeline, retries, pacing and which requests carry a credential.

| file | owns | start at |
|---|---|---|
| `src/net.rs` | the run's network: lazy clients, redirect policy, concurrency, and which credential a request carries | `Net`, `Options` |
| `src/net/api.rs` | small answers read whole, such as listings and metadata, bounded in size | `Api`, `Response` |
| `src/net/api/memo.rs` | metadata answers reused within a run, and one request shared by callers that ask at once | `Memo` |
| `src/net/cookies.rs` | the one cookie jar every client of a run sends from, its generation keeping memoized answers apart | `Cookies` |
| `src/net/download.rs` | one file into the cache: resumed, retried, every byte hashed on its way to disk and checked against what was expected | `download`, `Fetch` |
| `src/net/download/split.rs` | one large file over several ranged streams, a stream added only while it makes the whole faster | `split` |
| `src/net/headers.rs` | the headers downloads read: lengths, ranges, validators and `Content-Disposition` file names | `file_name`, `validators` |
| `src/net/failure.rs` | which failures get another try, and how long to wait | `classify`, `Failure` |
| `src/net/pace.rs` | how often a host lets fetchloom ask, from its rate-limit headers or a spacing such as NCBI's | `Paces` |
| `src/net/rate.rs` | the one byte budget every response in a run shares | `Rate` |
| `src/net/address.rs` | where a request may go: private addresses only for origins a person typed, named as a mirror or listed in `allowed_origins`; DNS; and proxies | `Reach`, `Proxies` |
| `src/net/address/reach.rs` | whether the proxy carrying a request can be reached, so a dead proxy is named within seconds | `Proxies::carrier` |
| `src/net/shown.rs` | links as messages and logs show them, every query value masked unless its name is known to be harmless | `Shown`, `mask_query` |
| `src/net/sign.rs` | AWS signature version 4, so the secret itself is never sent | `authorization` |
| `src/net/google.rs` | which requests carry a google token: listed buckets once refused, and the drive requests `drive` lets through | `Google` |
| `src/net/google/drive.rs` | which drive api requests carry a token: items `drive_folders` lists and what google lists inside them | `Drive` |
| `src/net/azure.rs` | which azure blob requests carry a SAS, a key's signature or a token, never after a redirect | `Azure` |
| `src/net/azure/shared_key.rs` | azure's shared key signature; the key is never sent | `authorization` |
| `src/net/azure/identity.rs` | the managed identity's token from 169.254.169.254, through a client of its own | `Net::identity_token` |
| `src/net/roots.rs` | the linux TLS roots: the system's, else the mozilla roots fetchloom carries | `tls` |

## mirror

a run asks its mirrors before any source and checks every file one gives against its pin; [cache](cache.md#copies-from-mirrors) covers the copies.

| file | owns | start at |
|---|---|---|
| `src/mirror.rs` | the layout folder and web mirrors share, and the mirrors a run asks; nothing a mirror gives is trusted | `Mirrors`, `Location` |
| `src/mirror/folder.rs` | a mirror on a disk this machine reads, its files copied and never linked, and `Target`, which writes one for `push`, checking each file | `probe`, `Target` |
| `src/mirror/web.rs` | a mirror on a web server: files through the download pipeline, marker and trees read whole up to 64 MiB | `probe`, `object` |

## cache

most files here add methods to `Cache`. [cache](cache.md) gives the layout and when an object may be trusted.

| file | owns | start at |
|---|---|---|
| `src/cache.rs` | one read-only file per object named by its SHA-256, plus URL and MD5 records and imported files | `Cache`, `Cache::find` |
| `src/cache/partial.rs` | unfinished downloads: the bytes so far, their record and a lock keeping each to one process; never opened through a link | `Cache::claim`, `PartialWriter` |
| `src/cache/partial/hashing.rs` | a bounded queue that lets disk writes go on while another thread hashes earlier bytes | `HashPipe` |
| `src/cache/partial/hashing/lanes.rs` | the optional MD5 lane, which shares a batch's bytes | `Lane` |
| `src/cache/deferred.rs` | finished downloads that take their names only after a flush covers them, one flush per group on linux | `Cache::defer_downloads`, `Finished` |
| `src/cache/object.rs` | objects written in one go, as archive members or mirror copies: sealed read-only in `tmp/`, hashed and flushed before publishing | `NewObject`, `Batch` |
| `src/cache/held.rs` | the cache-wide lock: runs hold it shared, `cache gc` exclusive | `Cache::hold` |
| `src/cache/staging.rs` | archive staging folders and recipe step folders, whose locks tell a live writer from a killed run | `Cache::staging`, `Staging` |
| `src/cache/unpacked.rs` | what each archive unpacked to, keyed by its SHA-256 or a split archive's parts | `Cache::record_unpacked`, `tree_hash` |
| `src/cache/results.rs` | recipe results under `builds/<key>/<tree>`, and their key locks | `Cache::record_result`, `Cache::hold_key` |
| `src/cache/changed.rs` | objects found changed, remembered after repair so projects can check their old hardlinks | `Cache::mark_changed`, `Doubt` |
| `src/cache/upkeep.rs` | the projects that use the cache, gc, verification, and the objects each project needs | `Cache::register`, `Needed` |
| `src/cache/usage.rs` | where the cache's space goes, for `fetchloom cache` and the weekly reminder alike | `Usage::measure` |
| `src/cache/failure.rs` | the filesystem failures inspection and cleanup keep to report | `Failure` |

## archive

[archives](archives.md) gives the checks every member passes before it is published.

| file | owns | start at |
|---|---|---|
| `src/archive.rs` | archives unpacked into the cache member by member, refused whole if anything is unsafe | `unpack`, `Limits` |
| `src/archive/detect.rs` | an archive's format, read from its first bytes | `detect` |
| `src/archive/input.rs` | an archive's bytes as readers take them: one file, or a split archive's parts in order | `Input` |
| `src/archive/parts.rs` | split archives found by their names; rar volumes and spanned zips stay packed | `split` |
| `src/archive/guard.rs` | one guard per archive, checking every name and counting every byte written against the limits | `Guard` |
| `src/archive/names.rs` | member names made portable and compared as disks compare them | `Names` |
| `src/archive/zip.rs` | zip through its central directory, every entry checked before the first byte is written | `unpack` |
| `src/archive/zip/directory.rs` | bounds on central-directory work before the decoder reads it | `check` |
| `src/archive/tar.rs` | tar, plain or compressed, read in one pass | `unpack` |
| `src/archive/sevenz.rs` | 7z a block at a time, with the memory the limits allow and no password | `unpack` |
| `src/archive/store.rs` | writer threads storing members while one thread reads; nothing published before the whole archive passed | `store_all`, `Writers` |
| `src/archive/store/stream.rs` | reusable buffers between a large member's decoder and its writer | `channel` |
| `src/archive/shards.rs` | numbered tar shards that webdataset loaders read packed, and the hint to keep them so | `shards` |

## plan, place and volume

`plan` decides where each pinned file goes and `place` does the filesystem work; [placing](placing.md) explains each action.

| file | owns | start at |
|---|---|---|
| `src/plan.rs` | where every pinned file lands, clashes found before anything is written, names compared as a disk that ignores case does | `Plan`, `Target` |
| `src/place.rs` | cached objects put in place by reflink, else hardlink, else copy, and copies made durable | `Placer`, `Method` |
| `src/place/parents.rs` | on unix, directory handles that keep later path swaps out of placement | `Parents` |
| `src/place/parents_windows.rs` | the windows twin: folder handles that deny deletion while placement uses their paths | `Parents` |
| `src/place/schedule.rs` | a cap on concurrent changes to one folder's entries, apart from the worker ceiling | `Folders` |
| `src/place/synced.rs` | folders onedrive, dropbox, icloud drive or google drive keep in sync, where files are copied rather than linked | `client` |
| `src/volume.rs` | what a volume allows: FAT32 and exFAT cannot hardlink, and on windows refuse to remove a read-only file | `Volume` |

## output

[output](output.md) covers which stream carries what, the palette and terminal fallbacks.

| file | owns | start at |
|---|---|---|
| `src/output.rs` | everything fetchloom prints: the answer on stdout, the story on stderr, and the JSON document | `Output` |
| `src/output/style.rs` | what the terminal can show, decided once at start: color, unicode, links, and `FETCHLOOM_COLORS` | `Style`, `Palette` |
| `src/output/progress.rs` | bars on a terminal a person watches, plain lines now and then anywhere else | `Bar`, `Ticker` |
| `src/output/picker.rs` | the arrow-key dataset choice, and its numbered form for accessible output | `choose` |
| `src/output/fit.rs` | lines wrapped to the terminal, and help examples continued as a shell reads them | `fit` |
| `src/output/warning.rs` | each warning's `kind`, which `--json` names so a script can act without reading words | `Warning` |

## primitives

every layer shares these small types and limits; [primitives](primitives.md) gives their rules.

| file | owns | start at |
|---|---|---|
| `src/digest.rs` | SHA-256, the identity of a file, and MD5, which is checked but never trusted as one | `Sha256`, `Hasher` |
| `src/name.rs` | names windows and linux can both store, dataset names, typo suggestions and case folding | `sanitize`, `fold` |
| `src/glob.rs` | `select` and `exclude` patterns, matched against a whole path inside a dataset | `Glob` |
| `src/units.rs` | one way to write sizes, rates, durations, counts, lists and paths everywhere | `size`, `duration` |
| `src/settings.rs` | every setting, read once at start with where it came from: variable, settings file, `resources` level, machine or default | `Settings` |
| `src/settings/resources.rs` | `resources` levels: what lean, default and max set, and what `-v` says a run spends | `Level`, `Spending` |
| `src/machine.rs` | processors, the memory a run may use inside cgroup limits, open-file limits, and programs on PATH | `Machine` |
| `src/space.rs` | free space before a download starts, and the largest file a disk holds, 4 GiB less one byte on FAT32 | `free`, `Needs` |
| `src/threads.rs` | the optional ceiling on local worker groups, and the scoped worker helper | `Threads`, `scoped` |
| `src/error.rs` | failures with stable kinds, exit codes and advice, for people and scripts | `Error`, `Kind` |
| `src/stop.rs` | ctrl-c: the first asks every task to wind down and keep what it has | `Stop` |
| `src/suggested.rs` | the commands hints name, carrying the folder a `-C` run worked in | `command` |
| `src/work.rs` | counts of the work a run did, which tests assert, `-v` prints and `--json` carries | `Work`, `Counts` |

## tests

the cli tests run the real binary in a sandbox; [building and testing](../engineering/building-and-testing.md) says which to run.

| file | owns | start at |
|---|---|---|
| `tests/cli/main.rs` | the cli test binary: one module per scenario file in `tests/cli/` | |
| `tests/cli/support.rs` | the sandbox every cli test runs in: a clean folder, a private cache and home, and a server that answers with scripted bytes | `Sandbox`, `Server` |
| `tests/cli/support/command.rs` | a command with a deadline, whose failure shows its stderr and the end of its debug log | `Fetchloom` |
| `tests/cli/support/running.rs` | a command running beside its test, its stderr read as it comes | `Running` |
| `tests/cli/support/server.rs` | listeners and connections, stopped and joined with their test server | `ServerTask` |
| `tests/cli/support/programs.rs` | the fetchloom binary and the `recipe_step` example the tests run | `executable`, `recipe_step` |
| `tests/support/paths.rs` | the workspace folder, shared by unit and cli tests | `workspace` |
| `tests/support/ports.rs` | an address where every connection is refused, shared by unit and cli tests | `Refusing` |
| `tests/support/skips.rs` | a skip that fails instead where `FETCHLOOM_TEST_REQUIRE` names the missing capability | `skip` |
| `examples/recipe_step.rs` | the step program cli tests put in recipes | `main` |
| `src/net/scripted.rs` | a local server that answers each connection with scripted bytes, for network unit tests | `serve` |
| `src/source/recorded.rs` | answers recorded from real services, replayed in source unit tests | `Recorded` |
| `src/archive/samples.rs` | archives built at test time, and a scratch cache to unpack them into | `Scratch` |

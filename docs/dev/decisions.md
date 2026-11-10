# decisions

these choices keep byte identity, recoverable storage and the command interface consistent across platforms.

## purpose

research code can outlive the dataset version it used. keep input identities beside that code so collaborators can retrieve the same published bytes without repeating browser downloads and folder renames. a lock cannot restore bytes that every host and cache has lost; long-term preservation still requires someone to retain the data.

## implementation

| what | why | implementation |
|---|---|---|
| keep one rust crate | modules provide boundaries without separate package versions; split only when an independent consumer needs one | [architecture](architecture/README.md) |
| ship one executable named `fetchloom` | powershell uses `fl` for `Format-List`; the full executable name avoids that collision, and users can choose their own shell alias | [CLI](architecture/cli.md) |
| keep adapters in the crate | version, checksum and paging rules stay beside their tests; there is no current plugin-protocol consumer | [sources](architecture/sources.md) |
| route DOIs through the registered handle URL | DOI prefixes and landing-page redirects do not reliably identify the repository | [source routing](architecture/sources.md#parse-before-resolving) |
| support shared-link sources alongside repositories | research inputs also arrive through hugging face, kaggle, drive and dropbox | [source behavior](../user/sources/README.md) |
| use reqwest, tokio and rustls | reuse HTTP/2, system proxies and TLS verification while retaining explicit redirect checks | [network](architecture/network.md) |
| keep disk writes and hashing off async workers | overlap byte processing without blocking request scheduling; bound queues to control retained work | [download pipeline](architecture/network.md#download-pipeline) |

## project inputs

| what | why | contract |
|---|---|---|
| identify files by SHA-256 and size | different URLs can share an object; moving a URL does not change byte identity | [cache](architecture/cache.md) |
| learn missing hashes from bytes | a source without a usable checksum still needs a checked pin | [sync loop](architecture/sync-loop.md#resolve-only-changed-specifications) |
| store archive member lists outside the lock | the packed-file entry and tree hash keep lock size independent of member count | [data.lock](../user/reference/data-lock.md) |
| select extraction by recognized archive names, then inspect bytes | ordinary research files stored in zip containers must stay whole | [archives](architecture/archives.md#names-choose-candidates-bytes-choose-decoders) |
| reuse resolved references for filter or extraction edits when `ref` is unchanged | retain explicit versions while selection changes; a changed reference or upgrade resolves the requested reference again | [sync loop](architecture/sync-loop.md#resolve-only-changed-specifications) |
| cap listings before selection | filtering must not hide unbounded source-list work; this is not a total-project size cap | [sources](architecture/sources.md#keep-listing-and-transfer-separate) |
| treat local state as observations, not byte authority | missing state can recover by hashing files; a broken pin cannot safely be guessed | [project](architecture/project.md#three-records-with-different-authority) |
| stop project discovery at the top of a git repository | data.toml belongs to the repository that commits it; a manifest above would put pins in another history, and a stray one in a home folder would capture every repository below. `-C` names such a project on purpose | [finding the project](../user/reference/project-folder.md#finding-the-project) |
| let status settle a project as sync does | after a move and `cache gc --now`, status listed every archive member as extra while sync, rightly, found nothing to do; one rule for both keeps status saying what sync would do | [settled-sync shortcut](architecture/sync-loop.md#settled-sync-shortcut) |

mutable URLs and OSF nodes cannot preserve an old listing through a filter edit. review lock diffs because new selections can include newer files; recorded hashes still pin the selected bytes.

## storage and requests

| what | why | invariant |
|---|---|---|
| try reflink, hardlink, then copy; read-only windows placement tries hardlinks first | reflinks isolate later writes; hardlinks share bytes and can reuse a windows scan; copies work when links fail. symlinks can dangle after cache cleanup and need extra windows privileges | [placing](architecture/placing.md#choose-a-filesystem-method) |
| separate object publication from partial ownership | hardlink publication avoids serializing publishers on one exclusive cache lock; unfinished bytes need an ownership lock, and the shared gc lock protects active runs | [cache publication](architecture/cache.md#publication-and-repair) |
| open cache hashing and copy inputs without following final links | a replaced object name must not redirect these readers to another file | [cache access](architecture/cache.md#safe-object-access) |
| date each object by its birth when sealing it | an edit through a placed hardlink changes the cache's copy behind fetchloom's back; a modification time moved past the birth time puts the object in doubt with one stat and no per-object record, as pnpm's recorded check time does for its store. the date lands just past the file's making, which an nfs 4.2 server with delegated timestamps keeps | [cache access](architecture/cache.md#safe-object-access) |
| keep a link's changed copy beside the pinned file sync puts back | an edit made in place through a writable hardlink changes the cache's copy every project shares, so sync must put the pinned bytes back; scripts such as pandas' `to_csv` write in place, so dropping the changed copy lost real work. a second name for the same file costs no bytes, and a name already taken is refused, never replaced | [local changes](../user/commands/sync.md#local-changes) |
| flush a run of up to 64 files one by one, and a larger one with one `syncfs` | `syncfs` also waits for everything other programs wrote to the filesystem, which took seconds on a busy disk; one by one cost an idle disk under 10 ms more for 64 files, and one large file the same either way | [batch durability](architecture/network.md#batch-durability) |
| flush copied payloads before sync writes state | state must not vouch for bytes still pending write-back; linux batching avoids one flush per small copy | [copy durability](architecture/placing.md#copy-durability) |
| leave checkpoint and lookup indexes as unflushed hints | losing a hint costs resumption or revalidation, while payload identity remains checked | [cache checkpoints](architecture/cache.md#partial-ownership-and-checkpoints) |
| give the other catalogs 10 s once one search catalog has answered | one failing catalog held the dataset picker for 76 s while the others had answered; a slow network still gets unlimited time for the first answer | [search](../user/commands/search.md) |
| limit silence instead of total download time | a large transfer that keeps producing bytes can finish | [request policy](architecture/network.md#request-policy) |
| begin adaptive downloads with one stream and split after it proves slow | extra connections slowed fast transfers and helped constrained ones | [network measurements](performance/measurements.md#network) |
| fetch file bodies over several connections per host | one connection queued every body behind the slowest on lossy links: far s2 took 0.288 and poor s2 0.424 of the time; lan and home stayed flat | [measurements](performance/measurements.md#ab-passes-on-the-linux-lab-2026-11) |
| write no closing resume record for a small download | a download of 1 MiB or less from its first byte finishes before its record would help, and writing it cost more: 10,000 small files took 0.757 to 0.840 of the time; it still takes its link's lock and checkpoints while slow | [measurements](performance/measurements.md#ab-passes-on-the-linux-lab-2026-11) |
| spread unfinished downloads and archive staging over 16 folders | files made at once in one folder wait on its lock: s3 took 0.902, s5 0.712 and zips 0.798 to 0.830 of the time | [measurements](performance/measurements.md#ab-passes-on-the-linux-lab-2026-11) |
| use mimalloc for every allocation in the static linux build, libc's own too | musl's allocator is slow under many threads, and it mapped and unmapped a page group for each folder a scan read: a no-op sync took 0.83 of the time once libc's allocations moved | [measurements](performance/measurements.md#ab-passes-on-placement-and-startup-2026-11) |
| build liblzma from its bundled source on every platform | its build script otherwise links any liblzma 5.8 or later that pkg-config finds, which on a mac is often homebrew's, missing from a user's mac; the bundled copy, 5.8.4, is what the linux and windows releases already carried | [releasing](engineering/releasing.md) |
| leave transparent huge pages to the system and the allocator | turning them off cut peak memory to 0.33 to 0.73 of what it was, but 4 KiB pages fault far more often: a no-op sync took 1.09 times as long and placing 100,000 files from the cache 1.13 times | [measurements](performance/measurements.md#ab-passes-on-placement-and-startup-2026-11) |
| put all archive decoders behind one guard | entry types, paths and expansion limits must agree before members become reusable | [archives](architecture/archives.md#validate-before-publication) |
| read 7z with sevenz-rust2's decoders, leaving ppmd out | it bounds header memory, checks crcs and shares the bzip2, zstd and flate2 crates fetchloom already uses. its `deflate` feature turns on flate2's default features: miniz_oxide, adler2 and simd-adler32 compile though zlib-rs stays the backend, and zlib-rs may pick avx2 code at run time. ppmd-rust is licensed `CC0-1.0 OR MIT-0`, outside deny.toml, so a 7z of ppmd data is refused; allowing `MIT-0` is the owner's call once a dataset needs it | [archives](architecture/archives.md#names-choose-candidates-bytes-choose-decoders) |
| preserve edits and recheck before replacement | files can change while downloads run; interruption recovery must not erase unrelated work | [placing](architecture/placing.md#choose-an-action) |

## interface and maintenance

| what | why | contract |
|---|---|---|
| use flags, environment variables and one settings file | a shared machine or a big scratch disk needs settings that last without exported variables. one order for every value, variable then file then machine then default, with `fetchloom settings` showing where each came from, keeps a second source from becoming a precedence puzzle | [settings file](../user/reference/environment.md#settings-file) |
| require an explicit dataset choice | public catalogs can return several plausible matches; `--yes` only approves downloads | [search selection](../user/commands/search.md#choosing-a-dataset) |
| give errors stable kinds | scripts can act without parsing prose | [exit codes](../user/reference/exit-codes.md) |
| keep terminal formatting in one renderer | commands share stream routing, prompts, aligned verbs and progress behavior | [output](architecture/output.md) |
| test behavior and work counts without coverage quotas | extra requests, downloads and copies can be detected independently of runner speed | [engineering](engineering/README.md), [performance](performance/README.md) |
| fuzz parsers in their modules | parsers can remain private without a separate fuzzing API | [fuzzing](engineering/fuzzing.md) |
| ship the same executable in wheels | published binary wheels will let pip users receive the same implementation without rust; current source installation still needs it | [releasing](engineering/releasing.md) |
| test and release native arm64 linux and windows | those artifacts need the same behavioral gate as x86_64 | [CI](engineering/ci.md), [release targets](engineering/releasing.md#workflow) |
| test macos on apple silicon and release it for x86_64 and arm64 | apfs clones files, ignores case and keeps unicode composition, so placement and status need their own gate; the intel build is tested only by its release smoke checks | [CI](engineering/ci.md), [release targets](engineering/releasing.md#workflow) |
| keep reasons near the implementation and update docs with behavior | contributors can check the constraint before changing it | [style](style.md) |

## apache-2.0

| what | why | contract |
|---|---|---|
| use Apache-2.0 | permit use, modification and redistribution with explicit patent terms | [LICENSE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/LICENSE) |
| distribute NOTICE | preserve attribution and the project-name notice; the license grants no trademark rights beyond describing origin | [NOTICE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/NOTICE) |

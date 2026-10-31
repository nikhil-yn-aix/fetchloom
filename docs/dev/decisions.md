# decisions

these choices keep byte identity, recoverable storage and the command interface consistent across platforms.

## purpose

research code can outlive the dataset version it used. keep input identities beside that code so collaborators can retrieve the same published bytes without repeating browser downloads and folder renames. a lock cannot restore bytes that every host and cache has lost; long-term preservation still requires someone to retain the data.

## implementation

| what | why | implementation |
|---|---|---|
| keep one rust crate | modules provide boundaries without separate package versions; split only when an independent consumer needs one | [architecture](architecture/README.md) |
| ship one executable named `fetchloom` | powershell uses `fl` for `Format-List`, and a managed machine's ransomware rule blocked an unsigned `fl.exe`; users can choose their own shell alias | [CLI](architecture/cli.md) |
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

mutable URLs and OSF nodes cannot preserve an old listing through a filter edit. review lock diffs because new selections can include newer files; recorded hashes still pin the selected bytes.

## storage and requests

| what | why | invariant |
|---|---|---|
| try reflink, hardlink, then copy | reflinks isolate later writes; hardlinks share bytes; copies work when links fail. symlinks can dangle after cache cleanup and need extra windows privileges | [placing](architecture/placing.md#choose-a-filesystem-method) |
| separate object publication from partial ownership | hardlink publication avoids a global cache lock; unfinished bytes need an ownership lock | [cache publication](architecture/cache.md#publication-and-repair) |
| open cache hashing and copy inputs without following final links | a replaced object name must not redirect these readers to another file | [cache access](architecture/cache.md#safe-object-access) |
| flush copied payloads before sync writes state | state must not vouch for bytes still pending write-back; linux batching avoids one flush per small copy | [copy durability](architecture/placing.md#copy-durability) |
| leave checkpoint and lookup indexes as unflushed hints | losing a hint costs resumption or revalidation, while payload identity remains checked | [cache checkpoints](architecture/cache.md#partial-ownership-and-checkpoints) |
| limit silence instead of total download time | a large transfer that keeps producing bytes can finish | [request policy](architecture/network.md#request-policy) |
| begin with one stream and split only after it proves slow | extra connections slowed fast transfers and helped constrained ones | [network measurements](performance/measurements.md#network) |
| put all archive decoders behind one guard | entry types, paths and expansion limits must agree before members become reusable | [archives](architecture/archives.md#validate-before-publication) |
| preserve edits and recheck before replacement | files can change while downloads run; interruption recovery must not erase unrelated work | [placing](architecture/placing.md#choose-an-action) |

## interface and maintenance

| what | why | contract |
|---|---|---|
| use flags and environment variables | avoid a separate config file and another precedence system | [environment](../user/reference/environment.md) |
| require an explicit dataset choice | public catalogs can return several plausible matches; `--yes` only approves downloads | [search selection](../user/commands/search.md#choosing-a-dataset) |
| give errors stable kinds | scripts can act without parsing prose | [exit codes](../user/reference/exit-codes.md) |
| keep terminal formatting in one renderer | commands share stream routing, prompts, aligned verbs and progress behavior | [output](architecture/output.md) |
| test behavior and work counts without coverage quotas | extra requests, downloads and copies can be detected independently of runner speed | [engineering](engineering/README.md), [performance](performance/README.md) |
| fuzz parsers in their modules | parsers can remain private without a separate fuzzing API | [fuzzing](engineering/fuzzing.md) |
| ship the same executable in wheels | published binary wheels will let pip users receive the same implementation without rust; current source installation still needs it | [releasing](engineering/releasing.md) |
| test and release native arm64 linux and windows | those artifacts need the same behavioral gate as x86_64 | [CI](engineering/ci.md), [release targets](engineering/releasing.md#workflow) |
| keep reasons near the implementation and update docs with behavior | contributors can check the constraint before changing it | [style](style.md) |

## apache-2.0

| what | why | contract |
|---|---|---|
| use Apache-2.0 | permit use, modification and redistribution with explicit patent terms | [LICENSE](../../LICENSE) |
| distribute NOTICE | preserve attribution and the project-name notice; the license grants no trademark rights beyond describing origin | [NOTICE](../../NOTICE) |

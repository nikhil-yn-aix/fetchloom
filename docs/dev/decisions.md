# decisions

## implementation

| decision | reason | implementation |
|---|---|---|
| keep one rust crate | installed binaries need no language environment; modules provide boundaries without separate package versions. split a crate when an independent consumer needs it | [code map](architecture.md#code-map) |
| keep `fetchloom` and `fl` on one entry point | two executable names do not need two CLIs to maintain | [entry points](architecture.md) |
| keep source adapters in the crate | version, checksum and paging rules stay beside their tests; a plugin protocol has no current consumer | [source adapters](architecture.md#source-adapters) |
| route DOIs through the handle URL | a registered destination avoids guessing from DOI prefixes or landing-page redirects | [DOI resolution](architecture.md#source-adapters) |
| include shared-link sources | the first users fetch from hugging face, kaggle, drive and dropbox as well as repositories | [source scope](../scope.md#sources) |
| use reqwest, tokio and rustls | the client supplies HTTP/2 and system proxies; fetchloom retains redirect handling to check each hop before sending credentials or reaching private addresses | [net](architecture.md#net) |
| keep byte processing off async workers | bounded writer queues overlap network, decompression and disk work | [concurrency](architecture.md#concurrency) |

## project inputs

| decision | reason | contract |
|---|---|---|
| use SHA-256 as file identity | different URLs can share an object, and moving a URL need not change its pin | [cache](architecture.md#cache) |
| learn missing hashes from bytes | a source without a usable hash still needs a checked identity before its files can be pinned | [relocking](architecture.md#clisync) |
| keep archive member lists outside the lock | a source-file entry and tree hash keep lock size independent of the number of archive members | [lock format](../formats.md#datalock) |
| unpack recognized archive names by default | research code usually needs the members; name-based selection leaves zip-based data formats such as `.npz` intact | [archives](../cli.md#archives) |
| reuse resolved references for filter or extraction edits when `ref` is unchanged | explicit versions can stay fixed while selection changes; changing `ref` or using `--upgrade` resolves the manifest reference instead | [source records](../formats.md#what-each-source-records) |
| cap listings before filtering | limit accepted source-list work even when selection would keep few files; the cap does not bound total project or lock size | [listing limits](architecture.md#source-adapters) |

mutable URLs and OSF nodes cannot preserve a past listing through a filter edit. review the lock diff: new selections may include newer files, and only recorded hashes pin bytes.

## storage and requests

| decision | reason | invariant |
|---|---|---|
| try reflink, hardlink, then copy | reflinks share blocks with independent writes; hardlinks share the file; copies work when links fail. symlinks can dangle after cache deletion and need extra windows permissions | [placement](architecture.md#place) |
| separate object publication from partial ownership | hardlink publication avoids a global cache lock; unfinished bytes need an OS lock to keep writers apart | [publication and its fallback limits](architecture.md#cache) |
| limit silence instead of total download time | a large transfer that keeps producing bytes can finish without a whole-download deadline | [network limits](architecture.md#net) |
| put archive readers behind one guard | each decoder must obey the same name, entry-type and expanded-byte checks before its members become visible | [archive guard](architecture.md#archive) |

## interface and maintenance

| decision | reason | contract |
|---|---|---|
| use flags and environment variables | avoid a config file and another precedence system | [settings](../formats.md#settings) |
| give errors stable kinds | scripts can act without parsing prose | [error kinds](../agents.md#error-kinds-and-exit-codes) |
| keep terminal output in one renderer | command handlers share rendering and report policy | [output](architecture.md#output) |
| test behavior and work counters without coverage quotas | request, byte and placement counts catch repeated work independently of runner speed | [checks](engineering.md#checks), [regression checks](performance.md#regression-checks) |
| fuzz parsers inside their modules | parsers stay private without a separate public fuzzing API | [fuzzing](engineering.md#fuzzing) |
| package the same executables in wheels | pip users need no rust toolchain and receive the same implementation as a source install | [releases](engineering.md#releases) |
| keep design reasons with code | contributors can review a choice alongside its implementation; update documentation with format and behavior changes | [style](style.md) |

## apache-2.0

[LICENSE](../../LICENSE) grants use, modification and redistribution under apache-2.0, including its patent terms. [NOTICE](../../NOTICE) preserves the attribution and project-name notice. the license grants no trademark rights beyond describing origin.

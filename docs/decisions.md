# decisions

## 1. rust, with two entry points

`fetchloom` and `fl` call one rust implementation. shell users need no language environment once it is built. python packaging remains separate release work; it is not a runtime dependency.

## 2. keep the project loop small

add, remove, sync, lock, status, path, get and cache cover fetching published inputs. preprocessing, publishing and workflow execution stay outside the command set. [scope](scope.md) sets those boundaries.

## 3. SHA-256 is a file's identity

objects use the hash of their bytes as their name. two source URLs can therefore share one object, and a moved URL need not change a pin. source MD5 values get a lookup index; they do not replace SHA-256 identities. hash measurements belong in [performance](performance.md).

## 4. learn missing hashes from bytes

when a source supplies no usable SHA-256 and size, locking learns them from an earlier pin, a checked MD5 mapping or a download. new downloads check stated MD5 too. later project downloads must match the pin. unpinned `get` revalidates URLs and may accept changed bytes.

## 5. record source files, not every archive member

`data.lock` records the selected source files, even when a stated hash lets `lock` avoid downloading one. an unpacked archive gets one file entry and one tree hash. member lists stay in the cache, keeping the lock independent of the number of files inside an archive.

## 6. unpack recognized archive names by default

research code usually needs the members. `extract = false` and `--no-extract` preserve the packed file when it is the input instead. filename-based selection keeps zip-based data formats such as `.npz` intact. packed objects can leave the cache after unpacking, unless a registered project still needs them packed.

## 7. keep source adapters in the crate

source APIs have different version, checksum and paging rules. a closed enum and rust modules keep those rules beside their tests. a plugin interface would add a protocol without a current consumer.

## 8. route DOIs through their handle URL

fetchloom reads the URL value from doi.org's handles API, then parses that URL as a source reference. it does not infer a repository from a DOI prefix or follow a landing-page redirect to identify it. unknown destinations fail rather than being downloaded as datasets.

## 9. finish the shared-link sources before release

hugging face, kaggle, drive and dropbox remain in the release scope because the first users fetch from those hosts as well as repositories. that is scope, not a completion claim: [plan](plan.md#milestones) records what each adapter does today.

## 10. keep `fetchloom` and `fl`

both executable names use the same entry point, avoiding a second CLI to maintain. powershell already uses `fl` for `Format-List`; use `fetchloom` there.

## 11. reflink, else hardlink, else copy

reflinks share blocks while keeping independent files. hardlinks reuse the cached file itself. copying works when neither is available. symlinks would dangle after cache deletion and can require extra permissions on windows.

hardlinks are read-only. recognized synced folders and writable placements skip hardlinks, but still try reflinks. a changed-hardlink audit covers only the project being synced, so a writable shared cache still requires trust. [placement](architecture.md#placing-files) states the limits.

## 12. separate publication from partial ownership

hardlink publication lets competing writers publish a completed object without a global cache lock. unfinished downloads need an OS lock because their bytes are still changing. when locking is unavailable, private partials keep writers apart but lose cross-run resume.

the object-publication rename fallback still has a check/rename race. it must not be described as no-clobber on every filesystem.

## 13. keep one crate

modules hold the current boundaries without separate package versions. the only public library entry point runs the CLI. a second independent consumer would be a reason to split a crate.

## 14. reqwest, tokio and rustls

the client provides HTTP/2 and system proxy support. rustls uses the platform verifier, and ring is selected as its crypto provider. redirects are handled by fetchloom so each hop can be checked before sending credentials or reaching a private address.

## 15. keep byte processing off async workers

downloads hand bounded batches of chunks to blocking writers. archive readers use worker threads to write and seal members. this overlaps network, decompression and disk work. placement and metadata operations are still synchronous; there is no blanket rule that every disk call runs off the runtime.

## 16. limit silence, not download duration

a connection has 45 seconds to connect, each request hop has 75 seconds to produce headers, and a body may be silent for 30 seconds between chunks. there is no whole-download deadline. [network limits](architecture.md#the-network) define retries and redirects separately.

## 17. use flags and environment variables

there is no config file or config-file precedence to maintain. flags cover invocation choices; environment variables cover the cache, concurrency and logging. the exceptions to simple flag precedence are explicit in [settings](formats.md#settings).

## 18. give errors stable kinds

kinds such as `network.unreachable` and `integrity.mismatch` let scripts act without parsing prose. a small set of exit codes groups them. the error carries a message, detail lines and optional help; not every failure has a next-step command.

## 19. keep terminal output in one renderer

clap parses arguments. anstream and anstyle handle color, indicatif and console draw progress, and dialoguer asks yes/no questions. no text-entry prompt is implemented. normal output uses human steps; JSON combines a final report with collected warnings and errors.

## 20. put archive readers behind a shared guard

zip 9.1.3, tar, flate2 with zlib-rs, liblzma, zstd and bzip2 supply readers. fetchloom checks names, entry types and actual expanded bytes. members stay staged until the archive passes. 7z and rar decoders are not included; those files stay packed with a warning.

## 21. test behavior without coverage quotas

unit tests, CLI scenarios, snapshots and work counters check observable behavior. no coverage or mutation gate is configured. mutation and fuzz tooling remain work to add, rather than tests already running before releases.

## 22. assert work counters

CLI tests assert requests, bytes and placements for selected scenarios. these catch repeat work without relying on a runner's speed. there is no instruction-count harness or CI performance gate. wall-clock measurements belong in [performance](performance.md), with the machine and workload named.

## 23. keep distribution work explicit

source installation builds both executables with cargo. dist for standalone binaries and maturin for wheels are proposed release tools, not configured pipelines. installers, wheel tests, publishing and attestations remain [release work](plan.md#next).

## 24. no telemetry

there is no telemetry client or background service. network operations resolve references and fetch the selected data. no account with fetchloom is needed.

## 25. keep design with code

format and behavior changes need matching documentation in the same change. keeping the reasons in the repository lets a contributor review a choice with its implementation.

## 26. apache-2.0

[LICENSE](../LICENSE) grants use, modification and redistribution under apache-2.0, including its patent terms. [NOTICE](../NOTICE) names Nikhil Y N and KairosLab India Private Limited and preserves the notice about the project names. the license grants no trademark rights beyond describing origin.

## 27. reuse resolved references for filter edits

changing filters or extraction settings resolves the existing `resolved` reference when `ref` is unchanged. explicit versions can therefore stay fixed while the selection changes. changing `ref` or using `--upgrade` instead resolves the manifest reference.

this cannot preserve a past listing for a mutable URL or OSF node. new selections may include newer files. review the lock diff; only its recorded file hashes pin bytes.

## 28. cap listings per dataset

the accepted listing limit is 100,000 files; OSF includes folders in that count. filters run afterward. archives have a separate 1,000,000-entry limit. these limits bound individual accepted lists, not the total project size or serialized lock size.

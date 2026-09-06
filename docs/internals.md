# Internals

How it works, and why it is built this way. For what it promises, see [contracts.md](contracts.md).

## The problem

Most public datasets publish no checksums. The reference truth is usually your own first successful fetch. Fetchloom captures that moment in a lock, and every later run is checked against it. That is what Cargo.lock does for code, applied to data.

Pinning is the product. Verification is the mechanism.

## Three ideas

**A name for bytes that cannot lie.** Every object is hashed and the hash is its name. One byte differs, the name differs. Whether you got the right thing stops being trust and becomes arithmetic.

**Never claim something worked when it did not.** Anything lower than what was asked for emits a `degrade` event naming what was wanted, what happened instead, and why. There is no path where the worse thing happens quietly.

**Nothing appears half finished.** Work happens in staging, is checked, then becomes visible in one atomic rename. Kill it mid run and you have the old tree or the new one.

## Layers

**Identity.** BLAKE3 for the cache key and the resume authority, SHA-256 because that is what publishers state. Both computed in one pass over the bytes. BLAKE3 also gives a chunk tree, which is what makes damage locatable instead of just detectable.

**Store.** Content addressed objects. Small ones go into packs rather than their own file, because the cost of many small objects is dominated by file count, not bytes. One lookup answers where anything is, and packs are self describing so no index can disagree with them.

**Source.** Metadata probe, validators, ranges, authentication, retry guidance, immutable identity. Everything network specific lives behind this, which is why adding a protocol does not touch the engine.

**Archive.** Enumeration, selection, bounded extraction. This is the hostile input surface, so it is where most of the rejections live.

**Policy.** Trust, offline, limits, credentials, terms. Every decision that could weaken a guarantee goes through here rather than being made where it is convenient.

**Observer.** Events, errors, progress, redaction. Redaction happens at construction, not at output, so a secret cannot reach a stream by way of a call site nobody remembered.

Six seams, fixed early. Widening one takes the same justification as changing a contract.

## Why blocking, not async

There is no tokio here, no `async fn`, no `.await`. Blocking threads with a budget measured from affinity, container and job limits.

That is a decision, not an accident. Tokio's file I/O is not truly asynchronous: it dispatches to a blocking thread pool, and measurements put native tokio around 44x slower than the best implementations for small random reads. For disk work, threads are the right answer.

Async would only help network concurrency, and only well above the number of connections a polite dataset fetcher opens. The cost would be roughly doubling the dependency graph.

## Why not HTTP/2

Measured and declined. HTTP/2 wins by multiplexing many requests over one connection. This workload is a few large transfers, where several HTTP/1.1 connections perform the same. The client we use states its goal as a sync HTTP/1.1 client with minimum dependencies and has had an HTTP/2 issue open since 2020, so adopting it means moving to hyper plus tokio.

HTTP/3 was measured too. The Rust implementation is still described as experimental.

Revisit when the workload changes, not before.

## Why FTP was written rather than depended on

FTP has not changed since RFC 3659 in 2007, so a dependency on it buys future maintenance of a protocol with no future. The crate that would have been taken brings `chrono` and `regex` transitively, and both exist in it only to read a date and a size out of a `LIST` line, which is an `ls` line meant for a person. `MLSD` states the same as fields, and `FEAT` was checked against every host this is meant to reach before the choice was made: NCBI and UCSC offer it, EBI and Ensembl do not, so the `LIST` fallback is real rather than theoretical and is a `degrade` rather than a silent equivalence.

What is here is one control connection, one reply parser, `PASV`, and two listing readers, in one file. The TLS is the rustls already in the graph, so FTPS added no crate.

## Why no chunk deduplication

Content defined chunking looked obviously worth it and measurement said no.

Across seventeen distinct datasets, chunk level deduplication saved **4.3 to 5.4 percent**. The headline number of 36 percent came entirely from holding two consecutive releases of the same dataset at once, which is not something people do.

The decisive measurement was the same two kernel releases, chunked twice:

| Form | Saved |
|---|---|
| Uncompressed tar | 35.87 percent |
| The same pair as `.tar.gz` | 0.00 percent |

Gzip destroys chunk similarity completely, and `.tar.gz` is what publishers actually serve. Audio saved 0.00 percent at every chunk size.

There was also a security result. An unkeyed chunk index in a cache shared between users leaks the shape of one user's content to another, and keying it drops cross user deduplication to zero. So the feature is worthless in the form that would be safe.

Whole file deduplication already works and costs nothing.

## Why the archive is kept

After extracting a 10 GB archive to 30 GB, the cache keeps the archive. Measured, that costs 16.1 percent more disk than storing the extracted members instead.

It is kept for three reasons. The publisher's stated digest is over the archive bytes, and it is the only independent evidence in the system: throw the archive away and that proof is gone permanently. Localized repair depends on the archive and its chunk tree, and you cannot seek into a gzip stream to recover one member. And the cross dataset deduplication that the members layout would buy is the 5.4 percent above.

## Performance

Fetchloom is slower than `cp` and `curl` on every workload. It hashes every byte twice, builds a chunk tree, publishes through staging, and writes a record. Those tools do none of that.

Measured on one machine, `x86_64-pc-windows-msvc`. Wall time is a property of the machine as much as the code, so it is published and never gated. The deterministic counters are what gate, at five percent.

| Regime | Fetchloom | Alternative | Ratio |
|---|---|---|---|
| cold cache, 64 files | 1463 ms | Copy-Item 322 ms | 4.5x |
| warm cache, 64 files | 478 ms | Copy-Item 322 ms | 1.5x |
| cold transfer | 123 ms | curl 23 ms | 5.4x |
| interrupted transfer | 190 ms | curl 31 ms | 6.1x |
| 1024 small files | 7150 ms | Copy-Item 1049 ms | 6.8x |
| one 256 MiB file | 759 ms | Copy-Item 347 ms | 2.2x |
| many hosts | 9672 ms | curl 2114 ms | 4.6x |

Peak memory is 8.4 MB on every regime including the 256 MiB single file one. Nothing is ever loaded whole. Binary is 7.9 MB, startup 8 ms.

The many hosts ratio is mostly not transfer cost. That regime injects 100 ms of latency into 40 requests and rate limits one host. Most of the time measured is backoff this run waits out one request at a time, because a host asking to be left alone drives its concurrency back to one. The alternative waits out none of it.

### Hashing

Measured on this machine, MiB/s, medians of nine runs.

| Input | BLAKE3 serial | BLAKE3 parallel | SHA-256 | Both, sequential | Both, two threads |
|---|---|---|---|---|---|
| 64 KiB | 2920 | 491 | 1437 | 356 | 120 |
| 256 KiB | 3242 | 719 | 1188 | 902 | 501 |
| 1 MiB | 3007 | 6321 | 1440 | 1081 | 868 |
| 4 MiB | 3277 | 10557 | 1393 | 1105 | 421 |

Three things follow. Parallel BLAKE3 is slower than serial below 1 MiB and faster above it, which is why the threshold is exactly there. Running the two digests on separate threads is slower than running them one after the other, because the parallel BLAKE3 already owns every core. And SHA-256 caps the combined pass at about 1.1 GB/s no matter how fast BLAKE3 gets, so interop compatibility costs 0.76 CPU seconds per GiB. Hardware SHA is present here and worth 5x over the software path.

## Where the numbers came from

Every performance claim above comes from a repeatable harness, not from a one off run. Timings are medians with quartiles, because this machine has measured the same regime at 773, 2364 and 4165 ms across identical runs. Where an interquartile range overlaps, there is no result.

The corpus is a choice, not a sample of what users fetch: text, columnar, precompressed, images, genomics, audio, many small files, one large file, and consecutive releases of three real datasets. Anything phrased as across the corpus inherits that.

## Rules that do not move

Correctness never moves. Tuning, concurrency, protocol choice, clone versus copy, and I/O mode may change timing. They may never change bytes, digests, or the tree digest.

Every optimization can be disabled without disabling a correctness check.

A secret in an output stream is a breach, not a bug.

Deletion is a change and needs the same evidence as an addition.

The same lock produces a byte identical tree on Windows and Linux, or the run fails and names the exact reason. It never produces a quietly different tree.

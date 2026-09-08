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

Measured in CI on `ubuntu-24.04` and `windows-2025`, which is where the baseline
lives. Every deterministic counter below is the same number on both. Wall time is
a property of the machine as much as of the code, so it is published in the
lane's own output and never gated and never transcribed here.

| Regime | bytes read | bytes written | requests | file operations |
|---|---|---|---|---|
| no-op | 2,048 | 0 | 0 | 1 |
| cold cache, 64 files of 256 KiB | 33,554,432 | 33,554,432 | 0 | 87 |
| warm cache, the same 64 | 16,777,216 | 16,777,216 | 0 | 67 |
| cold transfer, 4 MiB | 4,194,304 | 8,388,608 | 2 | 40 |
| interrupted transfer, two kills | 8,388,608 | 8,388,608 | 6 | 58 |
| 1024 files of 1 KiB | 2,097,152 | 2,097,152 | 0 | 1,047 |
| one 256 MiB file | 536,870,912 | 536,887,240 | 0 | 34 |
| many hosts, concurrency | 0 | 6,291,456 | 32 | 295 |
| many hosts, backoff | 0 | 6,291,456 | 40 | 295 |
| constrained network | 2,097,152 | 4,194,304 | 2 | 40 |

Read the ratios rather than the totals. A cold cache reads its corpus twice and
writes it twice: once into the destination and once into the cache's own copy,
which is metadata instead of bytes on a volume that reference counts blocks and
which no volume in this matrix does. A warm cache reads and writes it once, which
is the speculative ingest write and has its own record. One 256 MiB file is
2.000x read and 2.000x written for the same reason, and 3.000x read when it
compresses, which is the publication-time compression pass. 1024 files of 1 KiB
cost 1.02 file operations each.

Peak memory is 7.5 to 21 MB across every regime including the 256 MiB single file
one. Nothing is ever loaded whole. The binary is 9,667,072 bytes on Windows and
8,815,488 on Linux.

The many hosts regimes are mostly not transfer cost. They inject 100 ms of
latency into 40 requests and rate limit one host, and most of what a clock would
show is backoff this run waits out one request at a time, because a host asking
to be left alone drives its concurrency back to one.

### What a volume accepts

The longest path a volume takes cannot be queried, only built. On Windows the two
candidates are 32,767 and 260, so measuring it meant building a chain of 139
nested 255-character directories, writing into it, failing, and removing the
chain: 325 to 450 ms, against 0.7 ms for the answer that turns out to be right.
That was 78 percent of a run that fetched nothing and correctly said `unchanged`.

The probe is still the authority. What changed is how often it runs: a cache
records the answer in `meta/volume-<volume id>` against the boot that measured it,
and a run whose cache holds a record for this boot and this volume reads it. Warm
`get`, twelve runs each, this machine: 105–131–187 ms with the record against
499–652–991 ms without it. `cache status`: 77 to 109 ms against 442 to 622 ms,
where `fetchloom --version` opens no cache at all and costs 92 to 174 ms.

Disabling it is removing the record, which the next run rewrites, and `--no-cache`
measures every time because a scratch store holds no record from a previous run.
Neither disables a check: the answer is still the volume's own, and a record from
another boot is refused rather than trusted.

Linux pays the same shape and almost none of the cost. Its candidates are 4096 and
255, so the chain is sixteen directories, and 4096 is refused on ext4 exactly as
32,767 is on NTFS. Timed on ext4, five rounds: 3.0 to 3.7 ms for the whole probe.

### Asking what changed

`status` answers from the record where it can and reads bytes where it cannot. Medians of three, release, on the same machine.

| Tree | Fingerprint | Reading every byte | Ratio |
|---|---|---|---|
| 100,000 files of 256 bytes | 6188 ms | 8904 ms | 1.44x |
| 2,000 files of 512 KiB | 393 ms | 1525 ms | 3.88x |

Two shapes because the pre-filter removes the hashing term and nothing else. Walking the tree, stating each file and parsing the record cost the same either way, and over a hundred thousand files those dominate a corpus that is only 25 MB. The term it removes grows with bytes; the term it leaves grows with file count. `verify <path>` over the same hundred thousand files is 8341 ms, which is what status would cost with no record to consult.

### Hashing

Measured on this machine, MiB/s, medians of nine runs.

| Input | BLAKE3 serial | BLAKE3 parallel | SHA-256 | Both, sequential | Both, two threads |
|---|---|---|---|---|---|
| 64 KiB | 2920 | 491 | 1437 | 356 | 120 |
| 256 KiB | 3242 | 719 | 1188 | 902 | 501 |
| 1 MiB | 3007 | 6321 | 1440 | 1081 | 868 |
| 4 MiB | 3277 | 10557 | 1393 | 1105 | 421 |

Two things follow, and the third thing that used to be here was wrong for four months. SHA-256 caps the combined pass no matter how fast BLAKE3 gets, so interop compatibility is what the pass costs, and hardware SHA is present here and worth 5x over the software path. Running the two digests on separate threads is faster than running them one after the other above about 256 KiB, by 1.30x to 1.43x measured six times. What is slower is running a parallel BLAKE3 on one of those threads, because it claims every core while SHA-256 is trying to use one of them: at 1 MiB that shape measures 461 and 473 MB/s where the serial split measures 642 and 619. The table above was measured against the parallel shape and reads as an argument against splitting at all. It was an argument against `update_rayon` inside the split, and the split now runs serial BLAKE3.

## Where the numbers came from

Every performance claim above comes from a repeatable harness, not from a one off run. Timings are medians with quartiles, because this machine has measured the same regime at 773, 2364 and 4165 ms across identical runs. Where an interquartile range overlaps, there is no result.

The corpus is a choice, not a sample of what users fetch: text, columnar, precompressed, images, genomics, audio, many small files, one large file, and consecutive releases of three real datasets. Anything phrased as across the corpus inherits that.

The regime harness has a corpus of its own, and until it was measured it repeated every 251 bytes, so 256 MiB of it stored as 109,034 and the regime named for one enormous file was measuring the compressed publication path. It is a stream no compressor shrinks now, a unit test asserts that against the product's own probe, and every regime number recorded before that boundary was taken against a different corpus than every number recorded after it.

## Rules that do not move

Correctness never moves. Tuning, concurrency, protocol choice, clone versus copy, and I/O mode may change timing. They may never change bytes, digests, or the tree digest.

Every optimization can be disabled without disabling a correctness check.

A secret in an output stream is a breach, not a bug.

Deletion is a change and needs the same evidence as an addition.

The same lock produces a byte identical tree on Windows and Linux, or the run fails and names the exact reason. It never produces a quietly different tree.

### What a pack costs to make durable

A pack is appended to by one process and one boot and by nothing else, so pushing
it to the volume after every entry buys nothing that pushing it once buys. It was
2000 `FlushFileBuffers` on one file for 2000 packed objects, and the run reports
that as 6,021 file operations against 2,023 now. `strict` still pays it, at 6,024,
because per-object durability is what `strict` is for. Disabling the batch is
`--durability strict`, which pushes more rather than less, and no check moves in
either direction: a pack states each entry's length ahead of its bytes, so an
entry cut short by a crash is not one the cache holds whatever was flushed.

### What a request buys

A cold transfer used to be a `HEAD` and then a `GET` on one connection. The
`HEAD` answers three questions: what a partial is keyed by when the key is not a
content digest, where a resume starts when a partial exists, and how wide to
split an object, which needs its length before the first byte. On a first fetch
of one source under a digest the manifest already states, all three are answered
without it — the key is the digest, no partial is recorded, and `parts_for`
refuses to split a host whose recorded concurrency is not above one, which a host
never has on a first fetch. So that fetch is one request, and every other fetch
still probes. `cold-transfer requests` is 1 where it was 2.

The lease is claimed before any request whenever the digest is known. That is
what makes the skip safe: the `GET` cannot start ahead of the deduplication that
stops two runs fetching one digest twice.

### The passes over a locally sourced object

One 256 MiB local file, measured: read 2.000x and written 2.000x when it does not
compress, read 3.000x when it does. Three passes and each is named. The
destination copy is one read and one write with both digests taken on the way
past, and is at its floor. The cache's own copy is one read and one write,
because the cache holds its own bytes and the caller's file is the caller's; on a
volume that reference counts blocks it is metadata instead, and no volume in this
matrix does. The third read happens only when the object compresses, because
compression is at publication and publication is where it has to be for a partial
to stay raw and resumable. A `file:` source has no partial, so that path pays for
a guarantee it never uses — and removing it means a second way to publish, which
is a change to the shape of publication rather than an optimization beside it.

### The archive read twice

A tar is decompressed to enumerate and decompressed again to extract, about 270
ms of a 2500 ms run on a 32 MB tar.gz. Only the enumeration is avoidable, and it
exists because selection, the plan and the bomb guard all take the whole member
list as input and reject before anything is written. Zip pays none of it, which
is what a central directory is for.

### The floor of each stage

Derived from the contracts before any of it was measured, kept because knowing
what a stage cannot avoid is what makes a measurement of it readable. Where the
measurement disagreed, the measurement is what the sections above record.

| Stage | What it cannot avoid | It is falling short if |
|---|---|---|
| resolve | One stat and one small read per document consulted; one round trip for a network reference | It consults the network for something a lock pins, or re-reads one document per artifact |
| plan | Nothing. It is arithmetic over what resolve produced | It stats the destination or the cache per artifact |
| source-select | Nothing when a lock pins the source; one probe per candidate otherwise, bounded by the probe limit | It probes candidates it will not use, or probes serially |
| transfer | The object length once, one connect and one handshake per host per run, one request per object | A handshake repeats per object, a resume refetches verified bytes, or a rung falls to a restart where a range would serve |
| hash | One pass, capped by SHA-256 | Anything hashes an object twice on a path that already has the digest |
| verify | Zero additional bytes during transfer; five stat fields on a fingerprint hit; one full read under `--verify always` | It reads bytes the fingerprint settled |
| store | A rename for an object that arrived over the network; one read and one write for one already local, or metadata where the volume clones | A locally sourced object moves more times than that |
| compress | The head read once and four level-1 compressions of 1 MiB for a decision, and no rewrite when the answer is raw | A raw decision costs a full extra read and write |
| unpack | One pass for tar, one seek and one directory read for zip | Validation walks the archive and extraction walks it again |
| reconcile | One directory walk and one stat per entry | It reads bytes the fingerprint settled |
| publish | One rename per published tree, one directory flush under a strict tier | It is per file |
| record | One small write and one rename, once per run | It is once per object |

Two of those are where the findings were. `store` and `compress` between them are
the two reads and two writes of a locally sourced object, and `record` at two file
operations each is what made the session record visible in the counters.

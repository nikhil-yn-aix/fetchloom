# internals

why the contracts are what they are, and where a change belongs. this is not the
list of promises, which is [contracts.md](contracts.md), and not the surface,
which is [reference.md](reference.md).

## the problem

most public datasets publish no checksum. the reference truth is usually your
own first successful fetch, and nothing records it. fetchloom captures that
moment in a lock and checks every later run against it. that is what
`Cargo.lock` does for code, applied to data.

pinning is the product. verification is the mechanism.

## three ideas

a name for bytes that cannot lie. every object is hashed and the hash is its
name. one byte differs, the name differs. whether you got the right thing stops
being trust and becomes arithmetic.

never claim something worked when it did not. anything lower than what was asked
for emits a `degrade` event naming what was wanted, what happened instead, and
why. there is no path where the worse thing happens quietly.

nothing appears half finished. work happens in staging, is checked, then becomes
visible in one atomic rename. kill it mid run and you have the old tree or the
new one.

## the six seams

identity. BLAKE3 for the cache key and the resume authority, SHA-256 because
that is what publishers state. both computed in one pass over the bytes. BLAKE3
also gives a chunk tree, which is what makes damage locatable instead of only
detectable.

store. content addressed objects. small ones go into packs rather than their own
file, because the cost of many small objects is dominated by file count, not
bytes. one lookup answers where anything is, and packs are self describing so no
index can disagree with them.

source. metadata probe, validators, ranges, authentication, retry guidance,
immutable identity. everything network specific lives behind this, which is why
adding a protocol does not touch the engine.

archive. enumeration, selection, bounded extraction. this is the hostile input
surface, so it is where most of the rejections live.

policy. trust, offline, limits, credentials, terms. every decision that could
weaken a guarantee goes through here rather than being made where it is
convenient.

observer. events, errors, progress, redaction. redaction happens at
construction, not at output, so a secret cannot reach a stream by way of a call
site nobody remembered.

the six were fixed early. widening one takes the same justification as changing
a contract, and narrowing one is the same change in reverse: a method no caller
reaches through the seam is not part of it.

`Store` is the one that was narrower than it looked. it carried twenty-one
methods where the only code generic over it, the transfer, reaches nine. the
other twelve, opening a finished object, its outboard tree, staging, pins,
prune, list, status and the format fingerprint, are asked of the one store by
name, so they are inherent to the cache and the seam does not state them.
nothing is polymorphic over them, and a trait method nothing is polymorphic over
is a function with extra steps, which is exactly the shape that makes a seam
read as wider than it is.

## what each crate knows

the rule is one sentence: a crate depends on the vocabulary of the contracts and
never on the mechanism of another crate. a change belongs in the crate whose
knowledge it needs, and if it needs two, it belongs in whichever of them owns
the decision rather than the mechanism.

| crate | knows | cannot know |
|---|---|---|
| `engine` | identity, canonical forms, references, manifests, locks, receipts, records, plans, selection, layout, trees, reconcile, three way, policy, limits, events, error kinds, the six seam traits | sockets, TLS, HTTP as syntax, tar and zip byte layout, terminal capabilities. it states what is transferred and why a rung was taken. it opens nothing and asks for nothing |
| `platform` | volumes, capability probes, atomic publication, advisory locks, boot identity, the processor budget, the credential store, uncached writes | datasets, references, manifests, digests, archives, the network, the cache's own layout |
| `cache` | object placement, packs, outboard trees, compression frames and dictionaries, quarantine, pins, prune, bundles, the format fingerprint, the meta records | references, manifests, selection, archives, the network, destinations, the library |
| `sources` | protocols, providers, listing APIs, credentials on the wire, per host measurement, range mechanics | the cache layout, the filesystem, archives, destinations, what a dataset is |
| `archive` | container bytes, member enumeration, bounded extraction, path safety, bomb limits | the network, the cache, references, the library |
| `view` | the event stream | everything else, which is the whole reason it is a crate rather than a module: the contract says it cannot influence a run, and a crate boundary is how that becomes a fact a compiler checks |
| `faults` | how to break a seam, and how to decline by name | the product's own logic |
| `cli` | argument syntax, configuration precedence, the project file, the run each command performs, rendering, exit codes | protocol details, archive byte layout, cache layout |

`cli` is the odd one and deliberately so. it holds the run itself, in `run/`,
because a run is where the concrete cache, platform and adapters meet and there
is exactly one composition of them. the rules those runs apply, the three way
table, the reconcile table, the trust classes, resume rungs, splitting and
retry, are in `engine` and are tested there without a filesystem. what is in
`cli` is the wiring and the order, not the decision.

## why blocking, not async

there is no tokio here, no `async fn`, no `.await`. blocking threads with a
budget measured from affinity, container and job limits.

that is a decision, not an accident. tokio's file I/O is not truly asynchronous:
it dispatches to a blocking thread pool, and measurements put native tokio
around 44x slower than the best implementations for small random reads. for disk
work, threads are the right answer.

async would only help network concurrency, and only well above the number of
connections a polite dataset fetcher opens. the cost would be roughly doubling
the dependency graph.

## why not HTTP/2

measured and declined. HTTP/2 wins by multiplexing many requests over one
connection. this workload is a few large transfers, where several HTTP/1.1
connections perform the same. the client used here states its goal as a sync
HTTP/1.1 client with minimum dependencies and has had an HTTP/2 issue open since
2020, so adopting it means moving to hyper plus tokio.

HTTP/3 was measured too. the Rust implementation is still described as
experimental.

revisit when the workload changes, not before.

## why FTP was written rather than depended on

FTP has not changed since RFC 3659 in 2007, so a dependency on it buys future
maintenance of a protocol with no future. the crate that would have been taken
brings `chrono` and `regex` transitively, and both exist in it only to read a
date and a size out of a `LIST` line, which is an `ls` line meant for a person.
`MLSD` states the same as fields, and `FEAT` was checked against every host this
is meant to reach before the choice was made: NCBI and UCSC offer it, EBI and
Ensembl do not, so the `LIST` fallback is real rather than theoretical and is a
`degrade` rather than a silent equivalence.

what is here is one control connection, one reply parser, `PASV`, and two
listing readers, in one file. the TLS is the rustls already in the graph, so
FTPS added no crate.

## why no chunk deduplication

content defined chunking looked obviously worth it and measurement said no.

across seventeen distinct datasets, chunk level deduplication saved 4.3 to 5.4
percent. the headline number of 36 percent came entirely from holding two
consecutive releases of the same dataset at once, which is not something people
do.

the decisive measurement was the same two kernel releases, chunked twice.

| form | saved |
|---|---|
| uncompressed tar | 35.87 percent |
| the same pair as `.tar.gz` | 0.00 percent |

gzip destroys chunk similarity completely, and `.tar.gz` is what publishers
serve. audio saved 0.00 percent at every chunk size.

there was also a security result. an unkeyed chunk index in a cache shared
between users leaks the shape of one user's content to another, and keying it
drops cross user deduplication to zero. so the feature is worthless in the form
that would be safe.

whole file deduplication already works and costs nothing.

## why the archive is kept

after extracting a 10 GB archive to 30 GB, the cache keeps the archive.
measured, that costs 16.1 percent more disk than storing the extracted members
instead.

it is kept for three reasons. the publisher's stated digest is over the archive
bytes, and it is the only independent evidence in the system: throw the archive
away and that proof is gone permanently. localized repair depends on the archive
and its chunk tree, and you cannot seek into a gzip stream to recover one
member. and the cross dataset deduplication that the members layout would buy is
the 5.4 percent above.

## performance

fetchloom is slower than `cp` and `curl` on every workload. it hashes every byte
twice, builds a chunk tree, publishes through staging, and writes a record.
those tools do none of that.

measured in CI on `ubuntu-24.04` and `windows-2025`, which is where the baseline
lives. every deterministic counter below is the same number on both. wall time
is a property of the machine as much as of the code, so it is published in the
lane's own output and never gated and never transcribed here.

| regime | bytes read | bytes written | requests | file operations |
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

read the ratios rather than the totals. a cold cache reads its corpus twice and
writes it twice, once into the destination and once into the cache's own copy,
which is metadata instead of bytes on a volume that reference counts blocks, and
which no volume in this matrix does. a warm cache reads and writes it once,
which is the speculative ingest write and has its own record. one 256 MiB file
is 2.000x read and 2.000x written for the same reason, and 3.000x read when it
compresses, which is the publication-time compression pass. 1024 files of 1 KiB
cost 1.02 file operations each.

peak memory is 7.5 to 21 MB across every regime including the 256 MiB single
file one. nothing is ever loaded whole. the binary is 9,667,072 bytes on Windows
and 8,815,488 on Linux.

peak memory is gated, per target, against the runner that recorded it. the two
runners differ by 44 percent on the same regime, 21.1 MB on Linux against 14.6
MB on Windows for many hosts, concurrency, because the allocator and the page
size differ by target, so one number could never gate both. across runs on one
runner it moves at most 2.8 percent, which is what makes a ten percent one sided
band a gate rather than a coin toss. on a developer's machine under load the
same regime has measured 9.9 and 17.9 MB, which is the other reason the band is
not five percent and the reason the lane is a CI lane.

the many hosts regimes are mostly not transfer cost. they inject 100 ms of
latency into 40 requests and rate limit one host, and most of what a clock would
show is backoff this run waits out one request at a time, because a host asking
to be left alone drives its concurrency back to one.

### what a volume accepts

the longest path a volume takes cannot be queried, only built. on Windows the
two candidates are 32,767 and 260, so measuring it meant building a chain of 139
nested 255-character directories, writing into it, failing, and removing the
chain: 325 to 450 ms, against 0.7 ms for the answer that turns out to be right.
that was 78 percent of a run that fetched nothing and correctly said
`unchanged`.

the probe is still the authority. what changed is how often it runs. a cache
records the answer in `meta/volume-<volume id>` against the boot that measured
it, and a run whose cache holds a record for this boot and this volume reads it.
warm `get`, twelve runs each, this machine: 105, 131 and 187 ms with the record
against 499, 652 and 991 ms without it. `cache status`: 77 to 109 ms against 442
to 622 ms, where `fetchloom --version` opens no cache at all and costs 92 to 174
ms.

disabling it is removing the record, which the next run rewrites, and
`--no-cache` measures every time because a scratch store holds no record from a
previous run. neither disables a check: the answer is still the volume's own,
and a record from another boot is refused rather than trusted.

Linux pays the same shape and almost none of the cost. its candidates are 4096
and 255, so the chain is sixteen directories, and 4096 is refused on ext4
exactly as 32,767 is on NTFS. timed on ext4, five rounds: 3.0 to 3.7 ms for the
whole probe.

### asking what changed

`status` answers from the record where it can and reads bytes where it cannot.
medians of three, release, on the same machine.

| tree | fingerprint | reading every byte | ratio |
|---|---|---|---|
| 100,000 files of 256 bytes | 6188 ms | 8904 ms | 1.44x |
| 2,000 files of 512 KiB | 393 ms | 1525 ms | 3.88x |

two shapes because the pre-filter removes the hashing term and nothing else.
walking the tree, stating each file and parsing the record cost the same either
way, and over a hundred thousand files those dominate a corpus that is only 25
MB. the term it removes grows with bytes. the term it leaves grows with file
count. `verify <path>` over the same hundred thousand files is 8341 ms, which is
what status would cost with no record to consult.

### hashing

measured on this machine, MiB/s, medians of nine runs.

| input | BLAKE3 serial | BLAKE3 parallel | SHA-256 | both, sequential | both, two threads |
|---|---|---|---|---|---|
| 64 KiB | 2920 | 491 | 1437 | 356 | 120 |
| 256 KiB | 3242 | 719 | 1188 | 902 | 501 |
| 1 MiB | 3007 | 6321 | 1440 | 1081 | 868 |
| 4 MiB | 3277 | 10557 | 1393 | 1105 | 421 |

two things follow, and the third thing that used to be here was wrong for four
months. SHA-256 caps the combined pass no matter how fast BLAKE3 gets, so
interop compatibility is what the pass costs, and hardware SHA is present here
and worth 5x over the software path. running the two digests on separate threads
is faster than running them one after the other above about 256 KiB, by 1.30x to
1.43x measured six times.

what is slower is running a parallel BLAKE3 on one of those threads, because it
claims every core while SHA-256 is trying to use one of them: at 1 MiB that
shape measures 461 and 473 MB/s where the serial split measures 642 and 619. the
table above was measured against the parallel shape and reads as an argument
against splitting at all. it was an argument against `update_rayon` inside the
split, and the split now runs serial BLAKE3.

## where the numbers came from

every performance claim above comes from a repeatable harness, not from a one
off run. timings are medians with quartiles, because this machine has measured
the same regime at 773, 2364 and 4165 ms across identical runs. where an
interquartile range overlaps, there is no result.

the corpus is a choice, not a sample of what users fetch: text, columnar,
precompressed, images, genomics, audio, many small files, one large file, and
consecutive releases of three real datasets. anything phrased as across the
corpus inherits that.

the regime harness has a corpus of its own, and until it was measured it
repeated every 251 bytes, so 256 MiB of it stored as 109,034 and the regime
named for one enormous file was measuring the compressed publication path. it is
a stream no compressor shrinks now, a unit test asserts that against the
product's own probe, and every regime number recorded before that boundary was
taken against a different corpus than every number recorded after it.

## rules that do not move

correctness never moves. tuning, concurrency, protocol choice, clone versus
copy, and I/O mode may change timing. they may never change bytes, digests, or
the tree digest.

every optimization can be disabled without disabling a correctness check.

a secret in an output stream is a breach, not a bug.

deletion is a change and needs the same evidence as an addition.

the same lock produces a byte identical tree on Windows and Linux, or the run
fails and names the exact reason. it never produces a quietly different tree.

### what a pack costs to make durable

a pack is appended to by one process and one boot and by nothing else, so
pushing it to the volume after every entry buys nothing that pushing it once
buys. it was 2000 `FlushFileBuffers` on one file for 2000 packed objects, and
the run reports that as 6,021 file operations against 2,023 now. `strict` still
pays it, at 6,024, because per-object durability is what `strict` is for.
disabling the batch is `--durability strict`, which pushes more rather than
less, and no check moves in either direction: a pack states each entry's length
ahead of its bytes, so an entry cut short by a crash is not one the cache holds
whatever was flushed.

### what a request buys

a cold transfer used to be a `HEAD` and then a `GET` on one connection. the
`HEAD` answers three questions: what a partial is keyed by when the key is not a
content digest, where a resume starts when a partial exists, and how wide to
split an object, which needs its length before the first byte. on a first fetch
of one source under a digest the manifest already states, all three are answered
without it. the key is the digest, no partial is recorded, and `parts_for`
refuses to split a host whose recorded concurrency is not above one, which a
host never has on a first fetch. so that fetch is one request, and every other
fetch still probes. `cold-transfer requests` is 1 where it was 2.

the lease is claimed before any request whenever the digest is known. that is
what makes the skip safe: the `GET` cannot start ahead of the deduplication that
stops two runs fetching one digest twice.

### the passes over a locally sourced object

one 256 MiB local file, measured: read 2.000x and written 2.000x when it does
not compress, read 3.000x when it does. three passes and each is named. the
destination copy is one read and one write with both digests taken on the way
past, and is at its floor. the cache's own copy is one read and one write,
because the cache holds its own bytes and the caller's file is the caller's. on
a volume that reference counts blocks it is metadata instead, and no volume in
this matrix does. the third read happens only when the object compresses,
because compression is at publication and publication is where it has to be for
a partial to stay raw and resumable. a `file:` source has no partial, so that
path pays for a guarantee it never uses, and removing it means a second way to
publish, which is a change to the shape of publication rather than an
optimization beside it.

### the archive read twice

a tar is decompressed to enumerate and decompressed again to extract, about 270
ms of a 2500 ms run on a 32 MB tar.gz. only the enumeration is avoidable, and it
exists because selection, the plan and the bomb guard all take the whole member
list as input and reject before anything is written. zip pays none of it, which
is what a central directory is for.

### the floor of each stage

derived from the contracts before any of it was measured, kept because knowing
what a stage cannot avoid is what makes a measurement of it readable. where the
measurement disagreed, the measurement is what the sections above record.

| stage | what it cannot avoid | it is falling short if |
|---|---|---|
| resolve | one stat and one small read per document consulted; one round trip for a network reference | it consults the network for something a lock pins, or re-reads one document per artifact |
| plan | nothing. it is arithmetic over what resolve produced | it stats the destination or the cache per artifact |
| source-select | nothing when a lock pins the source; one probe per candidate otherwise, bounded by the probe limit | it probes candidates it will not use, or probes serially |
| transfer | the object length once, one connect and one handshake per host per run, one request per object | a handshake repeats per object, a resume refetches verified bytes, or a rung falls to a restart where a range would serve |
| hash | one pass, capped by SHA-256 | anything hashes an object twice on a path that already has the digest |
| verify | zero additional bytes during transfer; five stat fields on a fingerprint hit; one full read under `--verify always` | it reads bytes the fingerprint settled |
| store | a rename for an object that arrived over the network; one read and one write for one already local, or metadata where the volume clones | a locally sourced object moves more times than that |
| compress | the head read once and four level-1 compressions of 1 MiB for a decision, and no rewrite when the answer is raw | a raw decision costs a full extra read and write |
| unpack | one pass for tar, one seek and one directory read for zip | validation walks the archive and extraction walks it again |
| reconcile | one directory walk and one stat per entry | it reads bytes the fingerprint settled |
| publish | one rename per published tree, one directory flush under a strict tier | it is per file |
| record | one small write and one rename, once per run | it is once per object |

two of those are where the findings were. `store` and `compress` between them
are the two reads and two writes of a locally sourced object, and `record` at
two file operations each is what made the session record visible in the
counters.

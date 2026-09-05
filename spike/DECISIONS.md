# Decisions

Every decision the nine experiments produced, one or two sentences each, written so it can be
lifted into a contract. Each names the measurement that produced it, or says plainly that it
was decided on design grounds rather than measured.

Nothing here has been written into the main repository. Contract targets are named so the
next planning session knows where each lands.

## Experiment 1. Cache compression

**D1.1 Compress cached objects by default, at zstd level 1 only.** Level 1 is the only level
that keeps the whole hash-compress-write pipeline ahead of a gigabit link on every corpus
regime (124-259 MiB/s against 119 MiB/s); level 3 falls behind on float arrays at 86 MiB/s and
level 6 falls behind on three of four targets at 37-51 MiB/s.

**D1.2 The decision is the pipeline, not the codec.** Uncompressed hash-and-write tops out at
about 354 MiB/s on this machine, so above roughly 2.5 Gbit/s Fetchloom is hash-and-disk bound
and compression is never the right trade; below about 100 Mbit/s any level is affordable.

**D1.3 Frame size 1 MiB**, matching the outboard chunk group, the pack threshold and the
streaming buffer. It costs at most 3.2 percent of ratio against unframed zstd and at most 1.4
percentage points against zeekstd's 2 MiB default, and it reads ranges *faster* than 2 or
4 MiB frames in every one of the eleven entries tested.

**D1.4 Ranged repair pays 2.5x to 8.2x for compression, forever.** One random 1 MiB range from
a 1 MiB-framed seekable object costs 1.30 to 7.91 ms against 0.43 to 0.97 ms uncompressed.

**D1.5 Probe with a 1 MiB head at level 1 and store raw below ratio 1.10.** That classified
23 of 23 corpus entries correctly, zero false positives and zero false negatives; a 64 KiB
head misclassified 2 to 3 entries at every threshold because file headers compress when
bodies do not.

**D1.6 Byte-shuffle float arrays at stride 4 before compressing.** On the NOAA NCEP float32
array it lifted zstd-1 from 1.692 to 2.735, a 62 percent gain, and shuffled level 1 beats
unshuffled level 6 while running about 5x faster.

**D1.7 Detect whether the target volume already compresses, and do not compress twice.**
`FILE_ATTRIBUTE_COMPRESSED` on a file the cache creates anyway answers it for NTFS at no cost;
this belongs beside the existing platform capability list that already reports clone support,
sparse support and on-access scanners.

**D1.8 State the price as 3.96 to 8.23 CPU-seconds per gigabyte** for objects the probe
compresses, and effectively nothing for those it stores raw.

**D1.9 Columnar Parquet is an already-compressed regime.** All three real NYC TLC files
compress at ratio 1.002-1.004 at every level; no design may assume columnar means compressible.

Contract targets: a new cache-storage section, the tuning table, and the platform capability
list.

## Experiment 2. Chunk deduplication

**D2.1 Do not build content-defined chunking.** It saves 4.34 to 5.39 percent across
seventeen genuinely distinct real objects, and 0.00 percent between consecutive releases of
the same dataset in the compressed form publishers actually serve.

**D2.2 Compression destroys chunk-level similarity, measured not assumed.** Two genuine
consecutive-release pairs went from 35.87 and 32.78 percent saving uncompressed to 0.00 and
0.02 percent as the exact `.tar.gz` and `.gz` their publishers serve.

**D2.3 Cost was never the objection.** Chunking runs at about 700 MiB/s, twice the whole
hash-and-write pipeline, the whole-corpus index is 17.8 MB at a 16 KiB average, and lookup is
about 20 ns.

**D2.4 If it is ever built: 16 KiB average, keyed per cache, never per user.** Per-user keying
yields 0.00 percent cross-user deduplication on byte-identical content, so keying and
cross-user dedup are mutually exclusive rather than a trade-off to tune.

**D2.5 An unkeyed shared chunk index leaks one user's content to another, exactly.** Probing
a shared index gives 100.00 percent chunk hits when another user has cached an object and
0.00 percent when they have not, across 5,015 chunks — a reliable existence oracle, which
matters because a cache shared between users on one machine is supported.

**D2.6 A chunked object satisfies the whole-object invariant only if its whole-object digest
is verified after reassembly and before publication**, because chunk digests prove the parts
and not their order.

**D2.7 The finding worth keeping is not chunking.** Fetchloom sees zero version-to-version
similarity because publishers serve compressed archives; any future incremental-fetch work
should target obtaining uncompressed bytes, not chunking compressed ones.

Contract targets: none, this is a decision not to add a contract.

## Experiment 3. Storage layout

**D3.1 Keep the whole archive. Decided on design grounds, not benchmark grounds.** The
publisher's stated digest is over the archive bytes and is the only independent evidence in
the system; localized repair depends on the archive and its outboard tree, and a gzip stream
cannot be seeked into at all; and experiment 2 already showed the cross-dataset dedup argument
is worth 5.39 percent.

**D3.2 State the storage cost as about 16 percent on top of the extracted tree.** The Linux
kernel `.tar.gz` is 224,925,704 bytes beside an extracted tree of 1,393,614,166 bytes in
87,703 files, a 6.20x extraction multiple; the overhead is bounded by the archive's own
compression ratio and is never worse than 100 percent.

**D3.3 The reflink comparison is unmeasured and needs hardware this machine does not have.**
`D:` is NTFS, which has no reflink; answering it requires ReFS, or btrfs or XFS with
`reflink=1` in a privileged container.

Contract targets: the cache layout section, for the 16 percent figure.

## Experiment 4. Concurrency and splitting

**D4.1 Lower the per-host ceiling from 4 to 2.** Concurrency 2 was the only setting that
improved any real host (the academic mirror, 1.6 to 2.5 MiB/s, 1.56x); concurrency 4 was never
better than 2 on any real host and was worse on the academic mirror, where it also tripled
request latency.

**D4.2 Keep the per-host start at 2.** It is where the only measured gain is, and the adaptive
controller's shape is right; only its ceiling is a step too high.

**D4.3 Raise the global ceiling from 8 to 32.** Against 16 local hosts at a fixed per-host of
4, throughput scaled 1.00x, 1.70x, 2.67x, 3.94x and 4.71x at global 4, 8, 16, 32 and 64, with
non-overlapping interquartile ranges at every step; the current ceiling of 8 leaves about 2.8x
unused when a run has many hosts.

**D4.4 Do not raise it to 64.** The gain from 32 to 64 is 1.20x for double the sockets, it was
measured on loopback rather than against real hosts, and reaching it requires 16 hosts in
flight, a politeness surface this spike did not test in the wild.

**D4.5 Leave the split threshold at 64 MiB and keep its four preconditions.** aria2's 10 MB
default is unsupported by anything measured here — splitting at 10 MiB gained 1.06x, inside
the noise — and splitting at 64 MiB on a saturated link was 2.6x *worse* than not splitting.

**D4.6 Politeness evidence: no host ever pushed back.** One request in roughly 1,400 was not a
200 or 206, and it was a 21.7 second transport stall at per-host concurrency 1, not a 429 or a
503. No configuration tested was faster because it hammered a server.

**D4.7 The abort rule needs to distinguish a server objecting from a path failing.** Treating
any non-2xx as an objection dropped the object store on a transport timeout at concurrency 1
and cost that host's data; a 429 or 503 is pushback, a stall is not.

**D4.8 Fetchloom can materialise a tree it cannot verify, and this is a defect.** A 93,182-entry
tree materialised correctly, then every `verify` of it failed with `manifest.invalid`, because
the record `get` wrote is 19,288,996 bytes and the listing limit `verify` enforces is 16 MiB;
reconcile's cost curve could not be measured and stays an open risk for the overlay design.

Contract targets: the tuning table (`Connections to one host`, the global ceiling constant
`TRANSFERS_CEILING`, `Split threshold`), and the adaptive controller section.

## Experiment 5. HTTP/2 and HTTP/3

**D5.1 Do not adopt HTTP/2, and do not adopt an async runtime for it.** Fetchloom's workload
is a few large transfers, where multiplexing wins little; adopting it means hyper or reqwest
plus tokio, because ureq 3.x is deliberately a synchronous HTTP/1.1 client.

**D5.2 The dependency cost is measured: 80 crates against 176.** A reqwest and tokio HTTP/2
client alone carries more crates than Fetchloom's entire workspace carries today (161), to
replace one carrying 80; HTTP/3 costs 181.

**D5.3 Advertising a protocol is not offering it, and must be measured per host.**
`cdn.kernel.org` advertises `h3` in `alt-svc` yet its QUIC handshake timed out from this
network, while `cloudflare-quic.com` negotiated HTTP/3.0 with the same client.

**D5.4 A client that is not told which version to use will silently fall back and look like
it succeeded.** Requesting HTTP/3 without pinning the version returned HTTP/1.1 and a 200,
which is the exact shape of a number that looks like a result and is not one.

Contract targets: none; record in decisions.md as a considered and rejected option.

## Experiment 6. FTP and SFTP

**D6.1 Add FTP behind the existing blocking Source seam.** `suppaftp` supplied a bounded
metadata probe (`SIZE`), a modification time (`MDTM`), a directory listing and byte-exact
offset resume (`REST`) against `ftp.gnu.org` and `ftp.ensembl.org` with no async runtime.

**D6.2 FTP and SFTP both land on resume rung 4 and may claim no higher.** Both state a length
and a modification time; neither can state an entity tag or an immutable content address,
which rungs 3 and 2 require. Rung 1 stays reachable, because it rests on Fetchloom's own
outboard tree.

**D6.3 Do not add SFTP in the same change.** It lands on the same rung 4 and adds no resume
capability, while `russh-sftp` requires tokio and the synchronous alternative `ssh2` requires
a C dependency.

**D6.4 Each degrades to rung 4 and emits a `degrade` naming it**, stating that an entity tag
was wanted and a modification time was used.

Contract targets: the Resume ladder table, and the source adapter list.

## Experiment 7. The Python integration

**D7.1 Build the fsspec adapter; it is viable.** `pandas.read_parquet`, `pyarrow.dataset`,
`pyarrow.ParquetFile` over a file object, and `dask.dataframe.read_parquet` all returned the
same 2,964,624 rows from a real published Parquet file through `fetchloom://`, unmodified.

**D7.2 Warm overhead is not measurable: -1.3 percent** against reading the identical file
directly from local disk, 0.306 s against 0.310 s over 7 runs. Cold first access cost 1.324 s,
of which the subprocess was 0.747 s.

**D7.3 The binary needs a metadata probe command that states length and identity without
transferring.** `fs.info()` currently works only by materialising the whole object, and fsspec
calls it constantly; the Source seam already performs this probe internally and does not
expose it.

**D7.4 The binary needs a listing command that resolves a container without materialising
it.** `fs.ls()` on a container fails with `reference.unresolved`, which is correct behaviour
but means `fetchloom://` cannot support globs, directory datasets or partitioned Parquet —
which is how most real Parquet is published.

**D7.5 Ranged read out of a cached object is a library concern, not a flag.** It is the same
capability experiment 1's seekable frames provide and should be built once for both.

Contract targets: two new commands in the CLI surface; the cache read API.

## Experiment 8. CPU work

**D8.1 Keep SHA-256 with hardware acceleration; it is real and detected.** Forcing `sha2`'s
soft backend cost a factor of 4.8 to 10 on the same bytes (1,437 against 145 MiB/s at 64 KiB).

**D8.2 State the interop digest's price as 0.76 CPU-seconds per GiB.** BLAKE3 with rayon runs
at 6,845 MiB/s at 64 MiB and the combined two-thread pass at 1,124 MiB/s, so hashing a GiB
costs 0.91 CPU-seconds with interop and 0.15 without.

**D8.3 SHA-256 is the entire cost of the hashing pass, not a tax on it.** The combined pass
lands within a few percent of SHA-256 alone, so future work on hashing throughput must target
the interop digest; making BLAKE3 faster would gain nothing.

**D8.4 Make the two-thread hash split conditional on object size, threshold between 4 and
16 MiB.** `docs/standards.md` requires the split unconditionally, but below about 4 MiB it is
1.4x to 5.9x *slower* than hashing serially (119 against 701 MiB/s at 64 KiB).

**D8.5 Keep the BLAKE3 parallel threshold at 1 MiB.** Rayon loses to a single thread below
256 KiB and wins by 2.1x at 1 MiB, so the crossover sits between them.

Contract targets: `docs/standards.md` under CPU, and the tuning table.

## Experiment 9. Hint and watch

**D9.1 Delete `restarted_from_zero`.** No production code sets it; it is assigned only inside
`hint.rs`'s own test module, so its branch cannot be reached by a real run.

**D9.2 Make `offer_a_hint` fall through to the next candidate, or delete `selection`.**
`lock_written` is set by every successful `get` and outranks `selection`, and the already-said
check returns without trying the next candidate, so after the first successful get on a cache
no other hint can ever print.

**D9.3 A hint is printed once per cache, ever.** Across 15 real-console runs, 2 hints were
printed, both `lock-written` (confirmed by digest); a ten-run sequence on one cache printed
exactly 1.

**D9.4 Keep `watch`, and fix the promise rather than the feature.** It is 23 lines reusing the
existing `Event` type and `Live` renderer, but it stops at end of file instead of following,
so either implement following for the file form or restate `--help` as replay only. Judgment,
not measurement.

Contract targets: the hint section, and the `watch` help text.

# Spike results

Nine questions, measured on one machine. Every number here came out of a run in `data/`.
Where a thing could not be measured, it says so rather than substituting something weaker.

## The machine, and why that matters

```
CPU        12th Gen Intel Core i5-12500H, 12 cores / 16 threads, 4 P + 8 E
Memory     16.0 GiB total, 5.5-6.1 GiB free during the runs
Disk       D:, NTFS, 200 GB, 54-77 GB free across the session
OS         Windows 11 Home Single Language 10.0.26200
Toolchain  rustc 1.98.0, cargo 1.98.0, x86_64-pc-windows-msvc
Python     3.12.10
```

Conditions held for every local measurement unless a section says otherwise:
Defender real-time protection **on**, **no** exclusion for the repository (the process could
not read the exclusion list without administrator rights, so "no exclusion" is asserted from
the fact that none was added by this spike, not from reading the list), Docker Desktop
**running** (29.6.1), VS Code and Chrome resident. `data/machine_before_exp8.json` is the
recorded snapshot.

This machine is not quiet. `docs/standards.md` already records the same regime measured at
7,150 ms and 28,801 ms. Every timing below is the median of at least 7 runs and is reported
with min, median, max and interquartile range. Ratios and byte counts are deterministic and
do not carry that caveat.

Experiments were run **serially**. A timing taken while another experiment loaded the same
disk or the same link is not comparable and was discarded. Two such measurements were
discarded and re-taken: the first HTTP/1.1 and HTTP/2 smoke tests were taken while the
corpus was still downloading, and one experiment 9 run was contaminated by a second
invocation of itself.

## The corpus

Real data, no generated data, 23 entries, 15.11 GiB, described by `corpus/manifest.json`
with a provenance URL per entry.

| entry | kind | size | provenance |
|---|---|---|---|
| enwik8 | highly compressible text | 95.4 MiB | mattmahoney.net |
| silesia | highly compressible text | 202.1 MiB | Silesia corpus, 12 files, tarred |
| parquet-2024-01/02/03 | columnar | 47.6 / 48.0 / 57.3 MiB | NYC TLC yellow trip data |
| linux-6.6.1.tar.gz | already compressed | 214.5 MiB | cdn.kernel.org |
| linux-arch.pkg.tar.zst | already compressed | 148.4 MiB | Arch linux 7.2.3 package |
| binutils.pkg.tar.zst | already compressed | 8.2 MiB | Arch binutils 2.47 |
| ncep_air_2020.nc | already compressed | 103.3 MiB | NOAA NCEP, NETCDF4 with HDF5 zlib |
| photos | incompressible | 208.2 MiB | 15 Wikimedia Commons featured photographs |
| ncep_air_2020_float32.raw | float array | 249.5 MiB | the `air` variable, float32, written raw |
| librispeech_wav.tar | audio, uncompressed | 596.7 MiB | LibriSpeech dev-clean, 2703 utterances |
| librispeech_flac.tar | audio, compressed | 347.0 MiB | the same 2703 utterances as published |
| GRCh38.114.gtf | scientific | 1666.7 MiB | Ensembl release 114 |
| cdna.114.fa | scientific | 433.9 MiB | Ensembl release 114 |
| chr1.114.fa | scientific | 241.4 MiB | Ensembl release 114 |
| linux-6.6.1.tar | many small files | 1353.4 MiB | 87,703 files when extracted |
| primary_assembly.114.fa | one large file | 3005.4 MiB | Ensembl release 114 |

### The versioned pairs

Three pairs, and the distinction between them matters for experiment 2.

| pair | kind | A | B | genuine? |
|---|---|---|---|---|
| linux 6.6.1 → 6.6.2 | consecutive upstream point releases | 1353.4 MiB | 1353.4 MiB | **yes** |
| Ensembl GTF 113 → 114 | consecutive annotation releases | 1662.0 MiB | 1666.7 MiB | **yes** |
| Ensembl cDNA 113 → 114 | consecutive FASTA releases | 433.6 MiB | 433.9 MiB | **yes** |
| appendonly A → B | B is A plus a tail | 1333.3 MiB | 1666.7 MiB | **synthesised** |

The first three are genuine consecutive published releases, downloaded separately from the
publisher, not one file with bytes changed. The fourth is explicitly synthesised: it is the
Ensembl 114 GTF cut at 80 percent of its whole lines, so that B is exactly A plus a tail.
It is labelled synthesised everywhere it appears and exists only to measure the append-only
ceiling, which none of the genuine pairs occupies.

One candidate was **rejected**: Ensembl chromosome 21 FASTA is byte-identical between
releases 113 and 114 (both 11,787,503 bytes, same content). A pair that is 100 percent
identical measures nothing, so it was not used.

## Experiment 1. Cache compression

**Question.** Should cached objects be stored compressed, at what level, and how is that
decided per object?

**Method.** Four measurements, all in `src/bin/exp1_compress.rs`, all 7 runs per point.
Levels 1, 3 and 6 across all 23 entries on a 32 MiB slab; levels 9 and 19 on three
representative entries only, because a level that compresses at single-digit MB/s can never
be chosen on the download hot path no matter what its ratio is. Frame alignment and ranged
reads on a 256 MiB slab. Whole-object streaming ratios and sample prediction on the entire
file. The pipeline measurement uses a 512 MiB slab and `sync_all`. Raw output in
`data/exp1_{levels,pipeline,frames,probe}.json`.

### The headline: the whole pipeline, not the codec

This is the number that decides it. Compression goes on by default only if the pipeline
stays faster than the network. Hash means BLAKE3 plus SHA-256 on two threads, which is what
Fetchloom actually does; write means to disk followed by `sync_all`.

| entry | level | hash+write MiB/s (min/med/max) | hash+compress+write MiB/s | throughput cost | ratio | CPU s per GiB |
|---|---|---|---|---|---|---|
| primary-assembly (FASTA) | 1 | 40/354/391 | 80/**124**/146 | 65% | 3.391 | 8.23 |
| primary-assembly | 3 | 40/354/391 | 121/**129**/133 | 63% | 3.374 | 7.92 |
| primary-assembly | 6 | 40/354/391 | 37/**40**/42 | 89% | 3.519 | 25.83 |
| ncep float32 | 1 | 41/346/381 | 107/**132**/146 | 62% | 1.714 | 7.76 |
| ncep float32 | 3 | 41/346/381 | 82/**86**/95 | 75% | 2.101 | 11.86 |
| ncep float32 | 6 | 41/346/381 | 34/**37**/41 | 89% | 2.248 | 27.97 |
| linux tar.gz | 1 | 301/348/390 | 177/**259**/289 | 26% | 1.004 | 3.96 |
| linux tar.gz | 3 | 301/348/390 | 33/**228**/239 | 35% | 1.009 | 4.50 |
| linux tar.gz | 6 | 301/348/390 | 133/**170**/191 | 51% | 1.012 | 6.02 |
| librispeech WAV | 1 | 72/327/401 | 184/**225**/237 | 31% | 1.213 | 4.56 |
| librispeech WAV | 3 | 72/327/401 | 71/**115**/119 | 65% | 1.246 | 8.89 |
| librispeech WAV | 6 | 72/327/401 | 49/**51**/55 | 85% | 1.269 | 20.23 |

**Against real link speeds.** 100 Mbit/s is 11.9 MiB/s, 1 Gbit/s is 119 MiB/s, 2.5 Gbit/s is
298 MiB/s, 10 Gbit/s is 1,192 MiB/s.

| link | uncompressed pipeline (354) | zstd-1 (124-259) | zstd-3 (86-228) | zstd-6 (37-170) |
|---|---|---|---|---|
| 100 Mbit/s | ahead 30x | ahead 10-22x | ahead 7-19x | ahead 3-14x |
| 1 Gbit/s | ahead 3.0x | **ahead 1.04-2.2x** | **behind on float, ahead elsewhere** | **behind on 3 of 4** |
| 2.5 Gbit/s | ahead 1.2x | behind on all | behind on all | behind on all |
| 10 Gbit/s | behind | behind | behind | behind |

The uncompressed pipeline itself tops out at about 354 MiB/s on this machine, which is
2.9 Gbit/s, so above roughly 2.5 Gbit/s Fetchloom is already hash-and-disk bound and the
network is not the constraint. Between those points the answer is: **zstd-1 stays ahead of a
gigabit link on every corpus entry; zstd-3 does not, and zstd-6 is not close.** The worst
zstd-1 case, 124 MiB/s on FASTA, clears a gigabit link by 4 percent, which is not a margin to
design on, and that is why the probe matters: the entries where zstd-1 is slowest are the
ones that compress, and the entries that do not compress must never pay for it at all.

### Level sweep

Full table in `data/exp1_levels.json`; the shape of it:

| kind | representative | L1 ratio | L3 | L6 | L9 | L19 | L1 compress MiB/s (med) |
|---|---|---|---|---|---|---|---|
| highly compressible text | enwik8 | 2.451 | 2.821 | 3.064 | 3.213 | 3.683 | 230 |
| scientific text | gtf-114 | 30.269 | 28.413 | 33.222 | - | - | 656 |
| genomic FASTA | primary-assembly | 3.287 | 3.308 | 3.427 | - | - | 183 |
| float array | ncep float32 | 1.692 | 2.083 | 2.239 | 2.360 | 2.415 | 207 |
| float array, byte-shuffled | ncep float32 | **2.735** | 2.703 | 2.789 | 2.810 | - | - |
| audio WAV | librispeech | 1.213 | 1.246 | 1.269 | - | - | - |
| columnar | parquet-2024-01 | 1.002 | 1.004 | 1.002 | - | - | 980 |
| already compressed | linux tar.gz | 1.000 | 1.000 | 1.000 | 1.000 | 1.001 | 1478 |
| incompressible | photos | 1.024 | 1.024 | 1.024 | - | - | 963 |

**Byte-shuffling changes the answer for float arrays.** Stride-4 shuffling before compression
lifts zstd-1 from 1.692 to **2.735**, a 62 percent gain, and it collapses the level curve:
shuffled zstd-1 (2.735) beats unshuffled zstd-6 (2.239) while running roughly 5x faster. This
is what Blosc and the HDF5 filters do and the corpus confirms it is worth doing.

**Two reproducible inversions, where a higher level is worse.** On the GTF pair, level 3
scores *below* level 1 (30.269 against 28.413 for release 114; 30.449 against 28.583 for
release 113), and the same happens on cDNA (5.247 against 5.061). It reproduces across
independent sibling files, so it is not noise. Level is not monotone in ratio on this data.
**Marked UNEXPLAINED**: the plausible cause is zstd's strategy change between levels
interacting with this data's long, highly repetitive line structure, but that was not
verified and no decision here depends on it, because level 1 is chosen anyway.

**Parquet is an already-compressed regime, not a compressible one.** All three NYC TLC files
sit at ratio 1.002-1.004 at every level, because Parquet compresses its own column chunks.
Any design that assumes "columnar means compressible" is wrong on real published Parquet.

**Generic compression is not a substitute for a domain codec on audio.** On the same 2,703
LibriSpeech utterances, FLAC as published achieves ratio **1.719** where zstd-3 on the WAV
achieves **1.251**. Generic compression captures about a third of what the domain codec does,
and zstd on the FLAC recovers only a further 1.080.

**Counter-experiment.** The regimes where compression should look bad were measured and it
does look bad: three already-compressed archives at ratio 1.000, real photographs at 1.024,
Parquet at 1.002. The result is not one-directional.

### Frame alignment

256 MiB slab, zstd-3, zeekstd seekable format, 16 random 1 MiB ranges per point.
zeekstd's own default is a 2 MiB frame; Fetchloom's outboard chunk group, pack threshold and
streaming buffer are all 1 MiB.

| entry | ratio at 1 MiB | 2 MiB | 4 MiB | plain zstd-3 | 1 MiB ratio cost | 1 MiB range read ms | 2 MiB | 4 MiB | uncompressed read ms |
|---|---|---|---|---|---|---|---|---|---|
| enwik8 | 2.776 | 2.800 | 2.805 | 2.823 | 1.7% | **2.94** | 3.84 | 5.60 | 0.56 |
| silesia | 3.141 | 3.165 | 3.175 | 3.205 | 2.0% | **3.11** | 3.92 | 5.62 | 0.47 |
| linux-6.6.1.tar | 5.346 | 5.424 | 5.455 | 5.522 | 3.2% | **2.30** | 2.95 | 4.52 | 0.48 |
| primary-assembly | 3.506 | 3.513 | 3.498 | 3.519 | 0.4% | **3.30** | 4.18 | 5.63 | 0.47 |
| gtf-114 | 29.982 | 29.929 | 29.781 | 29.773 | -0.7% | **1.31** | 1.52 | 2.15 | 0.44 |
| ncep float32 | 2.096 | 2.099 | 2.100 | 2.100 | 0.2% | **7.91** | 13.85 | 22.98 | 0.97 |
| librispeech WAV | 1.263 | 1.265 | 1.265 | 1.271 | 0.6% | **3.84** | 11.96 | 7.28 | 0.51 |
| parquet | 1.002 | 1.002 | 1.002 | 1.004 | 0.2% | **1.30** | 1.66 | 2.10 | 0.53 |
| photos | 1.006 | 1.006 | 1.006 | 1.005 | -0.0% | **1.82** | 2.57 | 2.29 | 0.43 |

**A 1 MiB frame costs at most 3.2 percent of ratio, and usually under 1 percent.** Against
the 2 MiB frame zeekstd defaults to, the 1 MiB frame gives up between 0.0 and 1.4 percentage
points of ratio. It also reads *faster*, in every single entry, because a 1 MiB ranged read
has to decompress at most one 1 MiB frame instead of one 2 or 4 MiB frame.

**What ranged repair pays forever.** Reading one random 1 MiB range from a 1 MiB-framed
seekable object costs **1.30 to 7.91 ms**, against **0.43 to 0.97 ms** from an uncompressed
file. That is a factor of **2.5x to 8.2x**, or an absolute penalty of 0.7 to 6.9 ms per
range. The worst case is the float array, at 7.91 against 0.97.

**Correctness was asserted, not assumed.** For every entry and every frame size, all 16
ranged reads were compared byte-for-byte against the same offsets of the uncompressed
original: **16/16 identical in all 33 configurations, 528 ranges, zero mismatches.** A ratio
from a codec that returned wrong bytes would be worthless, and this one does not.

### The compressibility probe

Compress a small sample at level 1 and store raw below a threshold. The question is whether
the sample predicts the whole object. Full table in `data/exp1_probe.json`.

| sample | false positives at 1.05 | false negatives | at 1.10 FP | FN | at 1.20 FP | FN |
|---|---|---|---|---|---|---|
| 64 KiB head | 2 | 0 | 3 | 0 | 2 | 0 |
| 256 KiB head | 1 | 0 | 2 | 0 | 0 | 1 |
| **1 MiB head** | 0 | 1 | **0** | **0** | 0 | 1 |
| 4 MiB head | 0 | 0 | 0 | 0 | 0 | 1 |
| 4 MiB spread | 0 | 0 | 0 | 0 | 0 | 1 |

**A cheap probe does predict reliably, but not as cheaply as proposed.** A 1 MiB head at
level 1 with a 1.10 threshold classifies **all 23 entries correctly, zero false positives and
zero false negatives.** A 64 KiB head does not: it misclassifies 2 to 3 entries at every
threshold.

**Why the small samples fail, which is the useful part.** They read headers, and headers
compress when bodies do not. The NetCDF4 file's first 64 KiB compresses at 1.644 while the
whole object compresses at 1.004, because the head is HDF5 metadata and the body is already
zlib-compressed. The photo tar's first 64 KiB reads 1.221 against a true 1.006, because it is
tar headers and JPEG headers. Both are false positives that would spend CPU compressing
incompressible data.

**The opposite error also exists and bounds the threshold.** The probe *under*-predicts
archives: the kernel tar's 1 MiB head reads 4.374 against a true 6.807, because the head of a
tar is a run of small text files and the body is not representative. That is harmless at a
1.10 threshold, and it is why the threshold must stay low: at 1.20 the 1 MiB head starts
producing a false negative.

**Spreading the sample does not pay for itself.** The 4 MiB spread sample, four reads from
across the object, is no more accurate than the 1 MiB head at the 1.05 and 1.10 thresholds,
and it costs four seeks on a cold object instead of a sequential read the transfer is doing
anyway.

### Net effect on the whole corpus

Applying the policy, probe with a 1 MiB head at level 1 and store raw below 1.10, to the 18
entries that are not the second half of a versioned pair, using each entry's **measured
whole-object zstd-3 ratio**:

| | value |
|---|---|
| corpus, stored raw | 9,465,540,244 bytes |
| corpus, policy applied | 3,310,756,866 bytes |
| **saving** | **65.0 percent** |
| entries the probe stores raw | **9 of 18** |
| entries the probe compresses | 9 of 18 |

Stored raw: both `.pkg.tar.zst` packages, the `.tar.gz`, all three Parquet files, the photo
tar, the NetCDF4 file and the FLAC tar. Compressed: enwik8, silesia, both GTFs, both cDNA
FASTAs, the kernel tar, the primary assembly, the float array and the WAV tar.

**This figure uses zstd-3 whole-object ratios while the recommended policy is zstd-1, so it
overstates the saving slightly.** An end-to-end `net` stage that applies the policy at level 1
and measures its real CPU seconds was written (`exp1_compress.exe net`) but **not run**, because
the machine time was redirected to experiment 4. Running it is a five-minute job and it is the
one number in this section that is composed rather than measured end to end.

The CPU price is the pipeline column above: **3.96 to 8.23 CPU-seconds per GiB** at level 1
for the entries that are compressed, and only the 1 MiB probe for the 9 stored raw.

### Does the volume already compress?

Checked, because compressing a second time spends CPU for nothing.
`FILE_ATTRIBUTE_COMPRESSED` is **not** set on files created under `work/` on `D:`
(attributes `0x20`, `FILE_ATTRIBUTE_ARCHIVE` only), so NTFS compression is off here and the
measurements above are not double-compressed. The check is one `GetFileAttributes` on a file
the cache creates anyway, so it is free, and `contracts.md` already has the right home for
it: the platform capability list that reports clone support, sparse support and on-access
scanners.

### Decision

**Compress by default, at level 1 only, behind a probe.** Level 1 is the only level that
keeps the whole pipeline ahead of a gigabit link on every corpus regime (124-259 MiB/s
against 119 MiB/s); level 3 falls behind on float arrays and level 6 falls behind on
three of four. **Frame size 1 MiB**, matching the outboard chunk group, the pack threshold
and the streaming buffer, because it costs at most 3.2 percent of ratio against unframed
zstd, at most 1.4 points against zeekstd's 2 MiB default, and it reads ranges faster than
either. **Probe with a 1 MiB head at level 1 and store raw below ratio 1.10**, which
classified 23 of 23 entries correctly where a 64 KiB head misclassified up to 3.
**Byte-shuffle float arrays at stride 4 before compressing**, worth 62 percent on the one
float entry. The user pays **3.96 to 8.23 CPU-seconds per gigabyte** for compressed objects
and effectively nothing for the ones the probe stores raw.

### Confidence

**High** for the pipeline decision, the frame size and the probe threshold: each is a wide
margin over 7 runs, and the probe result is 23 for 23. **High** for correctness, which is
528 byte-exact range comparisons.

**Medium** for the net-effect figure, because it composes measured whole-object ratios rather
than being a single end-to-end run of the policy over the corpus, and because the corpus
composition is a choice rather than a sample of what users fetch.

**Low** for the level inversions, which are reproducible but unexplained.

What would raise confidence: running the policy end to end over the corpus and weighing it
against a real distribution of what users actually fetch, which this spike has no data for.

## Experiment 2. Chunk deduplication

**Question.** Does content-defined chunking save enough on this workload to justify turning
objects into recipes of chunks?

**Method.** `src/bin/exp2_dedup.rs`, FastCDC v2020 (`fastcdc` 5.0.0, normalization level 1)
at 8, 16 and 64 KiB averages with min = avg/4 and max = avg×4, chunks identified by the first
128 bits of their BLAKE3. Dedup measured at four scopes, and the versioned pairs measured
twice, once on the uncompressed release and once on the published `.tar.gz` of the same
release. Raw output in `data/exp2_dedup.json`.

### The headline: compression destroys chunk-level similarity

This is the claim under test, proven with the genuine versioned pairs, side by side.

| pair | input | 8 KiB | 16 KiB | 64 KiB |
|---|---|---|---|---|
| linux 6.6.1 → 6.6.2, pair saving | **uncompressed `.tar`** | **35.87%** | **28.83%** | **17.69%** |
| linux 6.6.1 → 6.6.2, pair saving | **published `.tar.gz`** | **0.00%** | **0.00%** | **0.00%** |
| linux, of 6.6.2 already present | uncompressed | 63.80% | 51.92% | 32.93% |
| linux, of 6.6.2 already present | `.tar.gz` | 0.01% | 0.00% | 0.00% |
| Ensembl GTF 113 → 114, pair saving | uncompressed | 32.78% | 26.43% | 7.89% |
| Ensembl GTF 113 → 114, pair saving | published `.gz` | 0.02% | 0.00% | 0.00% |
| Ensembl GTF, of 114 already present | uncompressed | 65.48% | 52.79% | 15.75% |
| Ensembl GTF, of 114 already present | published `.gz` | 0.03% | 0.00% | 0.00% |
| Ensembl cDNA 113 → 114, pair saving | uncompressed | 23.99% | 14.52% | 3.38% |

**Proven, not asserted.** Two genuine consecutive-release pairs, each fetched separately from
its publisher, go from 32-36 percent saving uncompressed to **0.00 percent** as the exact
`.tar.gz` and `.gz` those publishers actually serve. Deflate's rolling window means one
changed byte early in the stream re-codes everything after it, so no chunk boundary survives.
This is the whole question for Fetchloom, which fetches opaque compressed archives from
arbitrary servers rather than controlling both ends the way HuggingFace's Xet does.

### The append-only ceiling

The synthesised append-only pair, labelled synthesised, exists to measure CDC's best case,
which none of the genuine pairs occupies.

| pair | 8 KiB | 16 KiB | 64 KiB |
|---|---|---|---|
| append-only, pair saving | 44.44% | 44.44% | 44.44% |
| append-only, of B already present | 80.00% | 80.00% | 79.99% |

CDC recovers **80 percent** of the second snapshot and is completely insensitive to chunk
size, which is the signature of an append: every boundary before the append point is
untouched. The genuine edit-in-place pairs recover 52 percent at the same 16 KiB setting and
degrade sharply with chunk size. So 80 percent is the ceiling and roughly 52 percent is the
typical case, and only for uncompressed inputs.

### The other three scopes

| scope | 8 KiB | 16 KiB | 64 KiB |
|---|---|---|---|
| within one entry, best case (chr1 FASTA) | 6.42% | - | - |
| within one entry, every other entry | 0.00-0.21% | 0.00-0.07% | 0.00% |
| across entries of one kind, scientific | 29.63% | 22.97% | 6.62% |
| across entries of one kind, manysmall | 35.87% | 28.83% | 17.69% |
| across entries of one kind, text / columnar / audio / precompressed | 0.00-0.07% | 0.00-0.01% | 0.00% |
| whole corpus, **including** both halves of every versioned pair | 36.50% | 33.34% | 26.68% |
| whole corpus, **excluding** the second half of each pair | **5.39%** | **5.00%** | **4.34%** |

**The two whole-corpus rows are the decision.** The 36.50 percent figure is an artefact of a
corpus deliberately built to contain four versioned pairs; it measures the corpus, not the
workload. Excluding the redundant halves, deduplication across 18 genuinely distinct real
objects is **4.34 to 5.39 percent**. The same-kind rows say where even that comes from: it is
entirely the scientific and many-files groups, which contain the pair halves. Text, columnar,
audio and already-compressed content dedup at **zero**.

**Within one object there is essentially nothing.** Every entry except one is at or below
0.21 percent. The exception, human chromosome 1 at 6.42 percent, is runs of unsequenced `N`
bases, which any compressor already removes for free: zstd-1 gets ratio 3.287 on it.

### What dedup costs

| average chunk | chunking throughput | corpus chunks | flat index bytes | lookup |
|---|---|---|---|---|
| 8 KiB | 404-812 MiB/s, ~700 typical | 1,291,468 | 36,161,104 | 17.5 ns |
| 16 KiB | 636-812 MiB/s | 635,545 | 17,795,260 | 20.6 ns |
| 64 KiB | 417-815 MiB/s | 148,383 | 4,154,724 | 19.4 ns |

Chunking runs at about 700 MiB/s, which is **twice the whole hash-and-write pipeline
measured in experiment 1** (354 MiB/s), so CDC would not become the bottleneck. The index is
small and lookup is free. Cost is not the reason to decline this.

### The security dimension

**Keying destroys cross-user dedup completely.** Chunking byte-identical content with two
different FastCDC seeds produced **0.00 percent** deduplication between them, against 50.00
percent for the same seed, on all four entries tested (enwik8, Parquet, photos, float array).
There is no partial credit: a per-user key means a shared cache holds a full private copy per
user, so keying and cross-user dedup are mutually exclusive, not a trade-off to tune.

**An unkeyed shared index does leak, and the oracle is exact.** Chunking a candidate object
with the public parameters and testing its chunks against the shared index gives **100.00
percent hits when another user has cached that object and 0.00 percent when they have not**,
across 5,015 chunks. There is no ambiguity to hide behind: one lookup answers "has anyone on
this machine fetched this object". `contracts.md` supports a cache shared between users on
one machine, so this is a real exposure, and it is why Borg keys its chunker.

### Counter-experiment

The regime where CDC should look good was measured and it does: append-only recovers 80
percent, and uncompressed consecutive releases recover 52 percent at 16 KiB. The regime where
it should look bad was measured and it does: the same releases as published `.tar.gz` recover
0.00 percent. The result is not one-directional, and the two halves were measured with the
same code on the same day.

### Decision

**Do not build chunk deduplication.** On this workload it saves **4.34 to 5.39 percent**
across genuinely distinct objects, and **0.00 percent** between consecutive releases of the
same dataset in the compressed form publishers actually serve, which is the case Fetchloom
exists for. The 28-36 percent that CDC does deliver requires the user to fetch the same
dataset twice, uncompressed, which Fetchloom cannot arrange because it does not control the
publisher.

**If it is ever built, the terms are fixed by these numbers.** 16 KiB average, because 8 KiB
buys 0.4 points of corpus saving for twice the index; **keyed per cache, not per user**,
because per-user keying gives 0.00 percent cross-user dedup while unkeyed gives an exact
existence oracle against other users' content; and a chunked object still satisfies the
whole-object invariant only if its whole-object digest is verified after reassembly and
before publication, because chunk digests alone prove the parts and not the order.

**The one thing worth taking from this experiment now** is not chunking but the observation
underneath it: Fetchloom sees zero version-to-version similarity because publishers serve
compressed archives. Any future work on incremental fetch should target getting the
uncompressed bytes, not chunking the compressed ones.

### Confidence

**High** for the compressed-versus-uncompressed result, which is the headline: two genuine
independently published pairs, three chunk sizes, and the gap is 36 percent against 0.00
percent rather than a margin needing statistics.

**High** for the keying and leak results, which are exact by construction: 0.00 and 100.00
percent with no spread.

**Medium** for the 4.34-5.39 percent corpus figure, because it is a property of this corpus.
A corpus with more near-duplicate content would raise it and one with more compressed
archives would lower it, and this spike has no data on what users actually fetch.

What would raise confidence: a real distribution of what Fetchloom users fetch, and whether
they refetch successive releases at all. If they do so routinely and can obtain uncompressed
forms, the decision flips.

## Experiment 3. Storage layout: whole archives or extracted members

**Status: stopped before completion, deliberately, and not by me.** Partway through the run
the question was settled on design grounds rather than benchmark grounds, and the measurement
was cut to pricing. What follows separates what was decided from what was measured, because
they are not the same thing and the difference matters to anyone reading this later.

### The decision, and it is not mine

Keep the whole archive. Three reasons, none of which a throughput number can overturn:

1. **The publisher's stated digest is over the archive bytes.** It is the only independent
   evidence in the system, the only claim about the data that does not originate inside
   Fetchloom. Dropping the archive destroys it permanently and irreversibly, and
   `vision.md`'s stated user includes reproducibility reviewers who need to prove a tree
   years after the fact.
2. **Localized repair depends on the archive and its outboard tree.** A member layout means
   refetching a whole archive to recover one file, and a gzip stream cannot be seeked into at
   all. That is roadmap phase 5's exit criterion, given up for a storage saving.
3. **The cross-dataset dedup argument was already answered by experiment 2** in this same
   spike: 5.39 percent at an 8 KiB average across seventeen unrelated datasets once the
   versioned pairs are excluded. The member layout's best argument is worth five percent.

### The pricing, which is what was actually wanted

Taken from the corpus manifest, which is measured data, not from the interrupted run:

| quantity | value |
|---|---|
| `linux-6.6.1.tar.gz`, the archive | 224,925,704 bytes |
| the same tree extracted | 1,393,614,166 bytes in 87,703 files |
| extraction multiple | **6.20x** |
| **keeping the `.tar.gz` beside the tree** | **16.1 percent on top of the tree** |
| keeping an uncompressed `.tar` beside the tree | 101.8 percent, which is why the archive is kept in its published form |
| `silesia.tar` extracted | 1.00x, an uncompressed tar expands to itself |

**The number for the docs: keeping a compressed archive costs about 16 percent on top of the
extracted tree, and buys the publisher's digest and localized repair.** The brief's framing,
a 10 GB archive extracting to 30 GB, is a 3.3x expansion; the kernel tarball is 6.2x, so the
archive overhead is 16 percent rather than 33. The multiple is a property of the archive's own
compression ratio, so it is bounded by that and never worse than 100 percent.

### What was measured before the run was stopped

Nothing that survived. The run wrote its results only at the end and was killed at 59 minutes,
during the fourth of seven member-layout materialisations, so `data/exp3_layout.json` does not
exist. Two observations from the run are worth recording anyway because they cost nothing and
are facts about the platform:

- **Materialising 87,703 members individually ran at 92 files per second** on this machine,
  measured directly by counting the destination during the run. That is about 16 minutes per
  materialisation, against a single archive extraction of the same tree. It is a real number
  but an unfair one, for the reason in the next paragraph.
- **The member path attempted a reflink on every file before falling back to a copy**, and on
  NTFS the reflink can never succeed, so that is 87,703 guaranteed-failed syscalls folded into
  the timing. The reflink correction the brief asked for was not run, because the question was
  cut before its turn came. **So the 92 files per second figure is an upper bound on the cost
  and must not be quoted as the member layout's true cost.** I am recording it as
  contaminated rather than presenting it as a result.

### Reflinks

**Not measured, and it cannot be measured on this machine as configured.** `D:` is NTFS,
which has no reflink; the `reflink_probe` in `exp3_layout.rs` was written to record the exact
error but the run never reached its output. A volume that supports reflinks would mean either
ReFS, which this machine does not have, or a Linux volume with btrfs or XFS with `reflink=1`,
which would mean a privileged Docker container with a loopback filesystem. That is a real
experiment and it was not run. **What it would need: a second machine or a privileged
container with a btrfs or XFS reflink volume, and the same materialisation driven on both.**

### Decision

Keep the whole archive, on the three design grounds above. State the storage cost in the docs
as **about 16 percent on top of the extracted tree** for a compressed archive, and note that
the figure is bounded by the archive's own compression ratio.

### Confidence

**High** for the storage multiple, which is arithmetic over measured byte counts.
**None** for the materialisation timings, which were not completed and whose partial numbers
are contaminated by a per-file failed reflink. **None** for the reflink comparison, which was
not run and needs hardware this machine does not have.

## Experiment 4. Concurrency and splitting

**Question.** Fetchloom caps global in-flight transfers at 8, starts per-host at 2, ceilings
per-host at 4, and refuses to split an object below 64 MiB. hf_xet scales to 64 streams;
aria2 defaults to 4 per server with a 10 MB minimum split. Are our numbers right?

**Method.** `bin/exp4_sweep.sh` and `bin/exp4_local.sh`, driving the `ureq` client configured
exactly as `crates/sources/src/http.rs` configures it. Three real hosts of different
character, plus a local fault server (`src/bin/faultserver.rs`, HTTP/1.1 with ranges and
keep-alive) so the network's own variance can be separated from a host's behaviour. 7 runs
per point. Raw output in `data/exp4_raw.jsonl`.

**Politeness was enforced in code, not intended.** Every point records every non-2xx, every
`Retry-After` and every transport error, and a host is dropped from the sweep the moment it
returns anything that is not 200 or 206. The global-ceiling sweep was run against the local
server on purpose: realising a global concurrency of 64 across three reachable hosts would
mean 21 streams per host, which is exactly the hammering this experiment refuses to do.

### Politeness evidence first, because it bounds everything else

**Across the entire experiment, one request out of roughly 1,400 was not a 200 or a 206.**
No host returned 429. No host returned 503. No host sent `Retry-After`. No connection was
reset by a peer.

| host | what happened | count |
|---|---|---|
| object store (`noaa-ghcn-pds.s3.amazonaws.com`) | dropped from the sweep | 1 |

The single event was `{"status": 0, "secs": 21.70, "retry_after": null}` at **per-host
concurrency 1** — a transport stall of 21.7 seconds on a single stream, not a rate limit.
Reading it honestly: **that is an unreliable path, not a server objecting**, and it happened
at the lowest concurrency tested, where hammering is not a possible explanation. My abort rule
treated it as an objection and dropped the host, which cost me the object store's data. The
rule was too blunt — it should distinguish a 429 or 503, which is a server pushing back, from
a transport timeout, which is not. **I found no configuration that was faster because it
hammered a server, because no server ever pushed back at any concurrency I tested.**

### Per-host concurrency, real hosts

| host | per-host | runs | MiB/s min | median | max | IQR | median request latency ms |
|---|---|---|---|---|---|---|---|
| academic (`ftp.ensembl.org`) | 1 | 7 | 0.8 | **1.6** | 1.9 | 0.9 | 483 |
| academic | 2 | 7 | 1.7 | **2.5** | 2.7 | 0.8 | 675 |
| academic | 4 | 7 | 0.9 | **2.0** | 2.6 | 1.2 | 1631 |
| academic | 8 | 7 | 0.0 | **2.1** | 2.8 | 2.6 | 2690 |
| academic | 16 | 7 | 1.4 | **2.3** | 2.9 | 0.4 | 2868 |
| CDN (`cdn.kernel.org`) | 1 | 7 | 5.2 | **5.7** | 5.9 | 0.5 | 173 |
| CDN | 2 | 7 | 5.1 | **5.8** | 6.0 | 0.5 | 339 |
| CDN | 4 | 7 | 5.1 | **5.7** | 6.1 | 0.8 | 695 |
| CDN | 8 | 7 | 3.3 | **5.6** | 6.3 | 1.6 | 1334 |
| CDN | 16 | 7 | 0.3 | **3.7** | 4.2 | 0.2 | 4135 |
| object store | 1 | 4 | 0.4 | **0.5** | 1.4 | 1.0 | 1344 |

**Two different shapes, and the difference is the result.**

On the **CDN**, throughput is flat from 1 to 8 streams (5.7, 5.8, 5.7, 5.6) while per-request
latency scales almost exactly linearly (173, 339, 695, 1334 ms — each a clean doubling). Flat
throughput with linearly growing latency is the signature of a fixed-capacity resource being
queued against. Concurrency buys nothing here; it only makes each request wait longer. At 16
streams there is a genuine **cliff**: median falls to 3.7 MiB/s and one run reached 0.3.

On the **academic mirror**, which is the host character Fetchloom's users actually hit,
concurrency 2 is a real gain: **1.6 to 2.5 MiB/s, a 1.56x improvement**. Beyond 2 it does not
improve and the spread explodes — at concurrency 8 the interquartile range is 2.6 MiB/s on a
2.1 median, with one run at 0.0. **Per-host 2 is the only setting that measurably helped any
real host, and nothing above it helped anything.**

### The control: is that the network, or is it us?

The flat CDN curve has two possible causes with opposite implications: this machine's access
link, or `ureq`'s connection pool serialising. Same sweep, local fault server, network removed:

| per-host | runs | MiB/s min | median | max | IQR | median latency ms |
|---|---|---|---|---|---|---|
| 1 | 7 | 204.0 | **526.3** | 604.5 | 222.2 | 1.8 |
| 2 | 7 | 681.3 | **714.3** | 874.1 | 80.0 | 2.4 |
| 4 | 7 | 521.8 | **1137.0** | 1645.2 | 238.3 | 3.1 |
| 8 | 7 | 1362.8 | **1506.2** | 1865.7 | 360.1 | 4.6 |
| 16 | 7 | 965.7 | **1466.7** | 2002.0 | 355.0 | 7.5 |
| 32 | 7 | 715.9 | **1505.6** | 2255.7 | 679.9 | 9.6 |

**The client scales: 526 to 1,506 MiB/s from 1 to 8 streams, a 2.9x gain, saturating at 8.**
So `ureq` and the connection pool are not the constraint, and the flat real-host curves are
the network, not us. That validates the real-host numbers rather than invalidating them —
which is what this control existed to decide. Above 8, local throughput stops improving and
the spread widens sharply (IQR 680 at 32), so 8 concurrent streams saturates even a loopback
server with no latency at all.

### The global ceiling

**Machine condition for this table and the local per-host control, stated because it is not
ideal.** An earlier attempt at this sweep had a bug in its own driver — a bare `wait` that
also waited on the fault servers, which never exit — and it was killed. Sixteen fault-server
processes from that attempt were **still resident** while this table was measured, idle with
no client connected, so they consumed memory but no CPU. Free memory on this 16 GiB machine
was around 3 GiB at the time. The curve below is clean and monotone with non-overlapping
interquartile ranges, so I do not think it was distorted, but the run was not taken on a
quiet machine and the absolute MiB/s figures should be read as a lower bound rather than as
this machine's capability.


Realised as *k* distinct local hosts at a fixed per-host of 4, so global = 4k. This is the
hf_xet question directly: does raising the global ceiling help once there are enough hosts to
spend it on?

| global in flight | hosts | runs | MiB/s min | median | max | IQR | vs global=4 |
|---|---|---|---|---|---|---|---|
| 4 | 1 | 7 | 67.7 | **72.1** | 73.9 | 5.4 | 1.00x |
| 8 | 2 | 7 | 97.4 | **122.4** | 124.6 | 5.3 | 1.70x |
| 16 | 4 | 7 | 159.3 | **192.3** | 199.1 | 22.3 | 2.67x |
| 32 | 8 | 7 | 264.3 | **283.9** | 294.5 | 13.3 | 3.94x |
| 64 | 16 | 7 | 323.8 | **339.2** | 361.1 | 19.4 | 4.71x |

**Throughput rises all the way to 64 and had not stopped rising.** Our ceiling of 8 delivers
1.70x; 16 delivers 2.67x, 32 delivers 3.94x, 64 delivers 4.71x. **The current global ceiling
of 8 leaves roughly 2.8x on the table when a run has many hosts to spread across.**

The scaling is strongly sub-linear — 16x the hosts for 4.7x the throughput — so the returns
are diminishing, and this is a loopback server on a 12-core machine, which is the friendliest
possible case. But the direction is unambiguous and the interquartile ranges do not overlap
between any two adjacent settings.

### The split threshold

| host | span | conns | runs | MiB/s min | median | max | IQR | gain from splitting into 4 |
|---|---|---|---|---|---|---|---|---|
| CDN | 1 MiB | 1 | 7 | 1.7 | **3.3** | 4.2 | 1.9 | |
| CDN | 1 MiB | 4 | 7 | 3.1 | **3.9** | 4.3 | 0.8 | **1.16x** |
| CDN | 10 MiB | 1 | 7 | 3.0 | **3.9** | 4.4 | 0.9 | |
| CDN | 10 MiB | 4 | 7 | 3.3 | **4.1** | 4.4 | 0.3 | **1.06x** |
| CDN | 64 MiB | 1 | 7 | 3.6 | **4.4** | 4.7 | 0.7 | |
| CDN | 64 MiB | 4 | 7 | 0.5 | **1.7** | 7.0 | 2.9 | **0.38x** |
| CDN | 256 MiB | 1 | 3 | 1.1 | **1.2** | 1.3 | 0.2 | |
| CDN | 256 MiB | 4 | 2 | 1.6 | **1.6** | 1.6 | 0.0 | **1.34x** |

**Splitting never bought much, and at 64 MiB it cost 2.6x.** The 64 MiB row is the one the
current contract is built on, and it is the worst row in the table: median 4.4 MiB/s
unsplit against 1.7 MiB/s split, with the split runs ranging from 0.5 to 7.0. That spread is
the finding — splitting at this size did not reliably do anything except add variance.

**These rows are the weakest in the experiment and should not on their own move the
threshold.** The 256 MiB points have 3 and 2 runs rather than 7, the sweep was stopped before
the object store repeated it, and the whole table was taken on a link the per-host sweep
already showed to be the binding constraint at about 5.7 MiB/s. On a link that saturates at
one stream, splitting cannot help by construction; it can only add scheduling variance, which
is exactly what the table shows.

### Decision

**Per-host: start at 2, and lower the ceiling from 4 to 2 unless measurement on that host
says otherwise.** Concurrency 2 was the only setting that improved any real host (academic,
1.56x). Concurrency 4 was never better than 2 on any real host and was worse on the academic
mirror. The existing adaptive controller is the right shape; its ceiling is one step too high.

**Global: raise the ceiling from 8 to 32.** Measured 3.94x against 1.70x at the current
ceiling, with non-overlapping interquartile ranges at every step. Not 64: the gain from 32 to
64 is 1.20x for double the sockets, it was measured on loopback rather than against real
hosts, and a ceiling that high is only reachable with 16 hosts in flight, which is a
politeness surface this spike did not test in the wild.

**Split threshold: do not lower it to 10 MiB, and treat 64 MiB as unconfirmed.** aria2's
10 MB default is not supported by anything measured here — splitting at 10 MiB gained 1.06x,
inside the noise. Nothing measured supports lowering the threshold, and the 64 MiB row argues
against splitting at that size at all on a saturated link. The contract's existing four
preconditions, which include a recorded per-host concurrency above one, already prevent the
harmful case: on a link like this one the controller never records a per-host above 1, so no
split is attempted. **Leave 64 MiB and keep the preconditions.**

### What this experiment could not settle

The measurements that would change these numbers need a fast link. This machine's access link
saturated at about 5.7 MiB/s against the CDN and 2.5 MiB/s against the academic mirror, which
is below the point where per-host concurrency or splitting can demonstrate value. The local
control shows the client is capable of 1,506 MiB/s, so the ceiling questions are answerable —
just not from here.

### Confidence

**High** for the politeness result: 1 non-2xx in ~1,400 requests, no 429 or 503 ever, recorded
per request.

**High** for the client-scaling control (526 to 1,506 MiB/s) and therefore for the claim that
the flat real-host curves are the network rather than `ureq`.

**Medium** for the global ceiling recommendation: the curve is clean and the IQRs do not
overlap, but it is loopback, so it measures how many streams the *client* can usefully drive,
not how many a set of real hosts will tolerate.

**Medium** for per-host 2, which rests on one real host showing a 1.56x gain and two showing
none.

**Low** for the split threshold rows, for the reasons stated above. What would raise it: the
same sweep from a machine with a link faster than about 500 Mbit/s, against the same three
host characters.

### Experiment 4b. Reconcile cost against a large tree

**Question.** An overlay layer is about to be designed on top of reconcile, and reconcile in
this project has been quadratic once already. What shape is its cost curve at 0, 10, 1,000 and
100,000 changed entries?

**The curve was not obtained. The experiment hit a hard failure instead, and the failure is
worth more than the curve would have been.**

`fetchloom get` materialised the tree correctly: **93,182 entries, 1,393,614,166 bytes, in 846
seconds**. Every subsequent `fetchloom verify` of that destination failed, at every overlay
size including **zero changed entries**, with exit code 10 and:

```
manifest.invalid ... shorten the document, because it is 19288996 bytes
                     and the limit is 16777216
```

**Fetchloom can materialise a tree it cannot then verify.** The record `get` writes for this
tree is 19,288,996 bytes; the listing size limit `contracts.md` states in the tuning table is
16 MiB, and `verify` enforces it when reading. The write path and the read path disagree, so
any tree whose record exceeds 16 MiB is materialised successfully and is then permanently
unverifiable. The tuning table's `Manifest node count` of 100,000 suggests trees of this size
are meant to be supported; 93,182 entries is inside that and still produces an unreadable
record.

The seven timings per overlay size in `data/exp4b_reconcile.jsonl` are all 0.44 to 0.68
seconds and are **the time taken to fail**, not the cost of reconcile. They are kept as
evidence of the failure and must not be read as a cost curve.

**Decision.** Reconcile's cost curve is unmeasured and remains an open risk for the overlay
design. Before that design proceeds, the record-size mismatch is a defect to fix in its own
right: either the writer must respect the same limit the reader enforces, or the reader's
limit must not apply to a record Fetchloom itself wrote.

**Confidence: high** that the failure is real and reproducible — it occurred on all 28 runs
across four overlay sizes. **None** for the cost curve, which was not measured.


## Experiment 5. HTTP/2 and HTTP/3

**Status: stopped before the throughput sweep, deliberately.** The decision was taken on
architectural grounds. What was measured before it was stopped is reported; what was not
measured is named.

### Decision

**Do not adopt HTTP/2, and do not adopt an async runtime for it.** Fetchloom's workload is a
small number of large transfers. HTTP/2's advantage is multiplexing many requests over one
connection, which is worth most when a client makes hundreds of small requests to one host and
is worth very little when it makes four large ranged reads. Fetchloom has zero async today: no
tokio, no `async fn`, no `.await`, and blocking threads with a measured budget. Adopting
HTTP/2 means hyper or reqwest and tokio, because `ureq` 3.x states its goal as a synchronous
HTTP/1.1 client with minimum dependencies and its HTTP/2 issue has been open since 2020. The
cost is roughly double the dependency graph, an async runtime in a codebase built around
blocking threads, and a rewrite of the Source seam that every other adapter is built against.
The gain, on this workload, is multiplexing that a few large transfers barely use. **A
well-argued no.**

### What was measured

The dependency cost is measured, not estimated. Each client is a separate workspace with its
own lockfile, which is what makes this a count rather than a guess.

| option | crates in lock |
|---|---|
| `ureq` HTTP/1.1, configured exactly as `crates/sources/src/http.rs` does | **80** |
| `reqwest` + tokio, HTTP/2 | **176** |
| `reqwest` + tokio + quinn, HTTP/3 | **181** |
| Fetchloom's whole main workspace today, for scale | 161 |

An HTTP/2 client alone would carry **more crates than the entire Fetchloom workspace carries
today**, 176 against 161, to replace one that carries 80.

**HTTP/3 works, and advertisement is not availability.** Both facts were measured with a real
client before the experiment was stopped. `cloudflare-quic.com` negotiated `HTTP/3.0`
successfully. `cdn.kernel.org` advertises `h3` in its `alt-svc` header
(`h3=":443"; ma=86400, h3-29=..., h3-27=...`) and its QUIC handshake **timed out** from this
network. An earlier attempt without an explicit version on the request silently fell back to
HTTP/1.1 while appearing to succeed, which is exactly the failure mode that produces a number
that looks like a result and is not one. That is the case for the ALPN pre-check the brief
asked for, and it is why `bin/alpn_probe.sh` exists.

**HTTP/2 is negotiated by the hosts tested.** `cdn.kernel.org` negotiated `HTTP/2.0` when the
client was free to choose. The system `curl` on this machine cannot probe this at all: it is
built against Schannel with no nghttp2 and no HTTP/3, so it reports an empty version for both.
Protocol support had to be determined with the Rust clients.

### What was not measured, and would be needed to overturn this

The throughput comparison. Two smoke measurements exist in the transcript, HTTP/1.1 at
1.02 MiB/s and HTTP/2 at 4.23 MiB/s against `cdn.kernel.org`, and **they are discarded, not
reported as a result**: both were taken while the corpus was still downloading and saturating
the link, so they measure contention, not protocol. Presenting them would be exactly the error
this spike is supposed to avoid.

To overturn the decision, someone would need to show a large multi-host throughput gain on a
quiet link against academic and government hosts specifically, since those are the hosts
Fetchloom's users actually hit, and then weigh it against 96 extra crates and an async runtime.

### Confidence

**High** for the dependency counts, which are read off three real lockfiles.
**High** for the protocol availability findings, which came from a working client.
**Not applicable** for throughput, which was deliberately not measured.

## Experiment 6. FTP and SFTP

**Question.** Can these sit behind the existing blocking Source seam, and which resume rung
can each honestly claim?

**Method.** `spike/ftp`, a separate workspace, driving `suppaftp` 11 and `russh-sftp` 2
against four real servers. Every capability the Source seam requires was exercised for
real, and the resumed tail was compared byte-for-byte against the corresponding slice of
the whole object. Raw output in `data/exp6_ftp_sftp.json`.

| capability | ftp.gnu.org | ftp.ensembl.org | test.rebex.net | demo.wftpserver.com |
|---|---|---|---|---|
| protocol | FTP | FTP | SFTP | SFTP |
| bounded metadata probe | `SIZE` → 1,033,297 | `SIZE` → 114 | `stat` → 379 | `stat` → 127,734 |
| stated length before transfer | yes | yes | yes | yes |
| modification time | `MDTM` 2022-05-29 | `MDTM` 2026-08-14 | mtime 1695121923 | mtime 1781761297 |
| entity tag | **none** | **none** | **none** | **none** |
| immutable identity | **none** | **none** | **none** | **none** |
| directory listing | 45 entries | 4 entries | 2 entries | 15 entries |
| whole fetch | ok | ok | ok | ok |
| offset resume | `REST` ok | `REST` ok | seek+read ok | seek+read ok |
| **resumed tail byte-identical** | **yes** | **yes** | **yes** | **yes** |
| `FEAT` advertises `REST STREAM` | yes | yes | n/a | n/a |

**Counter-experiment.** The resume was not merely "did not error": the returned tail was
compared against the same offset range of the separately fetched whole object on all four
servers, and matched in all four. A resume that returns the wrong bytes would have shown up
here.

**Resume rung.** Both protocols supply a length and a modification time, and neither
supplies an entity tag or an immutable content address. That is the definition of rung 4,
weak validator only: resume, full verify at completion, quarantine on mismatch. Neither can
honestly claim rung 3, because rung 3 requires a strong entity tag and `If-Range`, and
neither protocol has the concept. Rung 2 is unreachable for the same reason. Rung 1 remains
available exactly as it is for HTTP, because rung 1 depends on Fetchloom's own outboard
tree for the expected digest and not on anything the source says.

**Can it be done without async?** For FTP, **yes**. `suppaftp` is a synchronous client and
every probe above ran on a blocking thread with no runtime. For SFTP, **no** with the crate
the brief names: `russh-sftp` is built on `russh`, which is a tokio client, and the SFTP
half of this experiment only runs inside `#[tokio::main]`. A synchronous SFTP would mean a
different crate, and the obvious candidate, `ssh2`, is a libssh2 C binding, which trades the
runtime for a C dependency and a build-time toolchain requirement.

**Dependency cost.** The `spike/ftp` workspace, carrying both clients plus tokio, resolves
to **213 crates**. The main workspace's lock today is 161. FTP alone is far cheaper than
that number suggests, because most of the 213 is the SSH and tokio half.

**Decision.** Add FTP behind the existing blocking Source seam; it lands on **rung 4** and
needs no runtime. Do not add SFTP in the same change: it lands on the same rung 4 and buys
nothing extra in resume capability, while costing either an async runtime or a C dependency.
Each degrades to rung 4 and reports it, because neither protocol can state an entity tag or
an immutable identity, and a source that states neither is what rung 4 is for.

**Confidence: high** for the capability matrix, which is byte-verified against four real
servers. **Medium** for the dependency claim, because 213 crates is the cost of both clients
together and the FTP-only figure was not measured separately.

## Experiment 7. The Python integration

**Question.** Does an fsspec adapter actually give Fetchloom the Python data ecosystem?

**Method.** `python/fetchloom_fsspec.py`, an `AbstractFileSystem` registered for the
`fetchloom://` protocol that shells out to the release binary
(`fetchloom get <ref> -o <dir> --json --no-extract`) and returns a normal file object over the
materialised file. Driven by `python/exp7_fsspec.py`, 7 runs per timed point, against the real
NYC TLC Parquet file. Raw output in `data/exp7_fsspec.json`.

**The interpreter.** `python3` is indeed not on this machine's `PATH` under Git Bash — the
WindowsApps shim resolves and then reports "Python was not found". The real interpreter is
`C:\Users\Naveen\AppData\Local\Programs\Python\Python312\python.exe`, reachable as `py -3`.
A virtual environment was created at `spike/.venv` and `fsspec`, `pandas`, `pyarrow` and
`dask[dataframe]` installed into it. Nothing was installed system-wide.

### It works, end to end

| reader | result | median secs (7 runs) |
|---|---|---|
| `pandas.read_parquet("fetchloom://...")` | **2,964,624 rows x 19 columns** | 0.306 |
| the same file read directly off local disk | 2,964,624 rows | 0.310 |
| `pyarrow.dataset` over a `fetchloom://` path | **2,964,624 rows** | 0.198 |
| `pyarrow.ParquetFile` over `fs.open(...)` | **2,964,624 rows** | 0.001 |
| `dask.dataframe.read_parquet("fetchloom://...")` | **2,964,624 rows**, 1 partition | 0.06 |

All four readers returned the same row count from the same object.

### Overhead

**Warm, the adapter costs nothing measurable: 0.306 s median against 0.310 s reading the
identical file directly from local disk, which is -1.3 percent — inside the noise.** That is
the expected result once the object is materialised, because at that point the adapter *is* a
local file read; the interesting number is what it costs to get there.

**Cold, the first access cost 1.324 s**, which is one subprocess launch plus a cache hit for a
47.6 MiB object. Of that, the `fetchloom get` subprocess accounted for 0.747 s. Every
subsequent access in the process was a hit at 0.0 s, because the adapter memoises the
materialised path.

### What fsspec wants that Fetchloom does not expose

This is the useful half, and all three gaps are real.

1. **A size without a fetch.** `fs.info()` returned a correct size of 49,961,641 bytes, but
   only because the adapter had already materialised the whole object. There is no way to ask
   the binary for a length alone. `fetchloom why` explains a reference but does not state a
   content length. fsspec calls `info` constantly, and every call currently means a full
   transfer on a cold cache.
2. **A listing without materialising.** `fs.ls()` on a container **fails**, and the binary's
   own error says why:
   `reference.unresolved ... name the objects instead of the container, because
   https://d37ci6vzurychx.cloudfront.net/trip-data/ answered with an index in no format
   Fetchloom recognizes`. This is correct, deliberate behaviour, but it means `fetchloom://`
   cannot support glob patterns, directory datasets, or partitioned Parquet — which is how
   most real Parquet datasets are published.
3. **File-like random access into a cached object.** This works today only because the adapter
   materialises the whole object to a destination and opens that. `pyarrow.ParquetFile` seeks
   to the footer and reads column chunks; it got 0.001 s medians here purely because the file
   was already on local disk. There is no way to read a byte range out of a cached object
   without writing the whole object somewhere first.

### Decision

**The adapter is viable, and it is worth building.** It works with pandas, pyarrow and dask
unmodified, over a real published dataset, at no measurable warm overhead. Shelling out to
the binary is sufficient for the single-object case.

**What it needs from the binary that does not exist yet, in priority order.** A **command**,
not a flag and not a library, for each of the first two: a metadata probe that states length
and identity without transferring (the Source seam already performs exactly this probe
internally — it is not exposed), and a listing that resolves a container to its objects
without materialising them. The third, ranged read out of a cached object, is a **library**
concern and is the one that would require real design, because it is the same capability that
experiment 1's seekable frames provide and it should be built once for both.

**A flag is not enough for any of the three**, because each is a different question than "give
me these bytes", which is the only question the CLI currently answers.

### Confidence

**High.** Four independent readers from three libraries returned identical row counts from a
real published file, the overhead comparison is 7 runs against a local-disk control, and the
three gaps are each demonstrated by a specific call — two by measurement and one by the
binary's own error message.

## Experiment 8. Is the CPU work already optimal

**Question.** Is SHA-256 hardware acceleration actually being used here, what is it worth,
and what does the interop digest cost?

**Method.** `src/bin/exp8_hash.rs`, 9 timed runs per point, two independent executions of
the whole matrix. The software baseline is a second binary built with
`RUSTFLAGS='--cfg sha2_backend="soft"'`, which is the switch `sha2` 0.11 itself exposes; the
default build selects SHA-NI at runtime through `cpufeatures`. Raw output in
`data/exp8_hash_{hw,soft}_run{A,B}.json`.

Runtime feature detection on this CPU: `sha_ni: true`, `avx2: true`, `avx512f: false`.

Throughput in MiB/s, mean of the two executions' medians:

| buffer | SHA-256 hw | SHA-256 soft | hw speedup | BLAKE3 1 thread | BLAKE3 rayon | combined serial | combined 2 threads |
|---|---|---|---|---|---|---|---|
| 64 KiB | 1437 | 145 | 9.9x | 2992 | 1865 | 701 | 119 |
| 256 KiB | 1308 | 237 | 5.5x | 3228 | 2089 | 954 | 409 |
| 1 MiB | 1429 | 295 | 4.8x | 3180 | 6735 | 969 | 837 |
| 4 MiB | 1425 | 274 | 5.2x | 3210 | 10314 | 1105 | 766 |
| 16 MiB | 1315 | 259 | 5.1x | 2838 | 6679 | 1162 | 985 |
| 64 MiB | 1292 | 129 | 10.0x | 2605 | 6845 | 1163 | 1124 |
| 256 MiB | 622 | 112 | 5.5x | 1339 | 1372 | 419 | 425 |

Spread at 16 MiB, SHA-256 hardware, per execution:

| execution | min | q1 | median | q3 | max | IQR |
|---|---|---|---|---|---|---|
| A | 1003.9 | 1157.0 | 1291.4 | 1356.7 | 1417.6 | 199.7 |
| B | 1010.2 | 1260.8 | 1339.2 | 1385.7 | 1429.4 | 124.9 |

**Is hardware acceleration being used?** Yes, and it is worth about **5x**. `contracts.md`
says it is detected and reported, and the detection is real: forcing the soft backend costs
a factor of 4.8 to 10 on the same bytes on the same machine.

**Is SHA-256 the bottleneck?** Yes, decisively. BLAKE3 with rayon runs at 6,700-10,300 MiB/s
above 1 MiB. SHA-256 with SHA-NI runs at about 1,300. The combined two-thread pass lands at
985-1,124 MiB/s, which is within a few percent of SHA-256 alone. **The interop digest is not
a tax on the content digest; it is the entire cost of the pass.** Removing BLAKE3 from the
combined pass would save almost nothing; removing SHA-256 would make the pass roughly 5x
faster.

**Cost of the interop digest.** At the combined two-thread median of 1,124 MiB/s at 64 MiB
against BLAKE3-rayon alone at 6,845 MiB/s: hashing one GiB costs **0.91 CPU-seconds** with
interop and **0.15 CPU-seconds** without. The interop digest therefore costs about
**0.76 CPU-seconds per GiB**, and that is the number the project can state honestly.

**The parallel threshold.** Fetchloom goes multi-threaded on BLAKE3 above 1 MiB. The sweep
says that is right, and slightly conservative. Below 1 MiB, rayon is *slower* than a single
thread (1,865 vs 2,992 MiB/s at 64 KiB; 2,089 vs 3,228 at 256 KiB) because the fan-out costs
more than it saves. At 1 MiB rayon is already ahead, 6,735 vs 3,180. The crossover is
between 256 KiB and 1 MiB, so a 1 MiB threshold sits just past it and never picks the losing
side.

**The two-thread split has its own threshold, and it is not set.** The combined *threaded*
pass is much worse than the combined *serial* pass at small sizes: 119 vs 701 MiB/s at
64 KiB, 409 vs 954 at 256 KiB. It only wins from about 16 MiB. `docs/standards.md` states
flatly that "the interop digest is computed on a separate thread from the content digest so
neither serializes the other", with no size condition. For objects below roughly 4 MiB that
rule makes hashing **up to 5.9x slower**, not faster. This is the one CPU change the numbers
justify.

**Counter-experiment.** The regime where the answer should reverse is small buffers, and it
does reverse: the threading that wins by 1,124 vs 1,163 at 64 MiB loses by 119 vs 701 at
64 KiB. The claim is not one-directional.

**The 256 MiB row.** Every column collapses at 256 MiB: SHA-256 hardware from 1,292 to 622,
BLAKE3 rayon from 6,845 to 1,372. It reproduced across both independent executions (622 and
619 for SHA-256), so it is real and not noise. The buffer is far past this CPU's 18 MB L3,
and the machine had 5.5-6.1 GiB free of 16 GiB, so this is most plausibly memory-bandwidth
and page pressure rather than anything about the hash. **Marked partially UNEXPLAINED:** the
plain bandwidth story does not account for why 64 MiB, also far past L3, holds full speed
while 256 MiB does not. It would take a machine with more free memory, or a hardware counter
profile, to settle it. Nothing in the decision below depends on this row.

**A second UNEXPLAINED.** Software SHA-256 drops from 259 MiB/s at 16 MiB to 129 at 64 MiB
and 112 at 256 MiB, while hardware SHA-256 holds 1,292 at 64 MiB. A slower implementation
should be *less* sensitive to memory bandwidth, not more. Reproduced across both executions
(129 and 130). Not explained. It does not affect the decision, because the software backend
is not a configuration Fetchloom ships.

**Decision.** Keep SHA-256 and keep BLAKE3; hardware acceleration is real, detected, and
worth 5x. Make the two-thread split conditional on object size rather than unconditional,
with the threshold between 4 and 16 MiB, because below it the split costs up to 5.9x. State
the interop digest's price as **0.76 CPU-seconds per GiB**. Keep the BLAKE3 parallel
threshold at 1 MiB.

**Confidence: high** for the SHA-NI speedup, the bottleneck finding and the small-buffer
threading regression, all of which reproduced across two executions with wide margins.
**Low** for the two anomalous rows, which are reproducible but unexplained.

## Experiment 9. Two features that may not be earning their place

### The hint: does it ever fire?

**Question.** Five conditions guard the hint. Across realistic runs, how often is one
actually printed?

**Method.** Instrumenting the conditions from outside is not possible, so the measurement
uses the hint's own side effect: `hint::remember` writes a file under `<cache>/meta/hints`
named for the BLAKE3 of the hint key, and it is called on exactly the code path that printed
a hint. Counting those files counts hints printed, and their names say which hint.

`terminal::hints_permitted` needs stderr **and** stdin to be a terminal and `CI` unset.
`winpty` could not supply a console from this parent process (it aborted with
`ASSERT_CONDITION("wp != nullptr && cols > 0 && rows > 0")`), so runs are launched with
PowerShell `Start-Process`, which gives the child its own console. The real release binary,
a spike-local cache, real corpus files. Raw output in `data/exp9_hint.jsonl`.

| scenario | console | hint printed |
|---|---|---|
| fresh cache, first successful get | yes | **yes** |
| same cache, second successful get | yes | no |
| same cache, 93,182-entry archive | yes | no |
| fresh cache, first get is the 93,182-entry archive | yes | no, and see below |
| fresh cache, stderr piped | no | no |
| fresh cache, `CI=1`, console | yes | no |
| ten consecutive realistic gets on one cache | yes | **1 of 10** |

**15 console runs produced 2 hints. In the ten-run sequence, 1 of 10.**

Every hint file written across every scenario had the same name,
`c47a58fcafa086d2726076c0e9ec7c674283e120ba921778c24e191a96dc5f0c`, which is
`blake3("lock-written")`. **Only one of the five hints ever fired.**

**Why the rate is what it is, from the code.** `Observed::hint` returns at most one hint in
a fixed priority order, and `lock_written` is set by `command/get.rs` on *every* successful
get. It outranks `selection`. `offer_a_hint` in `main.rs` takes that one hint and, if
`already_said`, returns without printing — it does **not** fall through to the next
candidate. So once the lock-written hint has been said for a cache, every later successful
get produces the same suppressed candidate and nothing else can ever be printed. The
selection hint, which needs 200-plus entries taken whole, is therefore **unreachable in
practice**: to reach it a run would have to succeed while writing no lock.

**`restarted_from_zero` is dead code.** `grep` across `crates/` finds it set in exactly two
places, both inside `hint.rs`'s own test module. No production path sets it. Its branch in
`Observed::hint` can never be taken by a real run.

**The one scenario that did not behave as predicted was investigated, not rounded away.**
A fresh cache whose first get is the 87,703-file kernel tarball printed nothing, where the
lock-written hint was expected. The cause is not the hint: the run failed with
`archive.collision`, because `linux-6.6.1/include/uapi/linux/netfilter/xt_connmark.h` and
`xt_CONNMARK.h` cannot both exist on NTFS. Fetchloom refused correctly, no lock was written,
so no hint was due. The experiment was re-run against a case-safe 93,182-entry tar rebuilt
from the extracted tree, and that run behaved as the code predicts.

**Counter-experiment.** The two gates were separated. `CI=1` with a real console suppressed
the hint, and a real console with a fresh cache printed it, so the terminal gate and the
content gate were each shown to be load-bearing rather than one masking the other.

**Decision.** The hint machinery has five branches, of which one fires, once per cache,
ever. Delete `restarted_from_zero`, which no production code sets. Either make
`offer_a_hint` fall through to the next candidate when one has already been said, or delete
`selection`, which cannot otherwise be reached. If neither is done, reduce the feature to
the one hint it actually is.

**Confidence: high.** The rate is measured end to end against the real binary, the identity
of the firing hint is confirmed by digest, and the reachability argument is read directly
off the code.

### Watch: the case for and against

Labelled explicitly as **judgment, not measurement**, as the brief asks. No compute was
spent on it.

**For.** It costs almost nothing. `observer::watch` is 23 lines and adds no dependency, no
module and no new type: it opens a file or stdin, parses each line as the `Event` the
project already defines, and hands it to the `Live` renderer the project already ships for
progress output. Its value is that `--events <path>` is the project's supported way to keep
a record of a run, and a recorded stream nothing can render is a worse feature than one
something can. It also gives long, unattended runs an inspection story that does not involve
scraping progress output, which `--help` explicitly offers as the reason to use it. Removing
it would not shrink the binary meaningfully, because `Live` and `Event` stay for progress.

**Against.** It does not do what its own help text says. The help promises rendering "live
if you point it at a run in progress", but the implementation reads until `read_line`
returns 0 and then stops. Pointed at a file a running process is still appending to, it
renders what exists and exits; it does not follow. Only the `-` form, fed by a pipe, behaves
the way the text describes. Comparable tools set the expectation the text sets: `docker
logs -f`, `kubectl logs -f` and `tail -f` all follow. So the feature as shipped is a replay
tool with a live-tail promise attached, and the honest options are to implement following or
to change the text to say "replay". Against keeping it at all: `jq` over the event file does
most of what replay does, and nothing else in the CLI exists to prettify a file the user
already has.

**Decision.** Keep `watch`, and fix the mismatch rather than the feature: either implement
following for the file form, or restate `--help` as replay-only. It is 23 lines of reuse, so
the cost of keeping it is not the issue; the promise it makes and does not keep is.

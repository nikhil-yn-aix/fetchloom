# measurements

what the linux lab measured for this release, the a/b passes behind its performance decisions, and the historical observations before them.

## final benchmark on the linux lab, 2026-11-09

the [lab](benchmark-design.md) on an azure Standard_NC24ads_A100_v4 (EPYC 7V13, 24 vCPU, nvme, loop-mounted ext4 and xfs), which another user's training job shared. "final" is the static musl build of d2bc3e7, as linux ships it; "main" is the static musl build of d168842, from before these passes. a lan case ran a warmup round and then seven measured ones, every tool once a round in a new random order; on 2026-11-08 the home cases ran three and the lossy s2 cases five, and lossy s1 ran fifteen. a time is the median of the measured rounds, and final/main the median of the paired ratios with its 95% bootstrap interval. the commands, round orders and every run's record stay with the lab's results, outside the repository.

curl, wget and aria2c, each within 17% of fetchloom on some lan case, ran in the same rounds. dvc, datalad and pooch had not changed and were at least 1.25 times slower than fetchloom on every lan case on 2026-11-08, so those runs stand, marked nov 8; main ran in both sessions, and its medians moved by 5.3% at most. the rows marked nov 8 in the first column are from that session too, with e8cff51: what changed since touches the cache, placement and process start, milliseconds against seconds of network time. that build kept transparent huge pages off, so their memory is left out. lossy s1 ran again, every run starting without tcp state from the one before ([scenarios](benchmark-design.md#scenarios)); its final/best interval resamples both tools' runs.

| case | main s | final s | final/main [95%] | best other tool | its s | final/best | peak MiB final, main |
|---|---:|---:|---|---|---:|---|---|
| ext4 lan s1, 1 GiB | 2.444 | 2.304 | 0.939 [0.906, 0.952] | curl | 2.791 | 0.826 | 71, 27 |
| ext4 lan s2, 1,000 files | 0.444 | 0.415 | 0.923 [0.898, 0.962] | curl | 0.484 | 0.858 | 75, 63 |
| ext4 lan s3, 10,000 files | 2.427 | 1.584 | 0.658 [0.639, 0.669] | curl | 1.743 | 0.909 | 138, 117 |
| ext4 lan s4, zip of 10,000 | 0.937 | 0.758 | 0.814 [0.803, 0.821] | curl and unzip | 0.835 | 0.908 | 119, 103 |
| ext4 lan s5, second project | 0.076 | 0.077 | 1.017 [1.011, 1.047] | pooch (nov 8) | 0.280 | 0.275 | 39, 33 |
| ext4 lan s6, no-op | 0.029 | 0.028 | 0.955 [0.944, 0.972] | wget | 0.116 | 0.239 | 23, 23 |
| xfs lan s1, 1 GiB | 2.423 | 2.339 | 0.951 [0.946, 0.965] | curl | 2.641 | 0.886 | 75, 27 |
| xfs lan s2, 1,000 files | 0.532 | 0.415 | 0.781 [0.759, 0.789] | curl | 0.495 | 0.839 | 77, 61 |
| xfs lan s3, 10,000 files | 3.002 | 1.787 | 0.601 [0.593, 0.609] | curl | 1.800 | 0.993 | 152, 115 |
| xfs lan s4, zip of 10,000 | 1.760 | 1.478 | 0.843 [0.831, 0.852] | curl and unzip | 0.849 | 1.741 | 123, 94 |
| xfs lan s5, second project | 0.095 | 0.073 | 0.772 [0.759, 0.798] | pooch (nov 8) | 0.271 | 0.270 | 37, 31 |
| xfs lan s6, no-op | 0.025 | 0.024 | 0.950 [0.931, 1.166] | wget | 0.116 | 0.204 | 23, 23 |
| ext4 home s1, 256 MiB (nov 8) | 23.014 | 23.020 | 0.998 [0.997, 1.000] | wget | 22.928 | 1.004 |  |
| ext4 home s2, 1,000 files (nov 8) | 8.949 | 8.957 | 1.001 [0.995, 1.001] | aria2c | 8.880 | 1.009 |  |
| ext4 home s7, resume (nov 8) | 13.238 | 13.367 | 1.010 [1.010, 1.011] | curl | 13.390 | 0.998 |  |
| ext4 far s1, 4 MiB |  | 17.795 |  | curl | 13.180 | 1.350 [0.694, 2.292] | 28 |
| ext4 far s2, 50 files (nov 8) |  | 5.779 |  | aria2c | 5.648 | 1.023 |  |
| ext4 poor s1, 1 MiB |  | 14.932 |  | wget | 16.994 | 0.879 [0.525, 1.212] | 21 |
| ext4 poor s2, 20 files (nov 8) |  | 8.100 |  | aria2c | 7.437 | 1.089 |  |

fetchloom verifies every downloaded byte, fills a reusable content-addressed cache and places from it; curl writes straight to its target and checks with `sha256sum` afterwards. fetchloom is ahead of curl on every ext4 lan case and level with it on xfs s3. it is behind on xfs s4, a zip of 10,000 members, where xfs's reflink bookkeeping and the lock on each folder's namespace dominate unpacking (see [placing](../architecture/placing.md)). on lossy single files no tool is reliably faster: every tool's 15 runs spread from about 3 to 30 s, by where losses fall in the transfer, and fetchloom's median is the slowest of the four on far s1 and the fastest on poor s1, both well inside their intervals. against main, only ext4 s5 is slower, by 1 ms, within the 2% the a/b rule allows.

peak memory is higher than main's on downloads. file bodies now come over their own HTTP/1.1 connections, up to four a host, which cut far s2 to 0.288 of its time. without HTTP/2's flow control, a lan download arrives faster than SHA-256 hashes it, and the hash queue, bounded at 16 MiB, fills. sent a 256 MiB file over HTTP/1.1 at full speed, main peaks as high: in two runs each, 48 and 60 MiB with transparent huge pages off and 68 and 85 MiB with them on, against 42 and 47, and 70 and 72, for final. huge pages stay on, since turning them off slowed placement and short commands ([placement and startup passes](#ab-passes-on-placement-and-startup-2026-11)).

### scale

one dataset of 100,000 or 1,000,000 files of 2,560 bytes, 1,000 to a folder, from the lab server on the lan profile. rounds alternate final and main: three at 100,000 files and two at 1,000,000. cold fills an empty cache; warm places from a full cache into a new project; settled is a sync with nothing to do; status, path and path-member read the project, path printing the dataset's folder and path-member one file's.

| case | main s | final s | final/main | peak MiB final, main |
|---|---:|---:|---:|---|
| 100,000 files, cold | 18.59 | 9.27 | 0.498 | 208, 223 |
| 100,000 files, warm | 1.02 | 0.94 | 0.928 | 201, 197 |
| 100,000 files, settled | 0.54 | 0.40 | 0.738 | 97, 97 |
| 100,000 files, status | 0.60 | 0.43 | 0.725 | 120, 120 |
| 100,000 files, path | 0.67 | 0.41 | 0.613 | 120, 97 |
| 100,000 files, path-member | 0.76 | 0.40 | 0.532 | 118, 119 |
| 1,000,000 files, cold | 215.77 | 116.47 | 0.540 | 1251, 1254 |
| 1,000,000 files, warm | 11.85 | 10.66 | 0.900 | 1166, 1081 |
| 1,000,000 files, settled | 6.41 | 4.55 | 0.710 | 754, 771 |
| 1,000,000 files, status | 7.29 | 5.45 | 0.747 | 850, 850 |
| 1,000,000 files, path | 7.35 | 5.78 | 0.786 | 850, 907 |
| 1,000,000 files, path-member | 8.65 | 4.89 | 0.566 | 854, 909 |

## a/b passes on the linux lab, 2026-11

each change below ran behind a switch in one build, against the same build with the switch off, in paired rounds shuffled with the [lab](benchmark-design.md)'s other tools, five to twelve rounds a case. a ratio is the median of new over old, with its 95% bootstrap interval where one was kept; under 1 is faster or smaller. a change stayed only if it won where it aimed and made no lan case slower (an interval's lower end above 1.02); it then became the only code and its switch went. a change that lost went with its code.

| change | commit | result | outcome |
|---|---|---|---|
| several connections per host for file bodies | 92bc25d | far s2 0.288, poor s2 0.424; lan and home flat | kept |
| no closing resume record for small downloads | 0462f98 | ext4 s3 0.840, xfs s3 0.757, xfs s2 0.831 | kept |
| archive staging spread over 16 folders | b4fd36a | ext4 s4 0.798, xfs s4 0.830; cpu 0.66 | kept |
| download permits released before publication | 8fce749 | ext4 s3 0.925, s2 0.942 | kept |
| placement workers limited by parent folder, one on exfat | ff9e441 | wall flat; cpu 0.79 to 0.81; exfat cpu 85% lower | kept |
| project scan in bounded parallel batches | 12cdbf6 | status at 100,000 files 0.600 s to 0.442 s | kept |
| fast lock reader for commented locks | 0f4ecab | list at 100,000 files 0.213 s to 0.100 s; peak memory 334 MB to 78 MB | kept |
| waiting `path` calls finish once the project is ready | 7393540 | 32 waiters 3.24 s to 0.25 s | kept |
| download jobs built at admission | cfabec6 | peak memory 228 MB to 205 MB at 100,000 pins | kept |
| hosts interleaved in the download queue | 53957ca | two-host fixture 0.764 | kept |
| transparent huge pages off for the process | 24cd618 | peak memory 0.33 to 0.73 on every lan case, as ext4 s2 0.327 [0.311, 0.331]; wall flat there, but slower for a no-op sync and for placing from the cache, [measured alone](#ab-passes-on-placement-and-startup-2026-11) | dropped |
| unfinished downloads spread over 16 folders | faed8f4 | ext4 s3 0.902 [0.809, 0.962], xfs s5 0.712 [0.499, 0.814] | kept |
| a hardlink's details taken from its object | 79cc47c | xfs s4 0.933 [0.876, 0.954] | kept |
| one flush per file instead of one `syncfs` a batch | e803ca7 | ext4 s3 1.567, s2 1.183 | dropped |
| no-replace rename for new objects | 3dca829 | ext4 s4 1.090 | dropped |
| parent folder handles kept between batches | 8a93d8e | s5 1.268 and 1.297 | dropped |
| small copies instead of reflinks on xfs | 052c4e9 | xfs s4 1.245 | dropped |
| project state written as a stream | 0f4fddc | no measured win | dropped |
| fast large files split into ranges, from a link-rate probe | c095724 | live hugging face, 548 MB: 3.7 s against 5.6 s; but lan 1 GiB 1.075, home s1 1.035 [1.032, 1.037] and s7 1.025, with 1.5 to 1.6 times the cpu | dropped; splitting waits for a slow stream |
| shorter download queues | none kept | xfs s1 1.108 [1.060, 1.181]; peak memory 0.866 on xfs s1 alone | dropped |
| xfs staging through `O_TMPFILE` | none kept | xfs s4 0.955 [0.914, 1.026]; cpu 0.827 | dropped, no wall win |
| `panic = unwind` for a clean thread error | none kept | no faster anywhere; abort up to 4% faster on s2; binary 14.5% larger | abort kept, with a startup thread probe (7e6ab3b) |

## a/b passes on placement and startup, 2026-11

the final benchmark found two cases slower than main: a no-op sync and placing from a full cache. measured alone, the build before the changes below took 1.19 times main's time for the no-op and 1.25 times to place 100,000 files, and profiles traced it to three causes. each change ran as a static musl build against the build before it, with main and every build run once a round in a shuffled order: six rounds placing 100,000 files from a full cache, 15 placing 1,000 one-file datasets and 40 no-op syncs of those datasets. a ratio is the median of new over old with its 95% bootstrap interval, and the rule above applied.

| change | commit | result | outcome |
|---|---|---|---|
| placement work per file cut: each folder checked and grouped once, the root stripped once, results put in order without a sort, a worker woken only when one waits | 5911edb | 100,000 files 0.840 [0.832, 0.862]; 1,000 datasets 0.955 [0.948, 0.962]; no-op flat | kept |
| transparent huge pages left on | 914b739 | 100,000 files 0.884 [0.873, 0.889]; 1,000 datasets 0.971 [0.959, 0.977]; no-op 0.918 [0.910, 0.924]; peak memory 1.17, 2.47 and 1.72 | kept |
| mimalloc for libc's own allocations on musl | d2bc3e7 | no-op 0.833 [0.828, 0.838]; 1,000 datasets 1.007 [0.995, 1.015]; 100,000 files 1.017 [1.002, 1.021] | kept |

with all three, against main: 100,000 files 0.944 [0.934, 0.948], 1,000 datasets 1.003 [0.991, 1.017] and the no-op 0.909 [0.905, 0.917].

## flushing a few files on a busy disk, 2026-11-09

the readme's first `add`, 18 KiB from hugging face, took 14 s on this machine's os disk, which a training job kept busy: the batch's one `syncfs` waits for everything any program wrote to the filesystem, and one took 16.7 s. the final benchmark measured d2bc3e7, and this change was measured on its own, below. the other commits since change nothing the lab times but a check for a git repository after each sync, a few system calls.

the flush alone, after the same writes on the lab's disks, with the median of seven rounds for one file and nine for 64:

| flush | ext4 | xfs |
|---|---:|---:|
| one 1 GiB file: fsync, then one `syncfs` | 1006.9, 1004.9 ms | 925.8, 927.5 ms |
| 64 files of 64 KiB: fsync each on 8 threads, then one `syncfs` | 7.6, 4.1 ms | 14.4, 5.8 ms |

on the busy os disk, one `syncfs` took a median 424 ms after one small file and up to 14.9 s, where fsync for each of 64 files took 13 to 190 ms.

a sync of four small files from a local server, each build once a round in a shuffled order, 15 rounds: on the busy os disk 684 ms (worst 1,860) before and 185 ms (worst 216) after, 0.264 of the time; on the lab's idle nvme 17 ms either way, 0.973.

| change | commit | result | outcome |
|---|---|---|---|
| the first 64 files of every batch flushed on their own | none kept | ext4 s1 1.078 [1.064, 1.303] in seven rounds; peak memory ext4 s2 1.151 [1.028, 1.266] and s3 1.078 [1.002, 1.146]: those 64 flushes held threads in every large batch | dropped |
| a run of up to 64 downloads, copies or archive members flushed one by one, larger runs as before | 4639987 | ext4 s1 0.985 [0.894, 1.041] and xfs s1 1.004 [0.988, 1.025] in 15 rounds; ext4 s4 1.003 [0.909, 1.199], peak memory 1.033 [0.984, 1.110]; s2, s3, s5 and s6 run the same code as before | kept |

## evidence limits

the sections from here on are non-reproducible historical observations, not benchmark results from the current harness. raw run records and complete reproduction commands were not retained alongside these tables. commits establish the implementation, not the measured ratios. missing machines, profiles, counts or statistics remain unknown; do not infer them or treat a reported percentage as a current guarantee.

use the [benchmark workflow](../engineering/benchmarks.md) for new comparisons with retained commands, run records and independent checking.

## conditions

W means windows 11 with defender real-time protection on. L means ubuntu 24.04 in WSL 2 on ext4. both use the same 11th gen intel i7-1185G7 laptop with 4 cores, 8 threads, 32 GB and nvme. unless a row says otherwise, these are release builds measured on 2026-10-30 or 2026-10-31.

baseline f9bbda5 used at least ten runs after three warm-ups on an otherwise idle machine; its command measurements are in [budgets](budgets.md). later rows preserve their own counts. "not recorded" means no commit, count, median or range was retained for that field; a range's midpoint is not a measured median.

## storage and memory

small-download runs used 1,000 URLs of 100 KiB, one dataset per URL, an empty cache and a local linux server. the older path paid three flushes per file, about 3.3 ms each. the retained change removes checkpoint and index flushes, not object, change-mark, unpack-record or registration flushes.

the memory fixture was a 100,000-member zip of 1 MiB. RSS is resident set size, the process's physical-memory footprint. `/usr/bin/time` measured peak RSS without the profiler; heaptrack identified allocations separately. the recent hash-thread and copy-flush records name linux but do not record a machine model, exact median or full per-configuration range beyond the values below.

## placement and settled sync

warm-placement runs copied `data.toml` and `data.lock` into an empty project using a fully populated cache. fixtures were 10,000 files of 4 KiB in 100 folders and 8,000 files of 2 KiB in one folder. on 2026-10-30, other builds kept the laptop at full load. fd64035 and 2ecb70a alternated ten runs per side. `FETCHLOOM_TRACE` separated command work from process startup, which alone took 1 to 3 s under that load.

placement was 2 to 3 times slower than in the earlier quiet window. the no-readback experiment won 3 of 10 pairs for 10,000 files and 6 of 10 for 8,000; individual pairs differed by up to 2 times either way. those medians did not establish a repeatable gain, so the experiment was reverted.

## network

network clients ran on W against a local test server's eth0 address with shaped traffic and an empty cache per run. "lan", "home" and "far" are the recorded profile names; their exact shaping parameters are not retained here. live rows name the service. paired configurations alternated. these results cannot be treated as fixed speeds for those services.

## observations and rejected experiments

parenthesized bounds are observed minima and maxima. when only a time, rate or peak was recorded, the median and range columns say so rather than supplying a new statistic.

| workload or change | commit | machine and runs | median or recorded value | range | outcome |
|---|---|---|---|---|---|
| local-download baseline | f9bbda5 | W and L; baseline counts | about 180 MB/s W; 460 MB/s L | not recorded | includes hashing and payload flush |
| in-memory SHA-256, 1 GiB | not recorded | same laptop; best of three | median not recorded; sha2 and ring rates at right | sha2 1.70 to 1.83 GB/s; ring 1.63 to 1.69 GB/s | keep sha2 |
| MD5 alone and SHA-256 with MD5 | not recorded | same CPU; best of three over 1 GiB | median not recorded; 0.68 GB/s and 0.48 GB/s | not recorded | compute MD5 only when needed |
| separate hash thread, local 1 GiB | 1648e29 | linux, machine not recorded; four interleaved pairs | median not recorded; about 25% faster | before 2.06 to 2.27 s; after 1.58 to 1.65 s | keep `HashPipe` |
| hand response chunks to the writer and hash threads in batches | 0616aa1 to 57438ae | L, benchmark lab: 1 GiB over HTTPS and HTTP/2 from a separate network namespace, empty cache, page cache dropped; five interleaved pairs | median 5.37 s to 4.03 s, 25% faster; futex calls 117,064 to 6,702 | before 4.73 to 5.73 s; after 3.53 to 4.64 s | keep: 256 KiB batches to the writer, 1 MiB to the hasher |
| the same, writer batches alone | 0616aa1 to a9c909f | same lab; five interleaved pairs | no change: 4.05 to 6.54 s against 4.08 to 5.42 s | as stated | kept with the hasher batches; the hasher handoff was the cost |
| flush a batch of downloads once with `syncfs`, then name them | 57438ae to 3837774 | L, benchmark lab: 1,000 files of 100 KiB over HTTPS, empty cache, page cache dropped; five interleaved pairs | median 4.93 s to 1.79 s, 64% faster; an upper bound without any flush was 1.36 to 1.74 s | before 4.16 to 5.61 s; after 1.62 to 2.14 s | keep on linux 5.8 and later; elsewhere each download still flushes on its own |
| flush 10,000 copies of 4 KiB with `copy = true` | f68ef48 per-copy; 1ef51b7 batching | linux, machine not recorded; six interleaved pairs | reported times: 0.27 s without flushing, 9.19 s per-copy, 0.41 s one `syncfs`; separate medians not recorded | not recorded | keep payload durability and batch on linux |
| checkpoint and index flush removal, 1,000 small downloads | 0533392 | L, local lan; ten interleaved pairs | before 4.46 s; after 2.18 s | before 4.27 to 6.31 s; after 2.09 to 3.87 s | 51% faster; keep |
| archive-member batch flush | 26a3dbc | L, ext4; three runs each | median not recorded; after about half a second; per-file flush 0.7 ms | before 8.5 to 10.2 s | per-file path cost twenty times more; keep batch |
| compact placement metadata, add | 9d3c7b1 to 33825eb | L, 100,000-member zip; count not recorded | peak RSS 162 MiB to 165 MiB; median not recorded | earlier spread not recorded | no add improvement |
| compact placement metadata, warm placement | 9d3c7b1 to 33825eb | L, same zip; count not recorded | peak RSS 183 MiB to 151 MiB; median not recorded | not recorded | keep, still misses 150 MB |
| heaptrack add investigation | 33825eb | L; count not recorded | heap peak 116 MiB; median not recorded | not recorded | retained allocations and allocator arenas explain part of RSS |
| `MALLOC_ARENA_MAX=2` | 33825eb investigation | L, eight unpack writers; count not recorded | add peak 143 MiB; median not recorded | not recorded | no programmatic cap: needs unsafe `mallopt` or another allocator |
| build placement paths per file | 584c9c0 | no isolated measurement recorded | median and peak not recorded | not recorded | keep lower retained path allocation |
| peak memory after compact metadata and per-file paths | 32c6e22 | L, 100,000-member zip of 1 MiB over local HTTP; three runs each | add 136 to 142 MiB; warm placement 94 to 102 MiB | add 6.6 to 7.3 s, one run 40.9 s; placement 1.0 to 1.3 s | both under the 150 MB budget |
| no hardlink readback, retested | 32c6e22 | W, 10,000 files of 4 KiB in 100 folders; two sets of eight interleaved pairs under load | first set: after won 7 of 8, best 2.49 to 2.24 s; second set: after/before ratios 0.70 to 2.77 | outliers to 70 s on both sides | inconsistent again; dropped |
| directory-listing metadata | not recorded | W; count not recorded | about 15 times faster; absolute median not recorded | not recorded | keep; L still needs stat calls |
| live warm placement, hf:datasets/Matthijs/cmu-arctic-xvectors | not recorded | W and L, debug builds under load; single reported runs | about 8,000 files in one folder: W 30.5 s, L 3.8 s | not recorded | investigate placement, not network |
| live warm placement, tar.gz | not recorded | W, debug under load; one reported run | about 52,000 small files: 87 s | not recorded | investigate filesystem cost |
| trace, 10,000 files in 100 folders | fd64035 / 2ecb70a study; traced side not recorded | W under full load; one trace | command 3.85 s; object checks 0.47 s; placing 3.19 s; rest 0.19 s | not recorded | placement dominates |
| trace, 8,000 files in one folder | same study | W under full load; one trace | command 12.1 s; object checks 0.29 s; placing 11.7 s; rest 0.12 s | not recorded | same finding |
| bare links, 10,000 files | not recorded | W; counts and load not recorded | median not recorded; across 100 folders 2.2 s | one folder 10.3 to 12.7 s | spread placement |
| bare links, 8,000 files | fd64035 / 2ecb70a study | W under load; count not recorded | median not recorded; across 100 folders 1.7 s | one folder 10.2 to 12.3 s; single-thread link 0.6 to 3.7 ms | no trend as folder fills |
| thread-count probe, 8,000 links in one folder | same study | W under load; interleaved, count not recorded | medians not recorded | one thread 17 to 149 s; two 10 to 49 s; four 8.5 to 20 s; eight 10 to 19 s | keep 8 threads |
| readback probe, 10,000 spread links | same study | W, 8 threads; count not recorded | 2.48 s without reads; 2.44 s with reads; medians not recorded | not recorded | no readback win |
| folder checks | same study | W; count not recorded | 5 to 10 us per file; 54 to 97 ms per 10,000; medians not recorded | those recorded bounds | about 2% of placing, below the 10% change threshold; retain checks |
| no hardlink readback, 10,000 files, process | fd64035 to 2ecb70a | W full load; ten interleaved runs each | before 11.4 s; experiment 12.3 s | 8.0 to 27.1 s; 8.4 to 28.4 s | drop experiment |
| no hardlink readback, 10,000 files, command | same pair | same runs | before 10.1 s; experiment 10.8 s | 7.0 to 25.6 s; 7.2 to 27.1 s | drop experiment |
| no hardlink readback, 8,000 files, process | same pair | same runs | before 20.0 s; experiment 16.8 s | 14.6 to 27.9 s; 13.3 to 29.3 s | inconsistent pairwise result |
| no hardlink readback, 8,000 files, command | same pair | same runs | before 17.9 s; experiment 15.2 s | 13.2 to 26.4 s; 11.9 to 27.8 s | inconsistent pairwise result |
| settled sync, 10,000 files, command | fd64035 to 2ecb70a | W full load; ten interleaved runs each | before 200 ms; after 131 ms | 153 to 708 ms; 91 to 174 ms | keep shortcut |
| settled sync, 8,000 files, command | same pair | same runs | before 303 ms; after 158 ms | 174 to 480 ms; 102 to 371 ms | keep shortcut |
| settled sync, 10,000 files, process | same pair | same runs | before 1.27 s; after 1.21 s | 1.11 to 3.18 s; 1.07 to 1.61 s | startup obscures command gain |
| settled sync, 8,000 files, process | same pair | same runs | before 1.53 s; after 1.82 s | 1.41 to 5.48 s; 1.23 to 12.8 s | startup remains noisy |
| adaptive split, lan 1 GiB | 0cbc48b | W; ten interleaved pairs | one stream 5.54 s; adaptive 5.48 s | 5.00 to 9.45 s; 5.07 to 6.12 s | no split |
| adaptive split, home 1 GiB | 0cbc48b | W; ten interleaved pairs | one stream 97.55 s; adaptive 97.82 s | 96.39 to 106.79 s; 96.94 to 99.25 s | no split |
| adaptive split, far 100 MiB | 0cbc48b | W; five interleaved pairs | one stream 496.65 s; adaptive 174.03 s | 495.82 to 518.50 s; 135.81 to 181.26 s | 4 requests, 65% faster |
| adaptive split, live cifar-10 at `www.cs.toronto.edu` | 0cbc48b | W; three interleaved pairs | one stream 156.29 s; adaptive 39.81 s | 118.58 to 178.45 s; 37.23 to 40.81 s | 74% faster; one stream about 1 MB/s that night |
| split from first answer, lan 1 GiB | 1583af9 | W, 2026-10-30; count not recorded | one stream 5.96 s; split 7.00 s | 5.01 to 7.41 s; 6.99 to 8.57 s | 6 requests, 17% slower; one stream about 180 MiB/s |
| split from first answer, home 1 GiB | 1583af9 | W; count not recorded | one stream 95.6 s; split 96.7 s | 95.5 to 96.2 s; 95.6 to 97.4 s | 4 to 5 requests, 1% slower |
| split from first answer, far 100 MiB | 1583af9 | W; three runs each | one stream 500 s; split 215 s | 490 to 531 s; 191 to 285 s | 2 to 4 requests, 57% faster; use adaptive default |
| HTTP/1.1 instead of HTTP/2, live `hf:datasets/cais/mmlu` without `data.tar`, 180 files, 99 MiB | commit not recorded; same binary both sides | W; five interleaved pairs | HTTP/2 45.32 s; HTTP/1.1 46.03 s | 42.69 to 49.50 s; 45.31 to 48.19 s | drop; earlier HTTP/2 run of 92 s with eight retries did not reproduce |
| rename publication instead of hardlink plus remove | not recorded | W; estimated saving, no run count | about 1 s of a 34 s unpack; no measured median | not recorded | below 10%; drop |
| skip small-member flushes | not recorded | W, random-file fixture; one recorded run | unpack 54 s; 10,000 scans, scan median 24 ms | not recorded | scan cost remains; drop |
| 8 MB/s split trigger | fafec0c | W, live toronto; two pairs | one stream at 5.6 MB/s: 29.16 s; adaptive 38.86 s | not recorded | 33% later; neutral on home and lan, but one lan run split under load; lower to 1 MB/s |
| fat rather than thin LTO | earlier version, commit not recorded | machine and count not recorded | 7% smaller at same speed; absolute medians not recorded | not recorded | retain fat LTO |
| mimalloc | earlier version, commit not recorded | machine and count not recorded | within noise; medians not recorded | not recorded | keep system allocator |
| historical local 1 GiB, empty cache and second project | 240f17a | L, 2026-10-31; 3 blocks instead of planned 5 | 4.00 s; second project 0.01 s | not recorded | predates hint-flush change |
| historical local 100 MiB | 240f17a | same L run | 0.35 s | not recorded | same older build |
| historical local zip, 10,000 files | 240f17a | same L run | 0.87 s; second project 0.20 s | not recorded | same older build |
| historical local tar.zst, 10,000 files | 240f17a | same L run | 0.76 s | not recorded | same older build |
| historical 1,000 URLs of 100 KiB | 240f17a | same L run | lan 3.53 s; home 9.46 s | not recorded | later hint-flush rows supersede this cost |

related: [budgets](budgets.md), [windows defender study](windows-defender.md), [profiling](../engineering/profiling.md), [network](../architecture/network.md), [placing](../architecture/placing.md).

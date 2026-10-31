# measurements

historical workload observations explain retained and rejected performance changes.

## evidence limits

these are non-reproducible historical observations, not benchmark results from the current harness. raw run records and complete reproduction commands were not retained alongside these tables. commits establish the implementation, not the measured ratios. missing machines, profiles, counts or statistics remain unknown; do not infer them or treat a reported percentage as a current guarantee.

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

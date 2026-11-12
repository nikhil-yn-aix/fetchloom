# measurements

what the linux lab measured for this release, the a/b passes behind its performance decisions, and the historical observations before them.

## ledger

every change measured with the current harness, one row each: what it gained where it aimed, what it cost, and whether it stayed. a ratio is new over old, so under 1 is faster or smaller; the section a row links to holds its fixture, rounds, intervals and keep rule. changes measured before 2026-11-09, without retained run records, are in [observations](#observations-and-rejected-experiments).

| area | change | result | outcome | details |
|---|---|---|---|---|
| network | several connections per host for file bodies | far s2 0.288, poor s2 0.424; lan and home flat | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | hosts interleaved in the download queue | two-host fixture 0.764 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | download jobs built at admission | peak memory 228 to 205 MB at 100,000 pins | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | download permits released before publication | ext4 s3 0.925 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | no closing resume record for small downloads | xfs s3 0.757, ext4 s3 0.840 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | unfinished downloads spread over 16 folders | xfs s5 0.712, ext4 s3 0.902 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | a large download written to the disk while it arrives | `get -o` of 10 GiB 0.531; small downloads flat | kept | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| network | an unchanged bucket object reused by its listed ETag and size | `lock --upgrade` of a prefix: 7 to 2 requests, 12.2 to 2.0 MB | kept | [bucket prefix](#upgrading-a-bucket-prefix-2026-11-12) |
| network | fast large files split into ranges from a link-rate probe | live hugging face 5.6 to 3.7 s, but lan 1 GiB 1.075 and 1.5 to 1.6 times the cpu | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | shorter download queues | xfs s1 1.108 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| hashing | a hashing read's buffer sized to the file | importing 100,000 files 0.812; `cache verify` of 100,000 objects 0.784 | kept | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| hashing | hashing read buffers started at a page | `cache verify` of 8 GiB on glibc 0.697; flat on musl | kept | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| flushing | runs of up to 64 files flushed one by one, larger batches with one `syncfs` | a sync of 4 small files on a busy disk 684 to 185 ms; the lab flat | kept | [busy disk](#flushing-a-few-files-on-a-busy-disk-2026-11-09) |
| flushing | a waiting group of finished downloads named after at most 1 s | a second project on a shared cache 26.4 to 12.9 s | kept | [shared cache](#a-second-run-on-a-shared-cache-2026-11-12) |
| flushing | one flush per file instead of one `syncfs` a batch | ext4 s3 1.567 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| flushing | the first 64 files of every batch flushed on their own | ext4 s1 1.078, peak memory 1.151 | dropped | [busy disk](#flushing-a-few-files-on-a-busy-disk-2026-11-09) |
| archives | archive staging spread over 16 folders | ext4 s4 0.798, xfs s4 0.830 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| archives | no-replace rename for new objects | ext4 s4 1.090 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| archives | xfs staging through `O_TMPFILE` | xfs s4 0.955 [0.914, 1.026] | dropped, no win | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| archives | a new object published on btrfs by one rename | 0.970 | dropped, no win | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| placement | placement workers limited by parent folder, one on exfat | wall flat; cpu 0.79 to 0.81, on exfat 85% lower | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| placement | a hardlink's details taken from its object | xfs s4 0.933 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| placement | placement work per file cut | 100,000 files 0.840 | kept | [placement](#ab-passes-on-placement-and-startup-2026-11) |
| placement | each dataset's folder found with one lookup | sync of 40,000 datasets 9.68 to 6.33 s | kept | [list of links](#adding-a-list-of-links-2026-11-12) |
| placement | small copies instead of reflinks on xfs | xfs s4 1.245 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| placement | parent folder handles kept between batches | s5 1.268 and 1.297 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| project state | project scan in bounded parallel batches | status at 100,000 files 0.600 to 0.442 s | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| project state | one pool of scan workers for the whole walk | settled sync of 100,000 files 0.902, but of 10,000 1.052 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| project state | the state's map built in one pass | settled sync of 100,000 files 0.784, but peak memory 1.148 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| project state | the same map built in merged chunks | 0.85, but on musl peak memory 1.05 to 1.06 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| project state | project state written as a stream | no measured win | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| lock and names | fast lock reader for commented locks | list at 100,000 files 0.213 to 0.100 s; peak memory 334 to 78 MB | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| lock and names | `add -i` writes data.toml once for the whole list | 10,000 links 415 to 4.6 s | kept | [list of links](#adding-a-list-of-links-2026-11-12) |
| lock and names | relocking looks datasets up in sets and maps | 40,000 links 31.7 to 21.6 s | kept | [list of links](#adding-a-list-of-links-2026-11-12) |
| lock and names | `path` and `sync <name>` read only the named folder, state records, pins and lock lines (five changes) | `path` of 3 files beside 100,000 415 to 29 ms, peak memory 0.305; `sync` 392 to 30 ms | kept | [named commands](#commands-that-name-a-dataset-2026-11-12) |
| lock and names | the number of processors read once a run | 0.997 | dropped, no win | [named commands](#commands-that-name-a-dataset-2026-11-12) |
| lock and names | waiting `path` calls finish once the project is ready | 32 waiters 3.24 to 0.25 s | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| cache | the cache report's copies pass on parallel workers | 100,000 placed files 0.82 to 0.60 s, same peak memory | kept | [cache report](#the-cache-report-on-100000-placed-files-2026-11-11) |
| push | files written, flushed by the batch rule, then read back | first push of 10,000 small files 36.7 to 2.7 s | kept | [folder mirror](#pushing-into-a-folder-mirror-2026-11-11) |
| push | a push's unfinished files spread over 16 folders | 100,000 files 0.729, but 50 files 1.046 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| build | a step's exit wakes the build's watch | a 10,000-file recipe 0.889, but on musl peak memory 1.061 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| process | transparent huge pages turned off | peak memory 0.33 to 0.73, but a no-op sync and placing slower. left on: 100,000 files 0.884 and the no-op 0.918, for peak memory 1.17 to 2.47 | dropped, huge pages stay on | [lab](#ab-passes-on-the-linux-lab-2026-11), [placement](#ab-passes-on-placement-and-startup-2026-11) |
| process | mimalloc for libc's own allocations on musl | no-op sync 0.833 | kept | [placement](#ab-passes-on-placement-and-startup-2026-11) |
| process | `panic = unwind` for a clean thread error | no faster anywhere; binary 14.5% larger | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| process | the runtime's threads joined, at most 25 ms, before exit | fixes a rare windows hang under `-vv`; wall within noise; a stopped run waits up to the bound | kept, for correctness | [end of a run](#the-end-of-a-run-2026-11-12) |

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

## a/b passes from a profile of nine jobs, 2026-11-12

profiling nine everyday jobs against the plain tools people use for them turned up eight changes. each ran behind a switch in one release build, against the same build with the switch off, every arm once a round in a seeded shuffled order, at least ten measured rounds a case, on ext4, xfs and btrfs over nvme with a local server; the static musl build unless glibc is named. a ratio is the median of new over old with its 95% paired bootstrap interval from `bench/ab.py`. a change stayed only if an interval where it aimed ended below 0.97, no run failed and no measured case's interval started above 1.02; the changes to how the state is read and to the step's wake were also held to no rise in peak memory. the machine rebooted midway, and no pair mixes runs from before and after it.

| change | result | outcome |
|---|---|---|
| a hashing read's buffer sized to the file, up to 1 MiB, rather than 1 MiB for every file | importing 100,000 files 0.812 [0.787, 0.823], on glibc 0.939 [0.933, 0.962]; `cache verify` of 100,000 objects 0.784 [0.725, 0.826]; hashing 8 GiB, a 1 GiB download, 10,000 small downloads and a zip of 10,000 members flat | kept |
| hashing read buffers started at a page | `cache verify` of 8 GiB on glibc 0.697 [0.693, 0.699] warm and 0.949 [0.944, 0.954] cold; flat on musl, whose allocator already starts them at a page | kept |
| a large download written to the disk while it arrives, every 32 MiB after its first 64 MiB | `get -o` of 10 GiB 0.531 [0.530, 0.540], at 500 MB/s 0.711 [0.704, 0.727]; 1 GiB 0.553 on ext4 and 0.913 on xfs; a resumed download 0.738; 1,000 and 10,000 small downloads flat | kept |
| a step's exit wakes the build's watch | a recipe that writes 10,000 files 0.889 [0.886, 0.896], but on musl peak memory 1.061 [1.058, 1.078], with the step's output threads joined first or not; on glibc 1.004 [0.999, 1.009] | dropped |
| the state's map built in one pass from a list of its files | a settled sync of 100,000 files 0.784 [0.771, 0.791], but peak memory 1.148 [1.130, 1.185] | dropped |
| the same map built in chunks of 4,096 or 1,024 files that merge | a settled sync of 100,000 files 0.85 with no more memory, but on musl peak memory 1.06 at 10,000 files and 1.05 for `status` of 100,000; on glibc 1,000,000 files 0.997 and 1.017 | dropped |
| one pool of scan workers for the whole walk | a settled sync of 100,000 files 0.902 [0.890, 0.917], but of 10,000 1.052 [1.046, 1.063] | dropped |
| a push's unfinished files spread over 16 folders | a first push of 100,000 files 0.729 [0.725, 0.748], but a push of 50 files 1.046 [1.034, 1.060] | dropped |
| a new object published on btrfs by one rename | a zip on btrfs 0.970 [0.956, 0.986] | dropped, no win |

with the three kept changes and the step's wake on together, against all off: importing 100,000 files 0.803 [0.797, 0.813], `cache verify` of 8 GiB on glibc 0.691 [0.686, 0.693], `get -o` of 10 GiB 0.553 [0.530, 0.587] and of 1 GiB 0.553; 10,000 small downloads, a settled sync of 100,000 files and `status` of 3 stayed flat.

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

## the cache report on 100,000 placed files, 2026-11-11

`fetchloom cache` reads each registered project's state and looks once at every placed file whose object the cache holds, to count copies. the fixture: two projects that each imported a folder of 50,000 files of 60 to 600 bytes into one cache, so 100,000 objects and 100,000 placed files, all hardlinks, on ext4. release builds of f5de3fe with two temporary switches, one leaving the copies pass out and one running it on `place::run_all`, and of the change itself; `fetchloom --json cache` under `/usr/bin/time`, each variant once before the rounds and then once a round in a rotated order, nine rounds. the machine was shared: ubuntu 24.04 on 24 cores of an AMD EPYC 7V13, with a load average of 43 to 63 from other work.

| report | median | range | peak RSS |
|---|---:|---:|---:|
| the needs walk and the object scan, as before the copies pass | 0.42 s | 0.41 to 0.43 s | 51.8 MiB |
| with the copies pass on one thread | 0.82 s | 0.81 to 0.85 s | 76.2 MiB |
| with the copies pass on `run_all`, at most eight threads | 0.60 s | 0.59 to 0.62 s | 76.2 MiB |

the pass took 0.40 s on one thread and 0.18 s on `run_all`, so the report runs it there: 0.73 of the one-thread time, with the same peak memory, which a project's parsed state adds. caches were warm; a cold disk or a network drive makes each look wait longer.

## live sites against their own tools, 2026-11-09

each service's public dataset from the [benchmark design](benchmark-design.md#tools), fetched on the shared machine of the final benchmark by fetchloom and by the service's own tool: five rounds, both tools in a new random order each round, every run with empty caches. `bench/live.py` ran them. fetchloom is the static musl build of f49f38e; the others are zenodo_get 3.1.0, `hf download` from huggingface_hub 0.35.3 with hf_xet 1.8.0 followed by `sha256sum -c`, kagglehub 0.3.13 and datahugger 0.13. a run counts only when every file it fetched matches the service's published SHA-256. a live site's speed changes from minute to minute, so, as the design says, each tool gets its median and range rather than a ratio.

| service | dataset | files · size | fetchloom, median and range | own tool | its median and range | CPU s, fetchloom and own tool |
|---|---|---|---|---|---|---|
| zenodo | 8164711 | 1 · 102.0 MiB | 42.3 s, 38.0 to 43.9 | zenodo_get | 56.0 s, 34.4 to 101.7 | 0.84, 1.20 |
| hugging face | stanfordnlp/imdb, plain_text parquet | 3 · 79.6 MiB | 5.3 s, 4.1 to 6.7 | `hf download` | 18.6 s, 15.5 to 19.7 | 0.35, 0.97 |
| kaggle | zynicide/wine-reviews, version 4 | 1 · 50.5 MiB | 1.95 s, 1.81 to 2.63 | kagglehub | 7.2 s, 3.3 to 13.2 | 0.22, 0.65 |
| figshare | 10.6084/m9.figshare.5616403 | 3 · 7.1 MiB | 1.51 s, 1.50 to 1.53 | datahugger | 7.2 s, 6.0 to 9.0 | 0.07, 0.71 |
| dataverse | doi:10.7910/DVN/TJCLKP | 3 · 295 KiB | 1.03 s, 0.99 to 1.34 | datahugger | failed in 5 of 5 | 0.03, none |

on hugging face, kaggle and figshare, fetchloom's slowest run was faster than the other tool's fastest. zenodo sends one file at its own pace, and its spread covers the difference: zenodo_get's fastest run beat fetchloom's fastest, and its slowest took more than twice as long. datahugger fetched dataverse's ingested `.tab` copy of a table instead of the published `.tsv`, which its own check reported as a checksum mismatch, so none of its runs counted; fetchloom fetched the published files. peak memory was 27 to 55 MiB for fetchloom and 36 to 127 MiB for the others.

## pushing into a folder mirror, 2026-11-11

`push` first flushed each file it wrote before reading it back and naming it, and on a busy disk that flush took about 20 ms a file. the change writes all of a push's files, puts them on the disk by the cache's batch rule, then reads each back and names it; both ways flush the folders that gained names at the end. one release build ran both behind a switch, both in every round, the order swapped each round, each run into a new mirror folder after a `syncfs` of the disk and 2 s of rest. the mirror, the cache and the project were on the os disk, ext4, of the shared 24-core machine, which other work kept at a load average of 44 to 62. every run ended with status 0, every file under its name and nothing left in `tmp/`, and in the first two rounds every file was hashed against its name. a ratio is the median, over rounds, of the time together over the time with each file flushed, with its 95% bootstrap interval.

| workload | rounds | each file flushed, median and range | together, median and range | ratio |
|---|---:|---|---|---|
| 10,000 files of 9 to 13 bytes, first push | 6 | 36.73 s, 34.72 to 57.42 | 2.71 s, 2.06 to 5.67 | 0.074 [0.058, 0.121] |
| 100 files of 10 MiB | 6 | 32.74 s, 26.69 to 61.59 | 20.02 s, 18.92 to 21.05 | 0.619 [0.378, 0.717] |
| one file of 1 GiB | 12 | 20.62 s, 17.26 to 42.56 | 19.04 s, 17.81 to 37.05 | 0.988 [0.888, 1.065] |

one file is under the batch rule's 64, so both ways flush it on its own; its row measures reading it back through a new handle after closing the one that wrote it, which cost nothing measurable. system cpu time was 2.91 s against 3.05 s for the small files, and the same for the large ones. built as committed, at a load average of 47, a first push of the 10,000 files took 2.58 to 2.93 s in three runs, and a second push, which finds each file with one look at its name and writes nothing, 0.02 s in nine.

## adding a list of links, 2026-11-12

`add -i` of a list of links, each to one file of 90,000 to 110,000 bytes on a server on this machine, each run into an empty project and an empty cache on tmpfs. the old code wrote data.toml's text again and read it back for each link, to add the link's dataset and name the next one; the change writes and reads it once for the whole list. release builds of c9da2d6 (before) and of the change (after) took turns, the order swapped each round, on the shared 24-core machine at a load average of 9 to 14.

| links | rounds | before, median and range | after, median and range | after/before |
|---:|---:|---|---|---:|
| 500 | 5 | 0.94 s, 0.92 to 1.10 | 0.24 s, 0.23 to 0.25 | 0.247 |
| 2,000 | 5 | 13.29 s, 13.23 to 14.89 | 1.37 s, 0.79 to 1.43 | 0.096 |
| 10,000 | 2 | 415.3 s, 410.3 to 420.3 | 4.64 s, 4.58 to 4.70 | 0.011 |

before's time was cpu, growing with the square of the list: 12.8 s of user time for 2,000 links, 414 s for 10,000. a trace of after's 10,000 links shows 3.1 s of downloads, 0.64 s relocking and 0.61 s placing, and at a terminal no gap of more than 0.85 s without a new line or bar. tmpfs leaves out the disk's flushes, which the change does not touch.

relocking then still looked up each new dataset in lists of all the others, four times over: in `resolve_all`, `rename::carry`, `draft` and `plan::from_cache_adding`. those became set and map lookups. the same setup with lists past 10,000 links, which repeat a link with `?c=1` and on, as the server ignores the query; release builds of b3b182a (before) and of the change, three rounds each:

| links | before, median and range | after, median and range | after/before |
|---:|---|---|---:|
| 10,000 | 4.76 s, 4.74 to 4.81 | 3.96 s, 3.90 to 4.07 | 0.823 |
| 40,000 | 31.71 s, 29.97 to 32.49 | 21.58 s, 21.56 to 21.95 | 0.680 |

a trace of after's 40,000 links shows relocking at 0.35 s, from 11.3 s before; placing them still takes 7.5 s, against 0.6 s for 10,000.

placing then still looked for each dataset's folder through every folder named before it, so its time grew with the square of the datasets; a profile of 40,000 links put a quarter of all cpu cycles in comparing folder names there. since 937fa9b, `add -i` leaves the datasets it adds out of those lines but for `-v`, so the search now runs when `sync` places many datasets, as in a fresh copy of such a project. it now finds each folder with one map lookup and still names the folders in the order they first come. release builds of 74b0d10 (before) and of the change (after), three rounds each, the order swapped each round, at a load average of 6 to 8. sync is `sync` into an empty folder holding only the project's data.toml and data.lock, with every file in the cache; add is the `add -i` above. the whole run's time and peak RSS come from `/usr/bin/time`, placing's from the `place` span of a traced run of its own:

| command | links | measure | before, median and range | after, median and range | after/before |
|---|---:|---|---|---|---:|
| sync | 10,000 | whole run | 0.91 s, 0.91 to 0.93 | 0.70 s, 0.67 to 0.70 | 0.769 |
| sync | 10,000 | placing | 0.40 s, 0.39 to 0.40 | 0.16 s, 0.16 to 0.17 | 0.400 |
| sync | 10,000 | peak RSS | 45.9 MiB, 45.5 to 46.1 | 46.2 MiB, 46.2 to 46.4 | 1.008 |
| sync | 40,000 | whole run | 9.68 s, 9.42 to 9.75 | 6.33 s, 6.18 to 6.61 | 0.654 |
| sync | 40,000 | placing | 4.04 s, 3.60 to 4.37 | 0.68 s, 0.66 to 0.70 | 0.168 |
| sync | 40,000 | peak RSS | 145.6 MiB, 145.4 to 146.3 | 145.9 MiB, 145.7 to 146.2 | 1.002 |
| add | 10,000 | whole run | 4.13 s, 3.65 to 4.68 | 4.35 s, 3.67 to 4.44 | 1.053 |
| add | 40,000 | whole run | 14.71 s, 14.69 to 15.14 | 15.02 s, 14.94 to 15.26 | 1.021 |

placing in sync now grows about in step with the datasets, and the time saved is cpu time: user time fell from 9.23 s to 5.84 s for 40,000. add's placing took the same both ways, 0.16 to 0.17 s for 10,000 links and 0.68 to 0.73 s for 40,000, since its datasets skip the search. its 40,000-link run came out 2% slower after, with after going second in two of the three rounds; six more rounds with the order balanced gave 15.34 s before and 15.05 s after. peak RSS rose 0.4 MiB for 10,000 datasets and stayed within its spread for 40,000. a sync of 2,000 datasets, three of them sharing a folder, printed the same lines byte for byte from both builds.

## a second run on a shared cache, 2026-11-12

j6-m4's shape at a smaller size: run A syncs a pinned 1 GiB file and 1,000 files of about 100 KB from a server on this machine paced to 40 MB/s, into an empty cache; 3 s later run B syncs the 1,000 files in another project on the same cache. before, A named its finished downloads 256 at a time or when every one of its downloads ended, so B waited on A's last group until A's large file was done. one release build of 28c136b with the change behind a switch ran each variant in every round, the order rotated each round; three rounds each, the bound's own rows two. nvme is ext4 on the machine's nvme drive; root disk is ext4 on its shared cloud disk, where a `dd` left 3 GB of another program's unwritten data just before A started.

| disk | group waiting at most | flush | B, median and range | A, median |
|---|---|---|---|---:|
| nvme | no limit (before) | filesystem | 26.44 s, 26.43 to 26.44 | 29.22 s |
| nvme | 1 s | filesystem | 12.87 s, 11.86 to 12.88 | 29.15 s |
| nvme | 1 s | each file of the group | 12.11 s, 11.87 to 20.15 | 29.19 s |
| nvme | 0.5 s | filesystem | 12.12 s, 12.12 to 12.12 | 29.16 s |
| nvme | 2 s | filesystem | 13.37 s, 13.37 to 13.37 | 29.20 s |
| root disk, 3 GB unwritten | no limit (before) | filesystem | 56.99 s, 56.63 to 72.41 | 59.86 s |
| root disk, 3 GB unwritten | 1 s | filesystem | 50.41 s, 49.97 to 51.68 | 59.19 s |
| root disk, 3 GB unwritten | 1 s | each file of the group | 54.31 s, 51.02 to 55.09 | 59.08 s |

B can take a file only once A has downloaded it, so its wait is A's time for the 1,000 files plus the bound. on the root disk both runs end with a flush of the whole filesystem, which waits for the other program's 3 GB whichever way a waiting group flushes; flushing the filesystem early wrote part of it sooner, and B ended 3.9 s before flushing each file would have let it. the same build, one run alone on nvme:

| workload | no limit | 1 s, filesystem | 1 s, each file |
|---|---:|---:|---:|
| 10,000 files of about 100 KB, unpaced | 3.90 s, 3.80 to 4.50 | 3.96 s, 3.83 to 3.99 | 4.29 s, 4.00 to 4.68 |
| 300 files paced to 4 MB/s | 7.46 s, 7.46 to 7.49 | 7.42 s, 7.28 to 7.48 | 7.45 s, 7.42 to 8.73 |

the change keeps a 1 s bound and flushes a waiting group as a full one is flushed, with one `syncfs`: B's wait fell by half on nvme and by a tenth on the busy root disk, A and the runs alone did not change beyond their noise, and a group flushed file by file was not reliably faster for B on nvme, one of its runs taking 20.15 s, and was slower on the busy disk and with many files.

## upgrading a bucket prefix, 2026-11-12

`lock --upgrade` of a dataset pinning five tar shards of 2,037,760 bytes from an s3-compatible stand-in on this machine, after a sixth appeared there. the stand-in states each object's size and an ETag in its listing and ignores conditional requests, as wf-audio's did; such a service's ETag is no MD5. the counts are the upgrade's alone, from `--json` work counters; one run each, since they do not vary.

| build | stand-in | requests | bytes downloaded | objects written |
|---|---|---:|---:|---:|
| c9da2d6 | ignores conditional requests | 7 | 12,228,135 | 126 |
| reuse by listed ETag and size | ignores conditional requests | 2 | 2,039,335 | 121 |
| c9da2d6 | answers a matching `If-None-Match` with 304 | 7 | 2,039,335 | 121 |
| reuse by listed ETag and size | answers a matching `If-None-Match` with 304 | 2 | 2,039,335 | 121 |

the two requests are the listing and the new shard. the stand-in that answers 304 sent no bytes for the five old shards before the change either, but took a request for each.

## commands that name a dataset, 2026-11-12

`path <name>` cost what the whole project costs. in a settled project of three imported datasets of 3, 1,000 and 10,000 files, `path` of the 3-file one took 47 ms on the glibc build, and a trace put 6.7 ms in reading data.lock, 7.5 ms in planning, 12.2 ms in reading the state and 14.9 ms in listing the data folder, each for every dataset. beside one dataset of 100,000 files, whose list data.lock keeps in data.lock.d, it took 0.47 s. the changes below make `path` and a settled `sync <name>` read only the pins of the named datasets, of what their recipes read and, for `path`, of datasets whose folders meet theirs, which a clash with their files could involve; only the state's records inside their folders; and only a listing of those folders. `list <name>` and `export <name>` read only the named pins.

both projects were git repositories on ext4 over nvme, settled before the runs. each change ran behind a switch in one release build, each arm adding one change to those before it, every arm once a round in a seeded shuffled order, 12 to 30 measured rounds a case; then the release builds of b3b182a and of the changes. a ratio is the median of new over old with its 95% paired bootstrap interval from `bench/ab.py`. a change stayed if an interval where it aimed ended below 0.97 and no case's interval for time or peak memory started above 1.02; batches that ran under a burst of other load ran again. the static musl build comes first, glibc after the semicolon.

| change | against the arm before it | outcome |
|---|---|---|
| list only the named dataset's folder, and what stands on the way down to it | `path` of 3 files among 11,000 0.654 [0.649, 0.664]; 0.672 [0.668, 0.684]. beside 100,000 files 0.648; 0.636 | kept |
| read from the state only the records inside that folder, passing over the others unbuilt | 0.629 [0.624, 0.636]; 0.740 [0.730, 0.749]. beside 100,000 files 0.570; 0.696, peak memory 0.811; 0.799 | kept |
| read only the pins path needs, and no other list in data.lock.d | 0.647 [0.644, 0.659]; 0.629 [0.622, 0.634]. beside 100,000 files 0.197; 0.240, peak memory 0.417; 0.435 | kept |
| pass over the lines of the other pins' files in data.lock unread | 0.626 [0.619, 0.637]; 0.713 [0.689, 0.722], peak memory 0.666; 1.000. flat where data.lock lists no other files itself | kept |
| answer a settled `sync <name>` from the named datasets alone | `sync` of 3 files among 11,000 0.567 [0.563, 0.572]; 0.595 [0.587, 0.602]. beside 100,000 files 0.370; 0.351 | kept |
| read the number of processors once a run, not the cgroup's cpu limit for each batch of work | 0.997 [0.985, 1.005]; 1.002 [0.988, 1.017], and no other case moved by 3%: a reading is some 30 system calls | dropped: no case moved past the 3% bar |

all of them, b3b182a against the changes, median times:

| case | musl | glibc |
|---|---|---|
| `path` of 3 files among 11,000 | 45.7 to 7.8 ms, 0.168 [0.166, 0.174], peak memory 0.382 | 48.8 to 11.3 ms, 0.231 [0.228, 0.236], peak memory 0.808 |
| `path` of 3 files beside 100,000 | 415 to 29 ms, 0.071 [0.070, 0.071], peak memory 0.305 | 479 to 50 ms, 0.107 [0.105, 0.109], peak memory 0.349 |
| `sync` of 3 files among 11,000 | 43.1 to 9.5 ms, 0.221 [0.215, 0.224], peak memory 0.392 | 45.8 to 12.9 ms, 0.283 [0.277, 0.287], peak memory 0.866 |
| `sync` of 3 files beside 100,000 | 392 to 30 ms, 0.077 [0.074, 0.078], peak memory 0.394 | 423 to 52 ms, 0.124 [0.118, 0.127], peak memory 0.415 |
| `list` and `export` of 3 files beside 100,000 | 125 to 3.5 ms and 62 to 3.4 ms | 162 to 3.7 ms and 77 to 3.8 ms |
| `path` and `sync` of the 10,000 files among 11,000 | 0.917 [0.900, 0.921] and 0.924 [0.915, 0.942] | 0.926 [0.909, 0.939] and 0.932 [0.918, 0.942] |
| `path` of the 100,000 files | 0.972 [0.959, 0.989] | 0.973 [0.952, 1.028] |
| `sync` and `status` of all, in either project, and `--version` | 0.967 to 0.999 | 0.990 to 1.004 |

no peak memory interval started above 1.001. beside 100,000 files, `path` of 3 still reads all of the state's 24 MB of json to pass over its records: 25 ms on musl and 45 ms on glibc, nearly all of what is left. going further needs a state that keeps each dataset's records apart.

## the end of a run, 2026-11-12

a run ended its tokio runtime with `shutdown_background`, so `main` returned while the runtime's threads were still exiting: a median of 9 of them at the end of a sync of 32 links. on windows, returning from `main` ends those threads wherever they are and then runs main's thread-local destructors, which under `-vv` take the same process-wide locks the threads take as they exit, and a ci run on fat32 hung after its last line. `end_runtime` now waits for them, at most `EXIT_WAIT`. one experiment build of a8f9dad chose the end at run time, `FETCHLOOM_EXP_EXIT_<ms>=1` for a bound of that many milliseconds and none for `shutdown_background`, and logged how long the end took. abh.py ran the arms in interleaved rounds with the measure lock held, on cores 12-23, or on two of them, alone or each shared with one or three busy loops. tokio joins the threads only when every one lets go within the bound, so an end shorter than the bound joined them all.

| run | build | wall, `shutdown_background` and with a bound | end of the run, median and slowest | joined |
|---|---|---|---|---:|
| sync of 32 links of 100 KB from an empty cache | glibc | 27.69 ms, 27.93 ms | 0.35 ms, 3.51 ms | 180 of 180 |
| the same | musl | 27.86 ms, 27.58 ms | 0.41 ms, 1.68 ms | 180 of 180 |
| settled sync of the 32 | glibc | 4.78 ms, 4.83 ms | 0.09 ms, 3.30 ms | 300 of 300 |
| the same | musl | 4.92 ms, 4.95 ms | 0.13 ms, 1.29 ms | 300 of 300 |
| sync of 32, 2 cores | glibc | 29.38 ms, 29.24 ms | 0.45 ms, 3.08 ms | 120 of 120 |
| settled, 2 cores | glibc | 4.88 ms, 4.97 ms | 0.10 ms, 2.60 ms | 200 of 200 |
| sync of 32, 2 cores with a busy loop each | glibc | 54.12 ms, 55.13 ms | 1.45 ms, 5.20 ms | 120 of 120 |
| settled, 2 cores with a busy loop each | glibc | 9.66 ms, 11.41 ms | 1.59 ms, 1.98 ms | 200 of 200 |
| sync of 32, 2 cores with three busy loops each | glibc | 116.89 ms, 122.18 ms | 1.74 ms, 8.65 ms | 120 of 120 |
| settled, 2 cores with three busy loops each | glibc | 24.31 ms, 24.73 ms | 6.01 ms, 10.09 ms | 200 of 200 |

walls are medians of 30 or 50 runs an arm; "with a bound" pools the bounds tried, which joined alike: 10, 25, 50, 100, 250 and 1,000 ms on 12 cores, 10 to 100 ms on 2. one end, 10.09 ms in the last row, passed 10 ms, so 25 ms is the smallest bound tried that every end fit in. on a loaded machine the wait shows in the wall, by about the end's own time.

a stopped run pays the whole bound when a blocking task outlives it: `status --verify` of a 4 GiB file on tmpfs, stopped by SIGINT once 512 MiB were read, its hash left running as a stop leaves a scan, eight runs an arm:

| end | `Stopped` to exit, glibc | `Stopped` to exit, musl |
|---|---:|---:|
| `shutdown_background` | 0.93 ms | 1.05 ms |
| 10 ms | 10.98 ms | 11.14 ms |
| 25 ms | 25.99 ms | 26.08 ms |
| 50 ms | 51.08 ms | 51.15 ms |
| 100 ms | 101.12 ms | 101.27 ms |
| 250 ms | 251.11 ms | 251.08 ms |
| 1 s | 1,001.12 ms | 1,001.12 ms |

a second SIGINT sent as `Stopped` showed did not shorten the wait at 100 ms or 1 s, since the run's listener goes with the runtime; a second ctrl-c while the command still runs exits 130 from a runtime thread without the wait.

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

# performance

the 1 GiB local-download gap to curl and the windows placement budget remain open. split downloads help slow individual streams; they do not remove hashing, flushing or filesystem costs.

## rules

- stream file bodies through bounded queues. listings, plans and archive member records still grow with file count.
- hash while writing the contiguous download prefix. split pieces received beyond it are read back when the prefix reaches them.
- avoid a HEAD before a GET and request each listing page once. [architecture](architecture.md#split-downloads) defines when a download adds range requests.
- many small files from one host share a connection over HTTP/2.
- new objects are flushed to disk before they are put in place under their hash, on several threads at once. publication exposes the whole object after its payload flush. on linux 5.8 and later an archive's members are flushed together, with one flush of the cache's filesystem before any is published, as git's batch mode does; one flush per small file cost twenty times more.
- read file metadata from directory listings where they carry it. on windows a listing carries sizes and times and is about 15 times faster than one call per file; linux listings carry neither, so there each file costs one stat, as in git.
- spread placement over folders. making links in one folder serializes on that folder; neighbours in different folders do not wait for each other.
- parallel work is bounded: a fixed number of downloads, hashing threads and placement workers, never one task per file.
- startup does only what the command needs. TLS roots, proxy settings and sources load on first use.
- reuse buffers in hot loops.

## budgets

release builds on 2026-10-30 and 2026-10-31, on one laptop: an 11th gen intel i7-1185G7 (4 cores, 8 threads), 32 GB, nvme. windows is windows 11 with defender's real-time protection on; linux is ubuntu 24.04 in WSL 2 on ext4. baseline timings at f9bbda5 use at least ten runs after three warm-ups, with no other work running. follow-ups below give their own run counts and load conditions. ranges are observed minima and maxima.

| what | budget | windows | linux |
|---|---|---|---|
| `fetchloom --version` | under 5 ms on linux, within noise of an empty program on windows | 13.9 ms, an empty program 12.1 ms | 1.1 ms |
| first line of output | under 100 ms | 29 ms (`get` while the server waits), 64 ms (`status`) | 3.9 ms, 42 ms |
| `sync` with nothing to do, 10,000 files | under 200 ms | 112 ms | 50 ms |
| `status`, 10,000 files | under 150 ms | 89 ms | 42 ms |
| placing 10,000 cached files into a new project | under 300 ms on linux, under 2 s on windows | about 9 s in one run under load; earlier quiet median 2.74 s: a miss | 126 ms |
| reading and checking a `data.lock` of 10,000 files | under 30 ms | 26 ms of work in a 38 ms command (an empty program takes 12 ms) | 10.7 ms |
| SHA-256 | at least 1 GB/s on one core with hardware support | 1.08 GB/s in the best `cache verify` of 1 GiB, 0.33 GB/s in the median run | 1.33 GB/s |
| memory used by downloads | a fixed buffer per running download, under 50 MB in total | 17 MiB for a 1 GiB download | 26 MiB |
| peak memory with 100,000 files in a project | under 150 MB | 108 MiB adding, 75 MiB `status` | 165 MiB adding, 151 MiB placing at 33825eb: misses; 77 MiB `status` in the baseline |
| one large file on a fast link | limited by the link, not by hashing or disk | 1 GiB from this machine in 5.8 s, curl 2.9 s | 2.2 s, curl 1.0 s |
| unpacking an archive of large members | within 2 times of bsdtar extracting it to disk | 512 MiB tar.zst: 2.2 s, bsdtar 1.7 s; zip: 2.4 s, bsdtar 1.2 s | tar.zst: 1.6 s, bsdtar 1.0 s; zip: 2.3 s, bsdtar 6.1 s |
| unpacking many small files | within 2 times of creating, writing and flushing the same files | 10,000 random 4 KiB files: 34 to 39 s; bsdtar 11 to 13 s on a different defender scan path | 0.47 to 0.56 s, bsdtar 0.59 s |
| cold download of a plain URL | one request unless redirects or [split ranges](architecture.md#split-downloads) are needed | counted in tests | counted in tests |
| a source's requests | only those its protocol needs, each counted in tests | counted in tests | counted in tests |
| a paged file listing | one request per page | counted in tests | counted in tests |

### local downloads

a 1 GiB download from a server on the same machine still takes about twice curl's transfer-only time. fetchloom also hashes each byte and flushes the object before placing it. the baseline moved about 180 MB/s on windows and 460 MB/s on linux.

hashing in memory on this CPU, best of three over 1 GiB: sha2's SHA-256 1.70 to 1.83 GB/s, ring's 1.63 to 1.69, MD5 0.68, and SHA-256 with MD5 in one pass 0.48. sha2 stays. a download computes MD5 only when no SHA-256 is pinned.

### many small downloads

1,000 URLs of 100 KiB each, one dataset per URL, `sync` into an empty cache from the lab server on the same machine (lan profile, linux): each file cost three flushes, its object, its resume checkpoint and its URL record, about 3.3 ms each on WSL's ext4 with four requests per host. a checkpoint or URL record lost in a power cut costs only a resume or a conditional request, and a torn one reads as absent, so those two and the MD5 index entries are now renamed into place without a flush. objects, change marks, unpack records and project registrations keep theirs. ten interleaved pairs at 0533392: 4.46 s (4.27 to 6.31) before, 2.18 s (2.09 to 3.87) after, 51% faster.

### small files on windows

defender's performance recorder, read with `Get-MpPerformanceReport`, traced unpacking `files-10k.tar.zst`: 10,000 files of 4 KiB random bytes. every staged member was scanned on close. it recorded 10,000 scans with a median of 21 ms and 228 s of scan time across eight writers; unpacking took 34 to 39 s. the scan times overlap, so their sum is not wall time.

skipping the per-member flush did not remove the scans: still 10,000, median 24 ms, and unpacking took 54 s in that run. bsdtar, windows' `tar.exe`, extracted the same archive in 11 to 13 s with no on-close scans of its files.

controlled probes wrote 5,000 files of 4 KiB from eight processes:

| probe | result |
|---|---|
| random bytes | 16.0 s |
| low-entropy bytes | 4.5 to 5.6 s |
| created read-only or writable | no change |
| `.new` or `.bin` names | no change |

a python script running in a signed interpreter wrote random `.new` files at 0.6 ms each on one thread.

the floor is defender scanning random-looking content written by a program it does not yet trust. `tar.exe` is a trusted windows binary. application code cannot legitimately remove that scan. signed releases that earn reputation, or a dev drive with performance mode, can lower the cost. fetchloom never asks for exclusions.

publishing each member by rename instead of hardlink plus remove would save about 1 s of a 34 s unpack. that is below the 10% bar, so the change was not made.

on linux, one flush per file cost 0.7 ms on ext4. flushing the batch took unpacking from 8.5 to 10.2 s down to about half a second at 26a3dbc, three runs each. [architecture](architecture.md#concurrency) describes the worker budgets; [archive storage](../../src/archive/store.rs) owns the batch flush.

### memory

on linux at 9d3c7b1, adding a 100,000-member zip of 1 MiB peaked at 162 MiB RSS. placing its 100,000 files from the cache peaked at 183 MiB.

heaptrack put most of the peak in placement retaining a full `Metadata` per placed file. at 33825eb, `place_all` keeps only size and time for project placement.

with the same zip on linux, `/usr/bin/time` measured maximum RSS at 151 MiB for placing from cache, down from 183 MiB. adding peaked at 165 MiB against 162 MiB before, within run-to-run spread. both still miss the 150 MB budget.

heaptrack put the add peak's heap at 116 MiB. glibc keeps per-thread malloc arenas for the eight unpack writers; `MALLOC_ARENA_MAX=2` lowered the add peak to 143 MiB. setting that cap in the program needs an unsafe `mallopt` call or another allocator, so the allocator behavior stays unchanged.

### placing from a warm cache on windows

live use found placing slow on windows: a second project of hf:datasets/Matthijs/cmu-arctic-xvectors, a zip of about 8,000 files in one folder, took 30.5 s against 3.8 s on linux, and a tar.gz of about 52,000 small files took 87 s, both on a debug build under load. two fixtures stand for them: 10,000 files of 4 KiB in 100 folders from the lab server, and 8,000 files of 2 KiB in one folder from a local server. each run copies data.toml and data.lock into an empty folder and syncs it from a cache that holds every file. these runs, on 2026-10-30, shared the laptop with other builds that kept it at full load, so every comparison interleaves ten runs of the old release binary (fd64035) and the new one (2ecb70a) and gives the median and the range. besides the time from start to exit, each run's `FETCHLOOM_TRACE` gives the time inside the command, since starting any program took 1 to 3 s at that load.

phase timings from one trace per fixture:

| phase | 10,000 files in 100 folders | 8,000 files in one folder |
|---|---|---|
| whole command | 3.85 s | 12.1 s |
| checking each object in the cache | 0.47 s | 0.29 s |
| placing | 3.19 s | 11.7 s |
| planning, the state and the rest | 0.19 s | 0.12 s |

bare-link probes and placement experiments:

- one folder is the cost. 8,000 links into one folder took 10.2 to 12.3 s, and the same objects spread over 100 folders 1.7 s; 10,000 links took 10.3 to 12.7 s into one folder and 2.2 s over 100. on one thread a link into one folder took 0.6 to 3.7 ms, with no trend as the folder filled.
- fewer threads are slower, not faster. 8,000 links into one folder, interleaved: one thread 17 to 149 s, two 10 to 49 s, four 8.5 to 20 s and eight 10 to 19 s. placement keeps 8 threads.
- reading each file back costs little. 10,000 links spread on 8 threads took 2.48 s without reading any file back and 2.44 s reading each. a build that took each hardlink's size and time from its object's check instead, reading nothing back, won nothing on either fixture, so that change was reverted.
- checking folders per file costs 5 to 10 us a file, 54 to 97 ms for 10,000, about 2% of placing, below the 10% a change must win, so it stays as it was.

| placing into a new project, ten interleaved runs | before | reading no hardlink back |
|---|---|---|
| 10,000 files in 100 folders, start to exit | 11.4 s (8.0 to 27.1) | 12.3 s (8.4 to 28.4) |
| 10,000 files, inside the command | 10.1 s (7.0 to 25.6) | 10.8 s (7.2 to 27.1) |
| 8,000 files in one folder, start to exit | 20.0 s (14.6 to 27.9) | 16.8 s (13.3 to 29.3) |
| 8,000 files, inside the command | 17.9 s (13.2 to 26.4) | 15.2 s (11.9 to 27.8) |

making 10,000 hardlinks with defender on still limits placement: one loaded run took about 9 s against the 2 s budget.

the build was faster in 3 of 10 interleaved pairs for 10,000 files and in 6 of 10 for 8,000, with single pairs apart by up to 2 times either way, so neither median is outside the spread. placing in this load took 2 to 3 times as long as in the earlier quiet window, and the slow live runs are mostly the cost of making links on windows under load, not work fetchloom adds.

the [settled-sync shortcut](architecture.md#the-project-loop) changed these timings:

| sync with nothing to do | before | after |
|---|---|---|
| 10,000 files in 100 folders, inside the command | 200 ms (153 to 708) | 131 ms (91 to 174) |
| 8,000 files in one folder, inside the command | 303 ms (174 to 480) | 158 ms (102 to 371) |
| 10,000 files, start to exit | 1.27 s (1.11 to 3.18) | 1.21 s (1.07 to 1.61) |
| 8,000 files, start to exit | 1.53 s (1.41 to 5.48) | 1.82 s (1.23 to 12.8) |

## downloads over the network

lab clients ran on windows against the server's eth0 address through the `tc` profiles in [benchmarks](benchmarks.md#workloads), with an empty cache per run. live runs used the named public service. all comparisons here used release builds on the same laptop on 2026-10-30 or 2026-10-31. paired runs alternated the compared configurations; times are medians with observed ranges.

### adaptive splitting

0cbc48b starts a download as one stream and splits once that stream moves under 1 MB/s for 2 s, subject to the [range and size checks](architecture.md#split-downloads).

| workload | interleaved pairs | one-stream build | adaptive | result |
|---|---|---|---|---|
| lan, 1 GiB | 10 | 5.54 s (5.00 to 9.45) | 5.48 s (5.07 to 6.12) | no split |
| home, 1 GiB | 10 | 97.55 s (96.39 to 106.79) | 97.82 s (96.94 to 99.25) | no split |
| far, 100 MiB | 5 | 496.65 s (495.82 to 518.50) | 174.03 s (135.81 to 181.26) | 4 requests, 65% faster |
| live cifar-10, `www.cs.toronto.edu` | 3 | 156.29 s (118.58 to 178.45) | 39.81 s (37.23 to 40.81) | 74% faster |

the toronto one-stream download ran at about 1 MB/s that night. these live results describe those three pairs, not a fixed speed for the service.

an earlier 8 MB/s trigger at fafec0c split a toronto download whose one stream ran at 5.6 MB/s. over two pairs, the one-stream median was 29.16 s and the adaptive median 38.86 s, 33% later. the 8 MB/s build was neutral on home and lan, but under machine load it split one lan download. those regressions are why the trigger is 1 MB/s. a `FETCHLOOM_RATE` cap stops a stream from being judged slow.

### splitting from the first answer

1583af9 split eligible files from the first answer on 2026-10-30:

| profile | file | one stream | split | requests | change |
|---|---|---|---|---|---|
| lan | 1 GiB | 5.96 s (5.01 to 7.41) | 7.00 s (6.99 to 8.57) | 6 | 17% slower |
| home | 1 GiB | 95.6 s (95.5 to 96.2) | 96.7 s (95.6 to 97.4) | 4 to 5 | 1% slower |
| far | 100 MiB, 3 runs per configuration | 500 s (490 to 531) | 215 s (191 to 285) | 2 to 4 | 57% faster |

on home, one stream already filled the link. on lan, it moved about 180 MiB/s and the split runs still added streams. the adaptive build leaves those fast streams alone.

### HTTP/2 on hugging face

`hf:datasets/cais/mmlu` without `data.tar`, 180 files and 99 MiB, live, five interleaved pairs with the same binary:

| protocol | median and range |
|---|---|
| HTTP/2 | 45.32 s (42.69 to 49.50) |
| HTTP/1.1 | 46.03 s (45.31 to 48.19) |

the difference is within noise, so HTTP/2 stays. an earlier single HTTP/2 run took 92 s with eight retries; the paired runs did not reproduce it.

## against other tools

the method is [benchmarks](benchmarks.md). on 2026-10-31 a reduced run used 3 blocks instead of 5, on linux only, with release build 240f17a, which predates the flush change above. the windows lab did not fit: the harness alone needs about 12 minutes after each run that leaves 10,000 files on windows, as [the plans](../../bench/plans/README.md) explain. the far and poor profiles and the live subset were not run in this pass. medians, wall time:

| workload, profile | fetchloom | others |
|---|---|---|
| 1 GiB file, lan | 4.00 s, second project 0.01 s | aria2c 1.90 s, curl with sha256sum 3.36 s, datalad 5.51 s, dvc 6.13 s (second 2.56 s), pooch 8.83 s (second 0.91 s) |
| 100 MiB file, lan | 0.35 s | aria2c 0.23 s, curl with sha256sum 0.41 s, pooch 1.13 s, datalad 1.71 s, dvc 2.10 s |
| zip of 10,000 files, lan | 0.87 s, second project 0.20 s | curl with bsdtar 0.66 s, pooch 2.21 s |
| tar.zst of 10,000 files, lan | 0.76 s | curl with bsdtar 0.59 s |
| 1,000 URLs of 100 KiB, lan | 3.53 s; 2.18 s after the flush change | curl parallel 0.30 s, aria2c 8.44 s |
| 1,000 URLs of 100 KiB, home | 9.46 s | aria2c 8.94 s, curl parallel 9.02 s |

every run of every tool fetched the expected bytes. fetchloom also hashes, pins and flushes what it fetches, and links a second project from its cache; curl, aria2c and bsdtar do none of that. aria2c beats it on one large local file, and curl's parallel mode on many small local files, where flushing each object is most of fetchloom's time. on the home profile all three are within 6% of each other.

## regression checks

[work counters](../formats.md#work-counters) let tests catch extra requests, downloads and copies on controlled workloads. elapsed time is not a CI assertion. shared runners and laptop load vary too much for a wall-clock gate.

for a slow command, measure a release build, use the [profiling workflow](engineering.md#profiling) to find its largest cost, then repeat the same workload. keep before and after results with their conditions. a budget change needs the measurement that showed the old budget was wrong.

## build profile

[Cargo.toml](../../Cargo.toml) sets `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` and `strip = true` for release builds. the earlier version measured fat LTO as 7% smaller than thin at the same speed. the allocator stays the system one until a measurement shows a gain worth a dependency; mimalloc was within noise in the earlier version.

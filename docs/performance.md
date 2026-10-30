# performance

fetchloom is fast because of how it is built: it streams, touches each byte once, bounds its parallel work and sends no request it can skip. a profiler then proves it and finds what the design missed.

## rules

- stream everything. no file, listing or response is held whole in memory unless it is known to be small.
- one pass over the bytes: read, hash and write in the same loop.
- the network is the scarcest resource. one request where one is enough, no HEAD before a GET, one request per page of a listing. with `FETCHLOOM_SPLIT=1`, a file of at least 64 MiB gets more requests, for ranges, only while each one makes it faster.
- many small files from one host share a connection over HTTP/2.
- new objects are flushed to disk before they are put in place under their hash, on several threads at once. publishing makes an object appear whole, and the flush makes it survive a power cut. on linux 5.8 and later an archive's members are flushed together, with one flush of the cache's filesystem before any is published, as git's batch mode does; one flush per small file cost twenty times more.
- read file metadata from directory listings where they carry it. on windows a listing carries sizes and times and is about 15 times faster than one call per file; linux listings carry neither, so there each file costs one stat, as in git.
- spread placement over folders. making links in one folder serializes on that folder; neighbours in different folders do not wait for each other.
- parallel work is bounded: a fixed number of downloads, hashing threads and placement workers, never one task per file.
- startup does only what the command needs. TLS roots, proxy settings and sources load on first use.
- hot loops reuse their buffers and do not allocate.

## budgets

measured on release builds. a budget is a number with a unit, or a comparison with a baseline. the measurements below are from 2026-10-30, commit f9bbda5, on one laptop: an 11th gen intel i7-1185G7 (4 cores, 8 threads), 32 GB, nvme. windows is windows 11 with defender's real-time protection on, as people have it; linux is ubuntu 24.04 in WSL 2 on the same laptop, on ext4. each timing is the median of at least ten runs after three warm-up runs, unless the row says otherwise, with nothing else running.

| what | budget | windows | linux |
|---|---|---|---|
| `fetchloom --version` | under 5 ms on linux, within noise of an empty program on windows | 13.9 ms, an empty program 12.1 ms | 1.1 ms |
| first line of output | under 100 ms | 29 ms (`get` while the server waits), 64 ms (`status`) | 3.9 ms, 42 ms |
| `sync` with nothing to do, 10,000 files | under 200 ms | 112 ms | 50 ms |
| `status`, 10,000 files | under 150 ms | 89 ms | 42 ms |
| placing 10,000 cached files into a new project | under 300 ms on linux, under 2 s on windows | **2.74 s**, best 2.15 s: a miss | 126 ms |
| reading and checking a `data.lock` of 10,000 files | under 30 ms | 26 ms of work in a 38 ms command (an empty program takes 12 ms) | 10.7 ms |
| SHA-256 | at least 1 GB/s on one core with hardware support | 1.08 GB/s in the best `cache verify` of 1 GiB, 0.33 GB/s in the median run | 1.33 GB/s |
| memory used by downloads | a fixed buffer per running download, under 50 MB in total | 17 MiB for a 1 GiB download | 26 MiB |
| peak memory with 100,000 files in a project | under 150 MB | 108 MiB adding, 75 MiB `status` | **162 MiB adding**: a miss; 77 MiB `status`, 141 MiB placing |
| one large file on a fast link | limited by the link, not by hashing or disk | 1 GiB from this machine in 5.8 s, curl 2.9 s | 2.2 s, curl 1.0 s |
| unpacking an archive of large members | within 2 times of bsdtar extracting it to disk | 512 MiB tar.zst: 2.2 s, bsdtar 1.7 s; zip: 2.4 s, bsdtar 1.2 s | tar.zst: 1.6 s, bsdtar 1.0 s; zip: 2.3 s, bsdtar 6.1 s |
| unpacking many small files | within 2 times of creating, writing and flushing the same files | 10,000 files of 4 KiB: 37 to 55 s, floor 21 to 24 s: a miss | 0.47 to 0.56 s, bsdtar 0.59 s |
| cold download of a plain URL | one request, plus any redirects the server sends | counted in tests | counted in tests |
| a source's requests | only those its protocol needs, each counted in tests | counted in tests | counted in tests |
| a paged file listing | one request per page | counted in tests | counted in tests |

what the measurements showed:

- a 1 GiB download from a server on the same machine runs at about 180 MB/s on windows and 460 MB/s on linux, since each byte is also hashed and the object is flushed before it is placed. that keeps up with links up to about 1.5 Gbit/s on windows; above that the disk and the hash set the pace. the best windows run matched curl, and the spread between runs came from the antivirus.
- placing hardlinks on ntfs with defender on costs about 130 us a link when neighbours go to different folders on 8 threads, and 400 to 1,100 us when threads fill one folder at a time. 10,000 bare links spread that way take 1.3 s, so the 2 s budget leaves room. the rest of the miss is checking each object in the cache before placing, about 0.5 s for 10,000, and the links themselves, whose cost moves with the machine's load; reading each placed file back and checking its folders cost little, as the next section shows.
- two budgets changed with these measurements. unpacking was "within 15% of the decompressor alone", but every member is also hashed and flushed, and on windows every new file is scanned: bsdtar decompresses the 10,000 small files in 0.18 s and writes them in 8.4 s. small members are now measured against a program that does nothing but create, write, flush and link the same bytes. on linux one flush per file cost 0.7 ms on ext4; flushing the batch at once took unpacking from 8.5 to 10.2 s down to half a second (commit 26a3dbc, three runs each). the batch holds the filesystem open from before its first write, since linux reports a failed write-back only to descriptors opened before it; kernels before 5.8 report none to syncfs, so there each file is flushed on its own.
- on windows defender scans each new file as it is closed, and for small files that scan sets the pace. the machine's load moved the cost of one file between 0.7 and 7 ms from minute to minute, so each comparison below interleaves its runs.
  - signing does not matter. microsoft's tar and a copy whose signature an appended byte broke wrote the same 10,000 files in 19.6 and 19.4 s.
  - writers help up to 8. writing took 44 to 51 s on 1 thread and 22 to 29 s on 2 to 8; linking into the cache took 14 to 82 s on 1 to 4 threads and 6 to 7 s on 8; 16, 32 and 64 were slower than 8. mercurial and rustup close files on a thread pool for the same reason.
  - a file no one else may open while it is written is scanned sooner on one thread: creating it unshared, as bsdtar does, took a file from 1.15 to 1.46 ms down to 0.95 to 1.23 ms. fetchloom's 8 writers hide that: unpacking 1,000 files took 2.2 s with and without it over six interleaved runs each, so the default stays.
  - bsdtar still writes such a file in about 0.7 ms, and why was not found: defender's performance recorder, which would show it, needs an administrator. under the same load fetchloom unpacked the 10,000 files in 27 to 31 s and bsdtar extracted them in 19 s.
  - the rest belongs to the machine's owner: a dev drive, on which defender scans after the file operation returns, or an exclusion for the cache folder, as pnpm suggests for its store. both need an administrator and were not measured here.
- `lock --check` reads and checks 10,000 locked files in about 26 ms on windows; starting any program there takes 12 ms.
- hashing in memory on this cpu, best of three over 1 GiB: sha2's SHA-256 1.70 to 1.83 GB/s, ring's 1.63 to 1.69, MD5 0.68, and SHA-256 with MD5 in one pass 0.48. so sha2 stays, and a download computes an MD5 only when no SHA-256 is pinned: bytes that match the pin match the MD5 recorded beside it.

### placing from a warm cache on windows

live use found placing slow on windows: a second project of hf:datasets/Matthijs/cmu-arctic-xvectors, a zip of about 8,000 files in one folder, took 30.5 s against 3.8 s on linux, and a tar.gz of about 52,000 small files took 87 s, both on a debug build under load. two fixtures stand for them: 10,000 files of 4 KiB in 100 folders from the lab server, and 8,000 files of 2 KiB in one folder from a local server. each run copies data.toml and data.lock into an empty folder and syncs it from a cache that holds every file. these runs, on 2026-10-30, shared the laptop with other builds that kept it at full load, so every comparison interleaves ten runs of the old release binary (fd64035) and the new one (2ecb70a) and gives the median and the range. besides the time from start to exit, each run's `FETCHLOOM_TRACE` gives the time inside the command, since starting any program took 1 to 3 s at that load.

where the time went, from traces of one run each:

| phase | 10,000 files in 100 folders | 8,000 files in one folder |
|---|---|---|
| whole command | 3.85 s | 12.1 s |
| checking each object in the cache | 0.47 s | 0.29 s |
| placing | 3.19 s | 11.7 s |
| planning, the state and the rest | 0.19 s | 0.12 s |

what a benchmark of bare links outside fetchloom showed, and what one build of fetchloom tried:

- one folder is the cost. 8,000 links into one folder took 10.2 to 12.3 s, and the same objects spread over 100 folders 1.7 s; 10,000 links took 10.3 to 12.7 s into one folder and 2.2 s over 100. on one thread a link into one folder took 0.6 to 3.7 ms, with no trend as the folder filled.
- fewer threads are slower, not faster. 8,000 links into one folder, interleaved: one thread 17 to 149 s, two 10 to 49 s, four 8.5 to 20 s and eight 10 to 19 s. placement keeps 8 threads.
- reading each file back costs little. 10,000 links spread on 8 threads took 2.48 s without reading any file back and 2.44 s reading each. a build that took each hardlink's size and time from its object's check instead, reading nothing back, won nothing on either fixture, as the table below shows, so that change was reverted.
- checking folders per file costs 5 to 10 us a file, 54 to 97 ms for 10,000, about 2% of placing, below the 10% a change must win, so it stays as it was.

| placing into a new project, ten interleaved runs | before | reading no hardlink back |
|---|---|---|
| 10,000 files in 100 folders, start to exit | 11.4 s (8.0 to 27.1) | 12.3 s (8.4 to 28.4) |
| 10,000 files, inside the command | 10.1 s (7.0 to 25.6) | 10.8 s (7.2 to 27.1) |
| 8,000 files in one folder, start to exit | 20.0 s (14.6 to 27.9) | 16.8 s (13.3 to 29.3) |
| 8,000 files, inside the command | 17.9 s (13.2 to 26.4) | 15.2 s (11.9 to 27.8) |

the build was faster in 3 of 10 interleaved pairs for 10,000 files and in 6 of 10 for 8,000, with single pairs apart by up to 2 times either way, so neither median is outside the spread. placing in this load took 2 to 3 times as long as in the earlier quiet window, and the slow live runs are mostly the cost of making links on windows under load, not work fetchloom adds.

a sync with nothing to do now skips reading archive member records, relocking and planning when data.toml and data.lock are as the last complete sync left them, as [the project loop](architecture.md#the-project-loop) describes. the scan of the data folder still runs.

| sync with nothing to do | before | after |
|---|---|---|
| 10,000 files in 100 folders, inside the command | 200 ms (153 to 708) | 131 ms (91 to 174) |
| 8,000 files in one folder, inside the command | 303 ms (174 to 480) | 158 ms (102 to 371) |
| 10,000 files, start to exit | 1.27 s (1.11 to 3.18) | 1.21 s (1.07 to 1.61) |
| 8,000 files, start to exit | 1.53 s (1.41 to 5.48) | 1.82 s (1.23 to 12.8) |

## downloads over the network

these runs went from windows to the network lab's server by its eth0 address, through its `tc` profiles. each run was `fetchloom -v get` of one file into an empty cache, and runs of one profile alternated between one stream, `FETCHLOOM_SPLIT=0`, and splitting, which that build did by default.

### split downloads

release build 1583af9, 2026-10-30. the times are medians, with the range in brackets.

| profile | file | one stream | split | requests | change |
|---|---|---|---|---|---|
| lan | file-1g.bin | 5.96 s (5.01 to 7.41) | 7.00 s (6.99 to 8.57) | 6 | 17% slower |
| home | file-1g.bin | 95.6 s (95.5 to 96.2) | 96.7 s (95.6 to 97.4) | 4 to 5 | 1% slower |
| far | file-100m.bin | 500 s (490 to 531), 3 runs | 215 s (191 to 285), 3 runs | 2 to 4 | 57% faster |

splitting won on far, where loss holds one TCP connection to about 2 Mbit/s, and lost on lan and home, so it is off unless `FETCHLOOM_SPLIT=1` asks for it: the bar was a 10% win on one profile and a loss on none. on home one stream already fills the link, and the second stream's range is read back to be hashed once the prefix reaches it. why lan lost a second was not found in the time box. far had three runs a side, not five: at 15:27 another session's lab traffic began and its `net.sh lan` removed the shaping during the fourth pair, and there was no time to repeat them. every run between then and 15:50 was discarded, and the home and lan rows were run again from 15:50 with the lab otherwise idle.

### rate limits

`fetchloom add zenodo:3684992 --yes`, 800 files and 19.3 MiB with no token, into an empty folder with an empty cache, over the internet with a debug build of 7a96361. before the change it failed after about 160 requests with exit 20: zenodo answered 429 once its 133 requests a minute were used up, and a request that met five 429s gave up. now it finished in 6 min 16 s with 836 requests, 35 of which zenodo answered 429 and fetchloom waited out without spending a try. each of zenodo's windows took 10 to 20 s of downloading and a wait of 30 to 48 s. zenodo also refused a few requests in the seconds after most resets, with 429s that asked for 1 to 4 s; those waits now pass without a line of their own.

## counters

every run counts its work: requests, bytes downloaded, retries, resumes, objects written, cache hits, files placed by each method and the total time. every counter except the time is deterministic, so tests assert them on every commit. a change that adds a request, or copies a file that should have been linked, fails CI.

`-v` prints the counters at the end of a run, and `--json` includes them. `FETCHLOOM_TRACE=run.json` writes a timeline of the run's phases, such as reading the lock, scanning the data folder, placing and writing the state, which perfetto opens.

## tools

| need | tool |
|---|---|
| CPU profile | samply on linux; on windows it needs the windows performance toolkit |
| timeline of one run | `FETCHLOOM_TRACE=run.json`, opened in perfetto |
| whole commands | hyperfine, warmed up, ten runs or more |
| CI regression gate | the counters, asserted in tests |

there is no heap profiler, micro-benchmark harness or instruction-count gate. add one with the job it will run. wall clock time is recorded here and in pull requests, never used to fail CI: shared runners and laptops vary too much.

## when something is slow

1. measure it with hyperfine on a release build and write the number down.
2. profile it with samply or a trace. find where the time goes before changing anything.
3. fix the largest cost, measure again, and put both numbers in the pull request.
4. a budget that proves wrong changes in this file, together with the measurement that showed it.

## build profile

release builds use `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` and `strip = true`. the earlier version measured fat LTO as 7% smaller than thin at the same speed. the allocator stays the system one until a measurement shows a gain worth a dependency; mimalloc was within noise in the earlier version.

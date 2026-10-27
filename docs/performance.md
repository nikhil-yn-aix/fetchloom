# performance

fetchloom is fast because of how it is built: it streams, touches each byte once, bounds its parallel work and sends no request it can skip. a profiler then proves it and finds what the design missed.

## rules

- stream everything. no file, listing or response is held whole in memory unless it is known to be small.
- one pass over the bytes: read, hash and write in the same loop.
- the network is the scarcest resource. one request where one is enough, no HEAD before a GET, one request per page of a listing.
- many small files from one host share a connection over HTTP/2.
- new objects are flushed to disk in parallel batches before they are renamed into place, never one blocking flush at a time. the rename makes an object appear whole, and the flush makes it survive a power cut.
- read file metadata by listing directories, never one call per file. on windows the listing is about 15 times faster.
- parallel work is bounded: a fixed number of downloads, hashing threads and placement workers, never one task per file.
- startup does only what the command needs. TLS roots and sources load on first use.
- hot loops reuse their buffers and do not allocate.

## budgets

measured on release builds. a budget is a number with a unit, or a comparison with a baseline.

| what | budget |
|---|---|
| `fetchloom --version` | under 5 ms on linux and macos, within noise of an empty program on windows |
| first line of output | under 100 ms |
| `sync` with nothing to do, 10,000 files | under 200 ms |
| `status`, 10,000 files | under 150 ms |
| placing 10,000 cached files into a new project | under 300 ms on linux, under 2 s on windows with defender on |
| cold download of a plain URL | one request, plus any redirects the server sends |
| a source's requests | only those its protocol needs, each counted in tests |
| a paged file listing | one request per page |
| one large file on a fast link | limited by the link, not by hashing or disk |
| SHA-256 | at least 1 GB/s on one core with hardware support |
| unpacking zip or tar.zst | within 15% of the decompressor alone |
| reading and checking a `data.lock` of 10,000 files | under 30 ms |
| memory used by downloads | a fixed buffer per running download, under 50 MB in total |
| peak memory with 100,000 files in a project | under 150 MB |

an earlier version of fetchloom measured most of these on an 11th gen intel laptop: 113 ms for a no-op sync of 10,000 files on windows, 25 ms to read a lock of 10,000 files, 7 to 21 MB of peak memory. SHA-256 ran at 1.13 GB/s on one core of that laptop.

## counters

every run counts its work: requests, bytes downloaded, retries, resumes, objects written, bytes written, files placed by each method, cache hits and milliseconds per phase. every counter except the timings is deterministic, so tests assert them on every commit. a change that adds a request, or copies a file that should have been linked, fails CI.

`-v` prints the counters at the end of a run, and `--json` includes them.

## tools

| need | tool |
|---|---|
| CPU profile on any platform | samply |
| timeline of one run | `FETCHLOOM_TRACE=run.json`, opened in perfetto |
| heap | dhat-rs in a profiling build |
| whole commands | hyperfine, warmed up, ten runs or more |
| small functions | divan |
| CI regression gate | counters, plus instruction counts from gungraun on linux |

wall clock time is recorded in pull requests and tracked over time, never used to fail CI: shared runners and laptops vary too much.

## when something is slow

1. measure it with hyperfine on a release build and write the number down.
2. profile it with samply or a trace. find where the time goes before changing anything.
3. fix the largest cost, measure again, and put both numbers in the pull request.
4. a budget that proves wrong changes in this file, together with the measurement that showed it.

## build profile

release builds use `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` and `strip = true`. the earlier version measured fat LTO as 7% smaller than thin at the same speed. the allocator stays the system one until a measurement shows a gain worth a dependency; mimalloc was within noise in the earlier version.

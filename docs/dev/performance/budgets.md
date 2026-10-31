# budgets

performance budgets set whole-command targets, with observed misses kept visible.

## baseline conditions

release measurements on 2026-10-30 and 2026-10-31 used one laptop: an 11th gen intel i7-1185G7, 4 cores, 8 threads, 32 GB and nvme. windows was windows 11 with defender real-time protection on; linux was ubuntu 24.04 in WSL 2 on ext4.

baseline f9bbda5 used at least ten runs after three warm-ups without other work running. follow-ups have their own counts and load conditions in [measurements](measurements.md). these baseline timings share the [historical evidence limits](measurements.md#evidence-limits); they are observations, not reproducible current benchmark results. ranges are observed minima and maxima, not confidence intervals. a single reported value has no recorded range unless one is stated.

## targets and observations

| workload | budget | windows observation | linux observation |
|---|---|---|---|
| `--version` | under 5 ms on linux; within empty-program noise on windows | 13.9 ms; empty-program control 12.1 ms | 1.1 ms |
| first output line | under 100 ms | 29 ms for `get` while the server waits; 64 ms for `status` | 3.9 ms and 42 ms |
| settled `sync`, 10,000 files | under 200 ms | 112 ms | 50 ms |
| `status`, 10,000 files | under 150 ms | 89 ms | 42 ms |
| place 10,000 cached files in a new project | under 300 ms on linux; under 2 s on windows | about 9 s in one loaded run; earlier quiet median 2.74 s; 2.2 to 5 s in most runs at 32c6e22 under load, a miss | 126 ms |
| read and check a 10,000-file lock | under 30 ms | 26 ms of work in a 38 ms command; empty-program control 12 ms | 10.7 ms |
| SHA-256 with hardware support | at least 1 GB/s on one core | 1.08 GB/s best, 0.33 GB/s median for `cache verify` of 1 GiB | 1.33 GB/s |
| download memory | fixed buffer per running download; under 50 MB total | 17 MiB for 1 GiB | 26 MiB |
| peak memory with 100,000 project files | under 150 MB | 108 MiB adding; 75 MiB for `status` | 136 to 142 MiB adding and 94 to 102 MiB placing at 32c6e22; baseline `status` 77 MiB |
| large file on a fast link | limited by link rather than hashing or disk | local 1 GiB in 5.8 s | 2.2 s; later hash-thread result is in [measurements](measurements.md#storage-and-memory) |
| unpack large members | historical relative target: within 2 times extraction-only work; no absolute time budget | 512 MiB tar.zst in 2.2 s; zip in 2.4 s | tar.zst in 1.6 s; zip in 2.3 s |
| unpack small files | within 2 times creating, writing and flushing the same files | 10,000 random 4 KiB files in 34 to 39 s | 0.47 to 0.56 s |
| cold plain URL | one request unless redirects or split ranges are needed | counted in tests | counted in tests |
| source resolution | only requests required by its protocol | counted in tests | counted in tests |
| paged listing | one request per page | counted in tests | counted in tests |

MB and MiB are different units; the memory observations above remain in the units measured. the 150 MB budget must not silently become 150 MiB.

[placement measurements](measurements.md#placement-and-settled-sync) separate process startup from command work under load. [windows defender](windows-defender.md) explains small-file scan costs. [roadmap](../roadmap.md) tracks remaining misses; a changed budget needs new evidence.

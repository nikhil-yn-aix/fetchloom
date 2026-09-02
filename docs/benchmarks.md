# Benchmarks

Every number here was measured by `cargo xtask bench --publish` on one machine, target `x86_64-pc-windows-msvc`, with the comparison tool run in the same round against the same input.

Wall time is a property of the machine as much as of the code. It is published and never gated; this machine has measured the same binary at 773, 2364 and 4165 ms on one regime. The deterministic counters are the ones that gate, at five percent.

Many-small-files lane: measured on a volume this platform cannot enumerate, where small writes cost 47.33 times one large write, so whether a scanner is present is unknown

| Regime | Fetchloom | Alternative | Ratio | What the alternative does |
|---|---|---|---|---|
| no-op | 9 ms | none |  | no tool does this, so there is nothing to compare against |
| cold-cache | 1463 ms | powershell Copy-Item -Recurse 322 ms | 4.54x | copies the same 64 files, hashing nothing and verifying nothing |
| warm-cache | 478 ms | powershell Copy-Item -Recurse 322 ms | 1.48x | copies the same 64 files again, which is what a tool with no cache must do |
| cold-transfer | 123 ms | curl --output 23 ms | 5.44x | downloads the same object from the same server, hashing nothing, verifying nothing and publishing nothing |
| interrupted-transfer | 190 ms | curl --output 31 ms | 6.13x | downloads the same object from the same server after its interruptions are spent, so it never resumes and never pays for one |
| many-small-files | 7150 ms | powershell Copy-Item -Recurse 1049 ms | 6.81x | copies the same 1024 files, hashing nothing and verifying nothing |
| one-large-file | 759 ms | powershell Copy-Item -Recurse 347 ms | 2.19x | copies the same 256 MiB file, hashing nothing and verifying nothing |
| many-hosts | 9672 ms | curl 2114 ms | 4.58x | fetches the same sixteen objects from the same two servers one after another, hashing nothing, verifying nothing, and never backing off when a host asks it to |

A ratio above one is a regime where Fetchloom is slower than the tool beside it. Those rows are the honest ones: Fetchloom hashes every byte twice, writes an outboard tree, publishes through staging and records what it did, and none of the tools it is measured against do any of that. The comparison is published so the cost is visible, not because the tools are doing the same job.

many-hosts carries the largest ratio on this page and most of it is not transfer cost. The regime injects 100 ms of latency into every request so that a per-host ceiling has something to hide, and it issues 40 of them. Those requests are not serial: the run holds several in flight per host, so the injected latency costs a fraction of the 4000 ms it would cost if they were. Most of the 9672 ms measured is the backoff the rate limited host asks for, which this run waits out one request at a time because a host asking to be left alone drives its count back to one. The alternative pays the same injected latency and waits out none of the backoff.

## Deterministic counters

| Regime | bytes read | bytes written | requests | file operations |
|---|---|---|---|---|
| no-op | - | - | - | - |
| cold-cache | 16777216 | 33559040 | 0 | 213 |
| warm-cache | 16777216 | 16777216 | 0 | 67 |
| cold-transfer | 0 | 8388608 | 2 | 38 |
| interrupted-transfer | 4194304 | 8388608 | 6 | 58 |
| many-small-files | 1048576 | 2170880 | 0 | 3093 |
| one-large-file | 268435456 | 805322696 | 0 | 30 |
| many-hosts | 0 | 6292608 | 40 | 325 |

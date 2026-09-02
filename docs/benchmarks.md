# Benchmarks

Every number here was measured by `cargo xtask bench --publish` on one machine, target `x86_64-pc-windows-msvc`, with the comparison tool run in the same round against the same input.

Wall time is a property of the machine as much as of the code. It is published and never gated; this machine has measured the same binary at 773, 2364 and 4165 ms on one regime. The deterministic counters are the ones that gate, at five percent.

Many-small-files lane: measured on a volume this platform cannot enumerate, where small writes cost 47.10 times one large write, so whether a scanner is present is unknown

| Regime | Fetchloom | Alternative | Ratio | What the alternative does |
|---|---|---|---|---|
| no-op | 8 ms | none |  | no tool does this, so there is nothing to compare against |
| cold-cache | 1455 ms | powershell Copy-Item -Recurse 263 ms | 5.54x | copies the same 64 files, hashing nothing and verifying nothing |
| warm-cache | 220 ms | powershell Copy-Item -Recurse 263 ms | 0.84x | copies the same 64 files again, which is what a tool with no cache must do |
| cold-transfer | 108 ms | curl --output 24 ms | 4.44x | downloads the same object from the same server, hashing nothing, verifying nothing and publishing nothing |
| interrupted-transfer | 547 ms | curl --output 26 ms | 21.39x | downloads the same object from the same server after its interruptions are spent, so it never resumes and never pays for one |
| many-small-files | 5884 ms | powershell Copy-Item -Recurse 987 ms | 5.96x | copies the same 1024 files, hashing nothing and verifying nothing |
| one-large-file | 906 ms | powershell Copy-Item -Recurse 416 ms | 2.18x | copies the same 256 MiB file, hashing nothing and verifying nothing |
| many-hosts | 8581 ms | curl 379 ms | 22.66x | fetches the same sixteen objects from the same two servers one after another, hashing nothing, verifying nothing, and never backing off when a host asks it to |

A ratio above one is a regime where Fetchloom is slower than the tool beside it. Those rows are the honest ones: Fetchloom hashes every byte twice, writes an outboard tree, publishes through staging and records what it did, and none of the tools it is measured against do any of that. The comparison is published so the cost is visible, not because the tools are doing the same job.

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

# performance

fetchloom avoids repeated requests and byte copies while preserving checked identities and recoverable disk state.

## design choices

| what | why | measured effect and conditions |
|---|---|---|
| hash the contiguous download prefix on a separate thread beside the writer | overlap CPU and disk work while bounded queues limit buffering | linux local 1 GiB download: 2.06 to 2.27 s before, 1.58 to 1.65 s after, four interleaved pairs, about 25% faster; [storage measurements](measurements.md#storage-and-memory) |
| reuse response chunks and read back only joined out-of-order ranges | avoid rereading an entire completed stream | partial tests assert no readback for one stream; no isolated wall-time result for chunk sharing |
| bound downloads, with 4 per host | overlap transfers without queuing a task and buffer for every file | small-download measurements use this limit; its isolated effect was not measured |
| split only an eligible stream below 1 MB/s | add connections when a single stream cannot fill the link | lossy far runs improved 65% and the measured slow toronto runs 74%; home and lan did not split; [network measurements](measurements.md#network) |
| flush copied payloads once per sync with `syncfs` on linux 5.8 and later | make state durable without paying one flush per small copy | 10,000 copies of 4 KiB: 0.27 s without flushing, 9.19 s per-copy flushing, 0.41 s with one filesystem flush, six interleaved pairs |
| batch archive-member flushes before publication on linux 5.8 and later | retain payload durability while avoiding per-member flush overhead | ext4 unpacking fell from 8.5 to 10.2 s to about half a second, three runs each |
| skip flushes only for checkpoint, URL and MD5 hint records | losing a hint costs reuse; resumed or reused bytes are checked | 1,000 URLs of 100 KiB on local linux lan: median 4.46 s to 2.18 s, 51% faster over ten interleaved pairs |
| use windows directory-listing metadata | avoid one metadata call per file where listings already contain size and time | about 15 times faster on windows; linux listings still need a stat per file |
| spread placement over folders | links in one folder serialize on that folder | 10,000 bare links on windows: 10.3 to 12.7 s in one folder, 2.2 s across 100 folders |
| retain only size and time, and build placement paths per file | release full metadata and avoid a batch of duplicate full paths | linux warm placement peak fell from 183 MiB to 151 MiB for compact metadata; lazy paths further reduce retained allocations, with no isolated peak recorded |
| skip relocking and planning after a complete unchanged sync | do not reread member records or rebuild destinations without a reason | command-only windows medians fell from 200 ms to 131 ms and 303 ms to 158 ms under load; [settled-sync measurements](measurements.md#placement-and-settled-sync) |
| hash racy files once, then settle their state | detect edits hidden by coarse timestamps without repeated hashes | placement tests pin both edit detection and later hash avoidance; no isolated wall-time result |
| avoid a HEAD before GET and fetch listing pages once | spend requests only on required metadata and bytes | request counters in tests; range downloads have the [documented exceptions](../architecture/network.md#the-1-mbs-handover) |
| lazily initialize TLS, proxies and source setup; reuse hot-loop buffers | avoid work a local command does not need | startup and memory budgets cover whole commands; no isolated A/B |

## measurement rules

use release builds and keep cache state, filesystem, machine load, run counts and network conditions with each result. interleave old and new builds when load or a live service can vary. keep a performance change only when it gains at least 10% on one workload or network profile without a regression elsewhere.

elapsed time is not a CI assertion. work counters catch extra requests, downloads and copies on controlled workloads. the repository has no benchmark suite; reproduce the named workload and record the conditions before treating an old timing as a baseline.

release uses fat LTO, one codegen unit, abort-on-panic and stripped symbols. the earlier version measured fat LTO as 7% smaller than thin at the same speed. the system allocator remains; an earlier mimalloc experiment was within noise.

## pages

| page | use it for |
|---|---|
| [budgets](budgets.md) | whole-command targets and the recorded baseline |
| [measurements](measurements.md) | commits, conditions, counts, medians, ranges and rejected experiments |
| [windows defender](windows-defender.md) | why random small-file writes can be expensive on windows |
| [profiling](../engineering/profiling.md) | timelines, CPU samples and allocation investigations |
| [network](../architecture/network.md) | queues, hashing and adaptive splitting |
| [placing](../architecture/placing.md) | copy flushing and per-file path construction |

# performance

fetchloom avoids repeated requests and byte copies while preserving checked identities and recoverable disk state.

## implementation and evidence

| cost | implementation | evidence |
|---|---|---|
| downloading and hashing | [network pipeline](../architecture/network.md#download-pipeline) | [storage observations](measurements.md#storage-and-memory) |
| constrained transfers | [adaptive splitting](../architecture/network.md#the-1-mbs-handover) | [network observations](measurements.md#network) |
| small-file durability | [copy flushing](../architecture/placing.md#copy-durability) and [archive publication](../architecture/archives.md#validate-before-publication) | [storage observations](measurements.md#storage-and-memory) |
| repeated placement and no-op work | [placement](../architecture/placing.md) and [settled sync](../architecture/sync-loop.md#settled-sync-shortcut) | [placement observations](measurements.md#placement-and-settled-sync) |
| windows scanning | [archive staging](../architecture/archives.md) | [defender observations](windows-defender.md) |

historical timings have [evidence limits](measurements.md#evidence-limits). the [benchmark suite](benchmark-design.md) records new comparisons with their commands and checker results.

## measurement rules

use release builds and keep cache state, filesystem, machine load, run counts and network conditions with each result. interleave old and new builds when load or a live service can vary. keep a performance change only when it gains at least 10% on one workload or network profile without a regression elsewhere.

elapsed time is not a CI assertion. work counters catch extra requests, downloads and copies on controlled workloads. [benchmark design](benchmark-design.md) describes the implemented linux suite. reproduce the named workload and record the conditions before treating an old timing as a baseline.

[Cargo.toml](../../../Cargo.toml) sets fat LTO, one codegen unit, abort-on-panic and stripped symbols for release builds. [historical observations](measurements.md#observations-and-rejected-experiments) record the LTO and allocator experiments.

## pages

| page | use it for |
|---|---|
| [budgets](budgets.md) | whole-command targets and the recorded baseline |
| [measurements](measurements.md) | commits, conditions, counts, medians, ranges and rejected experiments |
| [windows defender](windows-defender.md) | why random small-file writes can be expensive on windows |
| [benchmark design](benchmark-design.md) | implemented scenarios, fairness rules and checking boundaries |
| [running benchmarks](../engineering/benchmarks.md) | setup, smoke runs, full comparisons and retained results |
| [profiling](../engineering/profiling.md) | timelines, CPU samples and allocation investigations |
| [network](../architecture/network.md) | queues, hashing and adaptive splitting |
| [placing](../architecture/placing.md) | copy flushing and per-file path construction |

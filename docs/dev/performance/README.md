# performance

fetchloom avoids repeated requests and byte copies while preserving checked identities and recoverable disk state.

## implementation and evidence

| cost | implementation | evidence |
|---|---|---|
| downloading and hashing | [network pipeline](../architecture/downloads.md#download-pipeline) | [storage observations](measurements/historical.md#storage-and-memory) |
| constrained transfers | [adaptive splitting](../architecture/downloads.md#the-1-mbs-handover) | [network observations](measurements/historical.md#network) |
| small-file durability | [copy flushing](../architecture/placing.md#copy-durability) and [archive publication](../architecture/archives.md#validate-before-publication) | [storage observations](measurements/historical.md#storage-and-memory) |
| repeated placement and no-op work | [placement](../architecture/placing.md) and [settled sync](../architecture/sync-loop.md#settled-sync-shortcut) | [placement observations](measurements/historical.md#placement-and-settled-sync) |
| windows scanning | [archive staging](../architecture/archives.md) | [defender observations](measurements/historical.md#windows-defender) |
| the cache report's look at every placed file | [cache usage](../architecture/cache-retention.md#retention-and-cleanup) | [cache report](measurements/placement.md#the-cache-report-on-100000-placed-files-2026-11-11) |
| whole commands against main and other tools | [benchmark design](benchmark-design.md) | [final benchmark](measurements/README.md#final-benchmark-on-the-linux-lab-2026-11-09) |

historical timings have [evidence limits](measurements/historical.md#evidence-limits). the [benchmark suite](benchmark-design.md) records new comparisons with their commands and checker results.

## measurement rules

use release builds and keep cache state, filesystem, machine load, run counts and network conditions with each result. interleave old and new builds when load or a live service can vary. keep a performance change only when it gains at least 10% on one workload or network profile without a regression elsewhere.

elapsed time is not a CI assertion. work counters catch extra requests, downloads and copies on controlled workloads. [benchmark design](benchmark-design.md) describes the implemented linux suite. reproduce the named workload and record the conditions before treating an old timing as a baseline.

`Cargo.toml` sets fat LTO, one codegen unit, `panic = "abort"` and stripped symbols for release builds. a probe thread started before the runtime lets the CLI report a system that refuses threads as `local.io`; [the a/b passes](measurements/network.md#ab-passes-on-the-linux-lab-2026-11) kept abort, since unwinding was no faster and made the binary 14.5% larger.

[historical observations](measurements/historical.md#observations-and-rejected-experiments) record the LTO and allocator experiments.

## running benchmarks

`bench/` compares fetchloom with native download clients in a controlled linux lab and against public services.

### prepare a dedicated machine

the lab needs linux x86_64 with root; WSL works, a native windows runner does not. setup changes network namespaces, mounts, the hosts file and the system's certificate trust, dropping page caches affects the whole machine, and teardown deletes all lab state under `FL_BENCH_ROOT` (default `/srv/fl-bench`), results included, so copy wanted records out first. use a dedicated machine with at least 30 GiB free, and follow [bench requirements and setup](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#requirements) for the package pins, the python environment and the lab's lifecycle.

[build the measured executable](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#build-the-measured-executable), then set up in the same checkout. setup defines `benchmark_binary` as its absolute linux path; keep it and the `FL_BENCH_ROOT` and `FL_BENCH_VENV` variables in the root shell the commands below run in. measured files, caches and logs stay under `FL_BENCH_ROOT`, never on a windows mount, though the repository may be on one.

### choose a run

| goal | command or route |
|---|---|
| check runner changes without downloads | `python3 -m unittest discover -s bench -p 'test_*.py'` |
| small lab smoke | `python3 bench/run.py --fetchloom "$benchmark_binary" --quick --profiles lan --fs ext4 --out "$FL_BENCH_ROOT/results/quick/lab"` |
| small service smoke | `python3 bench/live.py --fetchloom "$benchmark_binary" --quick --out "$FL_BENCH_ROOT/results/quick/live"` |
| a release candidate against the last release, without the lab | `python3 bench/recheck.py --baseline <last release> --candidate <build> --root <folder>`, as [release recheck](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#release-recheck) describes |
| full comparison | [full run commands](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#run): ext4 for all scenarios, then xfs for s5 and s6 on lan, then live services |
| publish tables and figures | [render and keep results](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#render-and-keep-results) |

use a new output directory for each invocation. `--only-tools fetchloom,curl` narrows lab clients; `--services zenodo,huggingface` narrows public services. a quick s7 run still uses 256 MiB so the file remains in flight when SIGINT arrives. the lan-only smoke above avoids that download.

### inspect the evidence

`meta.json` records versions, exact commands, machine details, seed and randomized orders. `runs.jsonl` records timing, resource use, exit codes and checker findings; logs preserve command failures. wrong bytes, a timeout or a failed command cannot count as a fast result. inaccessible live services retain skip reasons.

[benchmark design](../performance/benchmark-design.md) owns the comparison rules and timing boundaries. `bench/render.py` generates the results page, once published, and charts only after real runs. never publish smoke data as a comparison.

## pages

| page | use it for |
|---|---|
| [budgets](budgets.md) | whole-command targets and the recorded baseline |
| [measurements](measurements/README.md) | the final lab benchmark, the a/b passes, and historical observations with their commits, conditions, counts and ranges |
| [windows defender](measurements/historical.md#windows-defender) | why random small-file writes can be expensive on windows |
| [benchmark design](benchmark-design.md) | implemented scenarios, fairness rules and checking boundaries |
| [native comparison](native.md) | two builds on windows, macos or any filesystem, without the lab |
| [native checks](native-checks.md) | independent bytes, durability, credentials and resource use on windows and macos |
| [running benchmarks](#running-benchmarks) | setup, smoke runs, full comparisons and retained results |
| [profiling](../engineering/profiling.md) | timelines, CPU samples and allocation investigations |
| [network](../architecture/network.md) | queues, hashing and adaptive splitting |
| [placing](../architecture/placing.md) | copy flushing and per-file path construction |

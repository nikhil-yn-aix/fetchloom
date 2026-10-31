# profiling

profile the largest measured cost of a release workload before changing its implementation.

## establish the workload

record the commit, machine, OS, filesystem, cache state, file count, byte count and network conditions. measure without a profiler first. use `-v` or `--json` to record [work counters](../../user/reference/json-output.md); extra requests or copies can explain a regression without a CPU profile.

```sh
cargo build --locked --release
target/release/fetchloom -v sync
```

for CPU samples with symbols while keeping release optimizations on linux:

```sh
CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_STRIP=none cargo build --locked --release
```

## timeline

`FETCHLOOM_TRACE` takes a nonempty Chrome trace path, resolved before `-C` changes the working directory. it records fetchloom spans and their arguments at trace level. on linux:

```sh
FETCHLOOM_TRACE=run.json target/release/fetchloom sync
```

on windows:

```powershell
$env:FETCHLOOM_TRACE = 'run.json'
.\target\release\fetchloom.exe sync
Remove-Item Env:\FETCHLOOM_TRACE
```

create the trace file's parent directory first and choose a writable destination. if opening the file fails, fetchloom warns and continues without a timeline; JSON retains the warning. a trace path is not created recursively.

open the recording in [perfetto](https://ui.perfetto.dev). it contains local paths and source references, so review it before sharing. [cli/logs.rs](../../../src/cli/logs.rs) holds the flush guard until the command ends.

spans cover command dispatch, source resolution and phases such as reading metadata, scanning, relocking, planning and placing. downloads and unpacking have no standalone spans of their own; CPU samples can explain work inside their surrounding phases. separate command duration from start-to-exit duration when process startup is noisy.

`-v` adds details, error kinds and work counters. `-vv` enables fetchloom debug events on stderr. `FETCHLOOM_LOG` accepts tracing target filters, for example `fetchloom=debug`, overriding that default when valid; an invalid filter falls back to the `-vv` default if requested. stderr logs suppress progress bars, including in quiet or JSON mode. a trace alone does not suppress bars.

## CPU and allocation profiles

with samply installed on linux:

```sh
samply record target/release/fetchloom sync
```

samply on windows needs the windows performance toolkit. use the same release workload before and after a change, and do not compare a profiled run with an unprofiled one.

on linux, first record peak RSS without allocation instrumentation:

```sh
/usr/bin/time -v target/release/fetchloom sync
heaptrack -o heaptrack-sync target/release/fetchloom sync
```

pass the recording path printed by heaptrack to `heaptrack_print`. inspect retained allocations, then repeat the workload without heaptrack. its wall time includes instrumentation overhead. [memory measurements](../performance/measurements.md#storage-and-memory) distinguish heap size from RSS and allocator retention.

## compare builds

interleave configurations in both orders, a then b and b then a. start each cold-download run with an empty cache; for warm placement, populate the cache once and use a fresh project per run. keep build profile, bytes, filesystem and worker settings constant.

report run count, median and observed range for each side, including regressions and failed attempts. use the [benchmark workflow](benchmarks.md) for checked comparisons. do not infer missing statistics from old ranges. [measurements](../performance/measurements.md) records rejected experiments so you can avoid repeating them without new evidence.

related: [performance](../performance/README.md), [windows defender](../performance/windows-defender.md), [output](../architecture/output.md), [building and testing](building-and-testing.md).

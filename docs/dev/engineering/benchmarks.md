# running benchmarks

`bench/` compares native download workflows in a controlled linux lab and against public services.

## prepare a dedicated machine

follow [bench requirements and setup](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#requirements) for the package pins, python environment and lab lifecycle. the runner needs linux x86_64, python 3.12, root, network namespaces, loop devices, ext4 and xfs. it is not a native windows runner; WSL measures the linux filesystem.

use a dedicated machine with at least 30 GiB free. setup changes namespaces, mounts, the hosts file and system certificate trust. dropping page caches affects the whole machine. teardown deletes all lab state, including results, under `FL_BENCH_ROOT` (default `/srv/fl-bench`); copy wanted records out first.

[build the measured executable](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#build-the-measured-executable), then follow setup in the same checkout. setup defines `benchmark_binary` as its absolute linux path; keep it and the `FL_BENCH_ROOT` and `FL_BENCH_VENV` variables in the root shell used below. keep measured files, caches and logs under `FL_BENCH_ROOT`, not a windows mount. the repository itself may be on a windows mount.

## choose a run

| goal | command or route |
|---|---|
| check runner changes without downloads | `python3 -m unittest discover -s bench -p 'test_*.py'` |
| small lab smoke | `python3 bench/run.py --fetchloom "$benchmark_binary" --quick --profiles lan --fs ext4 --out "$FL_BENCH_ROOT/results/quick/lab"` |
| small service smoke | `python3 bench/live.py --fetchloom "$benchmark_binary" --quick --out "$FL_BENCH_ROOT/results/quick/live"` |
| full comparison | [full run commands](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#run): ext4 for all scenarios, then xfs for s5 and s6 on lan, then live services |
| publish tables and figures | [render and keep results](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#render-and-keep-results) |

use a new output directory for each invocation. `--only-tools fetchloom,curl` narrows lab clients; `--services zenodo,huggingface` narrows public services. a quick s7 run still uses 256 MiB so the file remains in flight when SIGINT arrives. the lan-only smoke above avoids that download.

## inspect the evidence

`meta.json` records versions, exact commands, machine details, seed and randomized orders. `runs.jsonl` records timing, resource use, exit codes and checker findings; logs preserve command failures. wrong bytes, a timeout or a failed command cannot count as a fast result. inaccessible live services retain skip reasons.

[benchmark design](../performance/benchmark-design.md) owns the comparison rules and timing boundaries. `bench/render.py` generates the results page, once published, and charts only after real runs. never publish smoke data as a comparison.

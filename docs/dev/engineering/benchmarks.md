# running benchmarks

`bench/` compares native download workflows in a controlled linux lab and against public services.

## prepare a dedicated machine

follow [bench requirements and setup](../../../bench/README.md#requirements) for the package pins, python environment and lab lifecycle. the runner needs linux x86_64, python 3.12, root, network namespaces, loop devices, ext4 and xfs. it is not a native windows runner; WSL measures the linux filesystem.

use a dedicated machine with at least 30 GiB free. setup changes namespaces, mounts, the hosts file and system certificate trust. dropping page caches affects the whole machine. teardown deletes all lab state, including results, under `/srv/fl-bench`; copy wanted records out first.

build fetchloom in release mode and pass its absolute linux path to `--fetchloom`. keep measured files, caches and logs under `/srv/fl-bench`, not a windows mount. the repository itself may be on a windows mount.

## choose a run

| goal | command or route |
|---|---|
| check runner changes without downloads | `/opt/fl-bench/venv/bin/python -m unittest discover -s bench -p 'test_*.py'` |
| small lab smoke | `python3 bench/run.py --fetchloom /home/fl/bin/fetchloom --quick --profiles lan --fs ext4 --out /srv/fl-bench/results/quick/lab` |
| small service smoke | `python3 bench/live.py --fetchloom /home/fl/bin/fetchloom --quick --out /srv/fl-bench/results/quick/live` |
| full comparison | [full run commands](../../../bench/README.md#run): ext4 for all scenarios, then xfs for s5 and s6 on lan, then live services |
| publish tables and figures | [render and keep results](../../../bench/README.md#render-and-keep-results) |

use a new output directory for each invocation. `--only-tools fetchloom,curl` narrows lab clients; `--services zenodo,huggingface` narrows public services. a quick s7 run still uses 256 MiB so the file remains in flight when SIGINT arrives. the lan-only smoke above avoids that download.

## inspect the evidence

`meta.json` records versions, exact commands, machine details, seed and randomized orders. `runs.jsonl` records timing, resource use, exit codes and checker findings; logs preserve command failures. wrong bytes, a timeout or a failed command cannot count as a fast result. inaccessible live services retain skip reasons.

[benchmark design](../performance/benchmark-design.md) owns the comparison rules and timing boundaries. `bench/render.py` generates the [results, once published](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/dev/performance/benchmarks.md) and charts only after real runs. never publish smoke data as a comparison.

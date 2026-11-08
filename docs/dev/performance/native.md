# native comparison

`bench/native.py` compares two fetchloom builds on a native filesystem without the linux lab's namespaces, mounts or traffic shaping.

the runner needs python 3.9 or later and its standard library. use it on ntfs, ReFS/dev drive, apfs, exfat or a linux filesystem. [native checks](native-checks.md) adds independent byte verification, resource measurements and platform-specific checks.

## workloads

fixtures are generated under `ROOT/www` and served from `127.0.0.1` over HTTP with keep-alive.

| scenario | files | work |
|---|---|---|
| `small` | 10,000 files of 4 KiB, one dataset per file | requests, object publication and placement across dataset folders |
| `zip` | one stored zip of 10,000 members of 4 KiB | extraction and placement of many files in one dataset |
| `large` | one file, 1 GiB by default | hashing and writes; `--large-mib` changes its size |

preparation locks each scenario once per build, outside timing, then removes that preparation cache. each round runs both builds in a seeded random order, with an empty cache per build:

| phase | command and state |
|---|---|
| cold | `fetchloom -q --yes sync` in a fresh project |
| warm | the same command in a second fresh project, with the cache retained |
| settled | the same command again in that second project |
| path | `fetchloom -q path f00000` in the settled project |

there is no warmup round or page-cache drop. cold means an empty fetchloom cache, not a cold OS or server cache. preparation, cleanup and fixture generation are outside timing.

## run

```sh
python3 bench/native.py --baseline /path/to/baseline --candidate /path/to/candidate --root /path/to/new-run --scenarios small,zip --rounds 10 --seed 1729 --out /path/to/new-results
```

use `py -3` and native executable paths on windows. run from the repository root. every invocation needs a fresh, owned `--root`; do not reuse a root whose templates still name an earlier loopback server. do not point it at a shared cache or existing project: the runner deletes and recreates its workload folders, clearing read-only flags during cleanup.

| option | default |
|---|---|
| `--baseline`, `--candidate`, `--root` | required paths |
| `--scenarios` | `small,zip,large` |
| `--rounds` | `5`, a positive integer |
| `--large-mib` | `1024`, a positive integer |
| `--seed` | `1729` |
| `--out` | `ROOT/results` |

the child environment is inherited, apart from `FETCHLOOM_CACHE` and removal of `NO_COLOR`. clear unrelated fetchloom settings, proxies and trace output before a default comparison, or record the identical settings applied to both builds. close other heavy work and repeat with another fresh root before trusting a small difference.

## results and limits

`native.json` retains phase wall times, settings, machine information and executable SHA-256s. `native.md` contains the summary table. filesystem and Defender probes are best effort; unavailable or localized system output can leave fields empty, so record the actual volume and protection settings separately.

the ratio is the exponential of the median paired log ratio, candidate over baseline. its 95% percentile bootstrap interval uses 20,000 resamples through [ab.py](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/ab.py). with at least three positive pairs, an interval wholly below 1 says `candidate faster`, wholly above 1 says `candidate slower`, and otherwise says `no clear difference`. this is not the linux a/b runner's minimum-pair or effect-size acceptance gate.

the runner checks process exit codes, not output hashes independently. it captures but does not retain successful stdout and stderr, records no CPU time, memory, work counters or server request counts, and has no per-command timeout. a failed command aborts the run before the final report. keep failures and use the [native verification steps](native-checks.md#verify-the-fixture-bytes) before interpreting timings.

it measures loopback work, not a real network link. use the [lab](benchmark-design.md) for controlled latency, loss and bandwidth. WSL measures linux behavior, not native windows behavior.

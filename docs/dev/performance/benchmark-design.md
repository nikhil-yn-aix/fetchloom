# benchmark design

how fetchloom is compared with the tools people use today, and the rules that keep the comparison fair. [bench/](../../../bench/README.md) implements it.

## what it answers

1. how long does a first download take, from the command to checked files on disk?
2. how long does a second project or a fresh clone take when the bytes are already on the machine?
3. what does running it again cost when nothing changed?
4. how many bytes does an interrupted download cost?
5. how much CPU, memory and disk does each tool use for the same result?

## fairness rules

1. same job. every tool starts from the same inputs (the URLs or the project, and the expected SHA-256 of each file) and must end with the same files on disk. a run counts only when a checker finds every expected file with the right SHA-256; a run with a missing or wrong file is reported as failed, never as fast.
2. same checking. fetchloom always verifies hashes. a tool that can verify does so with its own option (aria2c `--checksum`, pooch `known_hash`). a tool that cannot, such as curl or wget, runs `sha256sum -c` in the same timed command. the clock includes that check.
3. each tool's own way. a tool runs the workflow its documentation recommends for the job, not an imitation of fetchloom: DVC pulls a tracked folder from an HTTP remote, datalad clones a dataset and gets its files, pooch retrieves files by known hash.
4. each tool's own speed settings. a tool runs with the parallelism its documentation offers (curl `--parallel --parallel-max 4`, aria2c `-j 4 -x 4`, DVC and datalad `-J 4`) and defaults for everything else. wget and pooch have no parallel mode and run as they are. every flag is in the results.
5. same machine, same network. all tools run one at a time on the same idle machine, against the same local server, through the same shaped link.
6. no ordering luck. each round runs the tools in a new random order (seed recorded), so a slow minute on the machine hits every tool alike. one untimed warmup round per case loads binaries and python bytecode first.
7. whole command. the clock covers process start to exit, including interpreter startup, resolution and verification, because that is what a person waits for.
8. pinned and recorded. each tool is installed on its own at a pinned version; versions, commands, kernel, filesystem and CPU are written into the results.

## scenarios

| id | scenario | state before | what it shows |
|---|---|---|---|
| s1 | one large file | empty caches | transfer and hashing speed |
| s2 | many 100 KiB files | empty caches | per-request overhead and parallelism |
| s3 | 10,000 files of 4 KiB | empty caches | per-file cost on disk |
| s4 | one zip of 10,000 members | empty caches | download plus unpacking |
| s5 | s2 again in a second project | the tool's cache filled by s2 | reuse on one machine |
| s6 | the same command again | everything in place | the cost of a no-op, paid by a notebook on every run |
| s7 | s1 interrupted halfway, then run again | a partial download | bytes downloaded twice |

cold runs empty every tool's cache and drop the client's page cache; the server's files are read once after the drop, so the server is warm like a real one. warm runs keep caches and page cache.

in s7 every tool gets ctrl-c (SIGINT) at the same moment, 10 seconds into a download on the home profile; tools still running after 5 more seconds get SIGKILL. the metric is the bytes the server sent for that file across both runs, divided by its size.

## tools

the lab runs on linux against nginx serving HTTPS with HTTP/2 from a separate network namespace. its certificate authority is installed in the system trust store, so every tool trusts it without special flags.

| tool | how it runs | scenarios |
|---|---|---|
| fetchloom | `fetchloom sync` in a project with `data.toml` and `data.lock` | all |
| curl | `curl --parallel --parallel-max 4`, then `sha256sum -c` | s1 to s4, s6, s7 |
| aria2c | `aria2c -j 4 -x 4` with `--checksum` per file | s1 to s4, s6, s7 |
| wget | `wget -i list`, then `sha256sum -c` | s1 to s4, s6, s7 |
| pooch | `pooch.retrieve` with `known_hash`, `Unzip` for s4 | all |
| DVC | `dvc pull -j 4` from an HTTP remote with `verify = true`, reflink, hardlink or copy as the cache type | s1 to s3, s5 to s7 |
| datalad | `datalad clone`, then `datalad get -J 4` from URLs registered with git-annex | s1 to s3, s5 to s7 |

for s4 the downloaders run `unzip -q` after the check, in the same timed command. DVC and datalad track files rather than archives, so they sit s4 out. s6 uses each tool's own way to skip work: curl `-z` asks the server whether each file changed, wget `-nc` keeps existing files, aria2c rechecks with `--check-integrity`, pooch rehashes, DVC and datalad report nothing to do.

live runs fetch real public datasets, 5 interleaved rounds each with empty caches, against each service's own tool:

| service | dataset | other tool |
|---|---|---|
| zenodo | a dataset of about 100 MB | zenodo_get |
| hugging face | a parquet dataset of about 100 MB | `hf download` with hf_xet |
| kaggle | a dataset of about 50 MB | kagglehub |
| figshare | 10.6084/m9.figshare.5616403 | datahugger |
| dataverse | doi:10.7910/DVN/TJCLKP, 175 files | datahugger |

live numbers vary with the service, so they show ranges and no ratios.

## network profiles

`tc netem` shapes the link between the client and the server namespace. sizes shrink on slower profiles so every case stays under about a minute per run.

| profile | delay | loss | bandwidth | s1 size | s2 files |
|---|---|---|---|---|---|
| lan | 0 ms | 0% | unlimited | 1 GiB | 1,000 |
| home | 20 ms | 0% | 100 Mbit/s | 256 MiB | 1,000 |
| far | 150 ms | 1% | 50 Mbit/s | 128 MiB | 300 |
| poor | 300 ms | 3% | 10 Mbit/s | 32 MiB | 100 |

s1 and s2 run on every profile. s3 to s6 run on lan, where local costs dominate. s7 runs on home.

## platforms

| machine | filesystems |
|---|---|
| linux x86_64 | ext4 and xfs, each a fresh loop-mounted image |
| windows x86_64 | NTFS with defender on |

windows runs s1, s2, s3, s5 and s6 on lan over plain HTTP, because a test certificate cannot be trusted there without a manual prompt; every tool uses the same plain link. its tools are fetchloom, curl with `certutil -hashfile`, aria2c and pooch.

## metrics

| metric | how |
|---|---|
| wall time | monotonic clock around the whole command |
| CPU time, user and system | `getrusage` of the finished command and its children |
| peak memory | the largest resident set among the command's processes |
| bytes and requests | the server's access log for that run |
| disk used | free-space change of the dedicated filesystem, so links and reflinks count once |
| correctness | SHA-256 of every expected output file |

## runs and statistics

lab cases run 10 rounds on lan and 5 on shaped profiles, after one warmup round. results report the median with the minimum and maximum, and each tool's median divided by fetchloom's. a difference under 5% is reported as even.

## results and presentation

each run of the suite writes `meta.json` and `runs.jsonl` under `bench/results/`, which git ignores. `bench/render.py` turns them into the charts under `docs/assets/bench/` and the tables in [benchmark results](benchmarks.md), so every published number traces back to a results file.

## not measured

- macos, which fetchloom does not support.
- services that need an account the lab cannot reproduce, beyond the live runs above.
- private remotes such as S3 buckets behind credentials.

## prior art

| project | what this design takes from it |
|---|---|
| [uv benchmarks](https://github.com/astral-sh/uv/blob/main/BENCHMARKS.md) | cold and warm scenarios, stated OS and filesystem caveats |
| [pnpm benchmarks](https://github.com/pnpm/benchmarks-of-javascript-package-managers) | each tool provisioned on its own, a local registry, a warm server, measuring kept apart from rendering |
| [hyperfine](https://github.com/sharkdp/hyperfine) | warmup rounds, prepare steps before each run, recorded commands |
| [dvc-bench](https://github.com/iterative/dvc-bench) | dataset sizes as a parameter |

related: [measurements](measurements.md), [budgets](budgets.md), [windows defender](windows-defender.md).

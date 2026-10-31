# benchmark design

how fetchloom will be compared with the tools people use today. the suite is designed and not built yet, so no comparison numbers exist.

## what it answers

1. how long does a first download take, start to checked files on disk?
2. how long does a second project or a fresh clone take when the cache already has the bytes?
3. what does a sync cost when nothing changed?
4. how much does an interrupted download lose?
5. how much disk, memory and CPU does each tool use for the same result?

## how it stays fair

every tool gets the same files, the same machine and the same network, and must end with the same bytes. after each run a checker hashes every output file against the expected SHA-256; a run with a wrong or missing file counts as failed, not as fast.

each tool is installed on its own at a pinned version, in its own virtual environment where it is a python package. the versions, flags and commands are written into the results, so anyone can rerun them.

a tool that does not check hashes is still timed, and the table says so beside its number. fetchloom always hashes, so the comparison shows what that costs.

the clock covers the whole command, including startup, resolving the reference and verification. it stops when the command exits.

## scenarios

| id | scenario | cache before | what it shows |
|---|---|---|---|
| s1 | one 1 GiB file | empty | raw transfer and hashing speed |
| s2 | 1,000 files of 100 KiB | empty | request overhead and parallelism |
| s3 | 10,000 files of 4 KiB | empty | per-file cost, defender cost on windows |
| s4 | one 1 GiB zip of 10,000 members | empty | download plus unpacking |
| s5 | s2 and s4 again in a second project | warm | reuse through the cache, links or copies |
| s6 | sync with nothing changed | warm, placed | the no-op cost a notebook pays on every run |
| s7 | s1 killed at 50%, then run again | partial | bytes downloaded twice after an interruption |

cold runs clear every tool's cache and drop the OS page cache before each run. warm runs fill the tool's cache once and are timed after that.

## tools

the lab runs every tool against a local HTTPS server, so the network is the only variable.

| tool | why it is here |
|---|---|
| curl and sha256sum | the baseline: one request per file, then a hash check |
| aria2c with 16 connections | the fastest common downloader for large files |
| wget | the other common downloader |
| pooch | the python data fetcher many science libraries use |
| dvc `import-url` | the data versioning tool with a cache and a lock |
| datalad | git-annex based data management with checksums |

live runs fetch real public datasets once a day for a week, to check that lab results hold on the real services. live numbers vary with the service, so they show ranges and are not ratios.

| service | other tool |
|---|---|
| zenodo | zenodo_get |
| hugging face | `hf download` with hf_xet |
| kaggle | kagglehub |
| google drive | gdown |
| figshare and dataverse | datahugger |

## network profiles

linux `tc netem` shapes the lab link. windows runs use the lan profile only, because netem is linux only.

| profile | delay | loss | bandwidth |
|---|---|---|---|
| lan | 0 ms | 0% | unlimited |
| home | 20 ms | 0% | 100 Mbit/s |
| far | 150 ms | 1% | 50 Mbit/s |
| poor | 300 ms | 3% | 10 Mbit/s |

## platforms

| machine | filesystems |
|---|---|
| linux x86_64 | ext4 (hardlinks), xfs (reflinks) |
| windows x86_64 | NTFS with defender on, dev drive (ReFS) |
| linux arm64 | ext4, lan profile only |

defender stays on for windows runs, since that is what users have.

## metrics

| metric | how |
|---|---|
| wall time | hyperfine |
| CPU time, user and system | hyperfine |
| peak memory | `/usr/bin/time -v` on linux, the process's peak working set on windows |
| bytes and requests | the lab server's access log |
| disk used | bytes allocated after the run, counting a linked file once |
| correctness | the SHA-256 checker described above |

## runs and statistics

hyperfine drives each case, with `--prepare` for cold runs and `--warmup 1` for warm ones. lab cases run 10 times and live cases 5 times. tools run interleaved in a random order each round, so a slow minute on the machine hits every tool instead of one.

results report the median with the minimum and maximum, and fetchloom's time as a ratio of each tool's median. a difference under 5% is reported as even.

## results and presentation

raw results are JSON, one file per run of the suite, with the machine, kernel, filesystem, tool versions and commands. they are attached to a github release and kept out of git. a script renders the tables and charts in the docs from that JSON, so the numbers in the docs always trace back to a file.

## not measured

- macos, which fetchloom does not support.
- sources that need an account the lab cannot reproduce, beyond the live runs above.
- tools that need a server of their own, such as a private DVC remote.

## prior art

| project | what this design takes from it |
|---|---|
| [uv benchmarks](https://github.com/astral-sh/uv/blob/main/BENCHMARKS.md) | cold and warm scenarios, hyperfine, stated OS and filesystem caveats |
| [pnpm benchmarks](https://github.com/pnpm/benchmarks-of-javascript-package-managers) | each tool provisioned on its own, a local registry, measuring kept apart from rendering |
| [hyperfine](https://github.com/sharkdp/hyperfine) | warmup, prepare commands, outlier warnings, JSON export |
| [dvc-bench](https://github.com/iterative/dvc-bench) | dataset sizes as a parameter |

related: [measurements](measurements.md), [budgets](budgets.md), [windows defender](windows-defender.md).

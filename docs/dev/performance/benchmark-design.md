# Benchmark design

The benchmark measures how long fetchloom and the tools researchers use now take to put checked research files on disk, and what that costs.

[bench/](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md) implements it; [running benchmarks](README.md#running-benchmarks) gives the contributor route.

## What it answers

Each part answers one question, and a case that answers none of them is not measured.

| Part | Question | How |
|---|---|---|
| A, before and after | How much faster and lighter is this release than the build it started from? | `bench/recheck.py`: fetchloom at the starting build against the candidate, on the same machine and files |
| B, against other tools | For the jobs researchers do, how does fetchloom compare with the tools they use now, at the same politeness and with the same checked result? | `bench/run.py` in the lab: one shaped link, a local server, every tool in turn |
| C, real services | Does fetchloom keep up with each repository's own tool against the real service? | `bench/live.py` against zenodo, hugging face, kaggle, figshare and dataverse |

Claims come only from six headline cases of part B, listed in the [analysis plan](#analysis-plan). Every other result describes what was measured and claims nothing.

## Fairness rules

1. Same result. Every tool starts from the same bytes and must leave every expected file with the right SHA-256, which a checker outside the clock reads. A wrong or missing file is a failed run, never a fast one.
2. Same checking of downloads. Every tool checks the bytes it downloads: aria2c with `--checksum`, pooch with `known_hash`, DVC with `verify = true` and git-annex with SHA-256 keys. curl runs `sha256sum -c` in its timed command, and fetchloom checks every byte.
3. Each tool's own workflow. A tool runs the workflow its documentation recommends for the job, not an imitation of fetchloom: DVC pulls a tracked folder from an HTTP remote, datalad clones a dataset and gets its files, and pooch retrieves files by their known hash.
4. Same politeness. No tool has more than 4 requests in flight to one host, fetchloom's own default. Each tool is held to it by its own flags, and the server's log shows whether it held, run by run. A faster mode that is less polite, such as aria2c's `-x 16`, is out of scope.
5. Same clock boundary, by how the tool is used. A command-line tool is timed from its start to its exit, interpreter startup included. pooch, a library, is timed around its calls inside an interpreter that has already imported it, as a notebook uses it, with the import time reported beside the calls.
6. Durability shown, not hidden. fetchloom flushes what it places before it names it, and most tools do not. Each tool runs its default, and curl+sync, curl followed by a flush of the filesystem it wrote, runs beside it in the cases that write data. The gap between them is this workflow's flush cost, not a general price of surviving a power cut.
7. Each no-op states what it checks. fetchloom and the git-based tools trust a file's size and modification time, fetchloom hashing a file whose time is too recent to trust, pooch and aria2c hash every file again, and curl `-z` asks the server. The results give each tool's guarantee beside its time.
8. The shape of real downloads. Most repositories answer a download with a redirect to a storage host, as hugging face, figshare, dataverse and github releases do. In s2r every link answers with a 302 to a second host name, and every tool follows it.
9. Same machine and link, one tool at a time, in a new seeded order every round. A cold run drops the page cache first, and every run starts without the TCP state an earlier connection left.
10. Paths of one length. A longer path costs every lookup more kernel time, enough to make an arm with longer folder names look slower, so every arm and tool runs in the same work folder with its cache at the same path, and logs go by a run id of one length. Tool and arm names live in the results, never in a path.
11. Pinned and recorded. Versions, flags, commands, the kernel, the filesystems, the CPU and the network shaping are written into the results.

## Scenarios

Sizes are chosen so a transfer takes about 20 to 60 seconds on its link. A timing pilot at full size sets the final sizes, shrinking a case for every tool alike where it cannot fit the budget.

| ID | Job | Links | Why it is here |
|---|---|---|---|
| s1 | One large file: 256 MiB at home, 64 MiB on the poor link | home, poor | Transfer, hashing and loss recovery |
| s2r | 100 KiB files, each link a 302 to the storage host: 1,000 at home, 200 on the poor link | home, poor | Per-request cost in the commonest real shape |
| s3 | 10,000 files of 4 KiB | home, lan | Per-file cost on the wire and on disk |
| s4 | A zip of 10,000 members of 4 KiB | home | Downloading plus unpacking many members |
| s5 | s2r's files in a second project | lan | Reuse on one machine: time and extra disk |
| s6 | s2r's command again, nothing changed | lan | What a notebook pays on every run |
| s7 | ctrl-c 10 seconds into s1, then the same command | home, poor | Resuming: what is asked for twice, and the time to finish |
| s8 | A tar.gz of 100 csv tables of about 1 MiB | home | Downloading plus decompressing |
| s9 | s3's command again, nothing changed | lan | The no-op where per-file checks add up |
| s10 | The link drops 10 seconds into s1, for 10 seconds, with nothing stopped | home | A dropped connection: the time lost and what is asked for twice |

A cold run empties every tool's cache, writes out the lab's filesystems, drops the machine's page cache and reads the server's files back, so the server is warm like a real one. Every run, cold or warm, also starts without what Linux kept from earlier connections between client and server, such as its estimate of packet reordering: the lab clears the server namespace's TCP metrics and the host's entry for the server. A warm run keeps its caches. s5 and s6 first run s2r's workflow outside the clock, and s9 s3's. In s5 fetchloom and DVC get a new project and datalad a clone of the first, all made before the clock starts, and pooch returns paths in its shared cache.

In s7 every tool gets ctrl-c (SIGINT) 10 seconds after its command starts, and a tool still running 5 seconds later gets SIGKILL. The same command then runs again, and its time is the one reported; curl continues with `-C -`.

In s10 the link drops at both ends 10 seconds after the command starts and comes back 10 seconds later: netem's loss goes to 100% for new packets, while the packets already queued still arrive, and nothing is stopped or signalled. The report sets each tool's total time against its own s1 on the same link and file. A run that ends before the link comes back failed, since it measured no drop.

s7 and s10 also report server body bytes / dataset size: the body bytes of the server's whole and partial answers for the file, over its size. It says what a tool asked for again. The server cannot see packets the network sent twice, so it says nothing about the wire.

## Tools

The lab runs on Linux against nginx serving HTTPS with HTTP/2 from a separate network namespace, at `lab.flb` and at the storage host `store.flb`. Its certificate authority is installed in the system trust store, so every tool trusts it without special flags.

| Tool | How it runs | Scenarios |
|---|---|---|
| fetchloom | `fetchloom -q --yes sync` in a project with `data.toml` and `data.lock`, with `FETCHLOOM_HOST_JOBS=4` | All |
| fetchloom, A/A arm | The same build again with a setting it never reads, `--variant aa:FETCHLOOM_EXP_NONE=1`, to measure the machine's own noise; never a rival | All |
| curl | `curl -q --parallel --parallel-max 4`, with `-L` for each transfer, then `sha256sum -c`; `-C -` in s7 and `-z` in s6 and s9 | All but s5 |
| curl+sync | curl as above, then `sync -f data` | s1 to s4, s7, s8 and s10 |
| aria2c | `aria2c -j 4 -x 4` with `--checksum` for each file; `-x 4` splits only the large file, into 4. `--check-integrity=true` in s6 and s9 | All but s5 |
| pooch | `pooch.retrieve` with `known_hash` in an interpreter that imported pooch first, `Unzip` in s4 and `Untar` in s8; one file at a time | All |
| DVC | `dvc pull -j 4` from an HTTP remote with `verify = true`, and reflink, hardlink or copy as the cache type | All but s4 and s8 |
| datalad | `datalad get -J 4` from links registered with git-annex, in a `datalad clone` made before the clock starts | All but s4 and s8 |

In s2r every tool's links start at `https://lab.flb/r/`, DVC's remote and the links registered with git-annex included, and curl follows them with `-L`. A run whose server log does not show both host names failed.

pooch's script times its own calls: the wall time and the process's CPU time around them, written with the import time when it exits, even when stopped. The results label that boundary "call inside a warm interpreter" and keep the whole process's time beside it. Its peak memory is the whole process's.

For s4 the downloaders run `unzip -q` after the check, in the same timed command, and for s8 `tar -xzf`. DVC and datalad track files rather than archives, so they sit s4 and s8 out. s8's members are csv tables from a seeded generator, rows a field survey might log, which gzip shrinks to about a fifth, as it does real tables; every other fixture is random bytes that no compressor shrinks, so s4's zip stores its members and only s8 times decompression. In s6 and s9 nginx answers a later `If-Modified-Since` with 304, since curl `-z` sends the time it downloaded the file.

Every command runs with an empty home and cache of its own and inherits no credentials, client configuration overrides or Git settings. fetchloom refuses a private address that a project names unless `allowed_origins` lists its origin, so it gets `FETCHLOOM_ALLOWED_ORIGINS` naming the lab's two origins; git-annex is allowed to reach `10.77.0.2` for the same reason. DVC analytics are off. These settings are recorded with every run.

## Network profiles

`tc netem` shapes the link between the client and the server namespace: half the round trip and the loss at each end, and the rate at the server's end.

| Profile | Stands for | Round trip | Loss each way | Server rate | Queue at the server | Large file | Many files |
|---|---|---:|---:|---:|---:|---:|---:|
| lan | The local network | 0 ms | 0% | Unlimited | None | Not run | 1,000 |
| home | Home broadband | 20 ms | 0% | 100 Mbit/s | 0.5 s, 4,250 packets | 256 MiB | 1,000 |
| poor | A poor connection | 300 ms | 3% | 10 Mbit/s | 1 s, 959 packets | 64 MiB | 200 |

A queue is set in time, not packets: the netem limit holds the profile's queue time at its rate, counting the packets its half of the delay holds, in packets of 1,500 bytes. The client's end carries requests and acknowledgements, which never fill netem's default of 1,000 packets. `meta.json` records each profile with its limit.

ext4 runs every scenario on every link it applies to. xfs adds s5 and s6 on lan, where reflinks change reuse and no-op work.

## Before and after

Part A's `bench/recheck.py` times what a release must not slow down: a status, a settled sync and a cache check of 100,000 files, `path` beside them, `status --verify` of 512 MiB, placing 40,000 one-file datasets, an `add` of 1,000 links, a zip of 10,000 files, a 1 GiB `get`, a merge and an exclude across 1,000 datasets, and `--version`, `help` and a settled sync of 3 files. With `--links` it adds s1, s2r and s7 at home and on the poor link, through the lab, 10 rounds each.

The arms are the starting build (fetchloom at d168842, main when this release's work began), the candidate, and the starting build's bytes again as the A/A arm; either build would be a valid noise control. Each arm runs a fresh copy of its binary and makes its own projects and caches from the same generated files, so no arm reads another's formats. The copies and each arm's folders go by the ids `arm0`, `arm1` and `arm2`. Every file a run downloads or places is checked after the clock stops and before its folder is cleared, at the path the arm's own `data.toml` and fetchloom's layout give it, against the SHA-256 its source serves, so exit 0 alone is never a pass and a file in the wrong place fails too.

The summary gives each case's failed runs and its time, CPU and peak memory ratio, arm over the starting build, with its 95% interval, read beside the A/A arm's: a shift both show is the machine. A comparison with a failed run shows no performance at all, and any failed run, a wrong file among them, makes the recheck exit nonzero.

## Real services

`bench/live.py` fetches public datasets with fetchloom and with each service's own tool, interleaved: 5 rounds, the two tools in a new random order each round, every run with empty caches.

| Service | Dataset | Other tool |
|---|---|---|
| zenodo | [8164711](https://zenodo.org/records/8164711), `pbmc.RData` | zenodo_get |
| hugging face | [stanfordnlp/imdb](https://huggingface.co/datasets/stanfordnlp/imdb), files matching `plain_text/*.parquet`, revision `e6281661ce1c48d982bc483cf8a173c1bbeb5d31` | `hf download` with hf_xet, then `sha256sum -c` |
| kaggle | [zynicide/wine-reviews](https://www.kaggle.com/datasets/zynicide/wine-reviews), version 4, `winemag-data-130k-v2.csv` | kagglehub, then a SHA-256 check |
| figshare | [10.6084/m9.figshare.5616403](https://api.figshare.com/v2/articles/5616403), files in the current article listing | datahugger |
| dataverse | [doi:10.7910/DVN/TJCLKP](https://dataverse.harvard.edu/dataset.xhtml?persistentId=doi:10.7910/DVN/TJCLKP), current published version and public raw files, recorded at preparation | datahugger |

`live.py` reads metadata, sizes and exact selections during preparation and downloads an independent reference before timing either client. Both clients keep archives packed; their file hashes are compared with the reference, and layout differences are reported apart from content differences. The hugging face and kaggle clients take no expected SHA-256, so their timed commands check it after the download, as curl's do. datahugger gets the explicit dataverse landing URL and an identified `fetchloom-benchmark/1 datahugger/0.13` User-Agent, since the [bench access notes](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#run) record its DOI parser and default User-Agent being refused; its timed call uses `checksum=True` and `unzip=False`. In the full dataverse dataset fetchloom requests the original TSV and datahugger the ingested TAB, so the bytes differ and the run fails the shared check rather than counting as a speed result.

A live service's speed changes from minute to minute, so live results give each tool's median and range, not ratios. Quick runs use the zenodo 3608990 iris record, hugging face `scikit-learn/iris` (`Iris.csv`), kaggle `uciml/iris` version 2 (`Iris.csv`), and files no larger than 1 KiB in the figshare and dataverse datasets.

## Metrics

| Metric | How |
|---|---|
| Wall time, the primary measure | The monotonic clock from a command's start to its exit; for pooch, around its calls |
| CPU time, user and system | From `os.wait4` of GNU time, which starts the command, so its children count; for pooch, the process's own time around its calls |
| Peak memory | The largest resident set among the command's processes, as GNU time reports it |
| Disk used | The free-space change of the dedicated filesystem, so links and reflinks count once |
| Requests, bytes, connections and protocols | The server's log for that run |
| Requests in flight | Per host, the most at once, HTTP/2 streams included: each request counts from its arrival until nginx logged it, and for at least the link's round trip, since a client sends its next request on a slot only after the last answer reaches it, less 2 ms for the log's millisecond clock. On lan a request answered within 2 ms does not count. A run over 4 is flagged, not failed |
| Server body bytes / dataset size | In s7 and s10, the body bytes of the server's whole and partial answers for the file, over its size |
| Time lost to a dropped link | In s10, each tool's total time against its own s1 on the same link |
| Correctness | A gate, not a measure: every expected file's SHA-256, outside the clock |

## Analysis plan

This plan is fixed before any data is seen.

- The unit is one round of one case: every tool and the A/A arm once, in a seeded shuffled order, after one warmup round that is left out.
- A ratio is another tool's time over fetchloom's in the same round. Each comparison reports the median ratio with a paired percentile bootstrap interval, from `bench/ab.py`: 20,000 resamples, seed 1729, a seed of its own for each case and tool.
- The six headline cases are s1, s2r and s7 at home, s2r on the poor link, s5, and s10, all on ext4. Each sets fetchloom against one comparator frozen before the confirmatory runs: the fastest other tool that passed every run of the timing pilot within the budget, recorded in the `meta.json` of the campaign that measures the case. A comparator recorded only by a later chunk does not count. Choosing the rival from the data that judges it would widen the family past six, so a comparator that fails, or runs over the budget, leaves its case without a verdict rather than handing it to another tool. The six comparisons share a 5% error, so each interval is 99.2%. Every other case uses 95% intervals against its fastest tool that passed every run, and is descriptive, never a claim.
- The verdict reads the interval: fetchloom faster when its lower end is above 1.05, the other tool faster when its upper end is below 0.95, even only when the whole interval sits inside 0.95 to 1.05, and inconclusive otherwise.
- The A/A arm is never a rival. On lan and home it must pass every run, in the same rounds as fetchloom, with at least 10 pairs, and its 95% interval must sit inside 0.97 to 1.03. Otherwise the machine's noise is unmeasured or too large, and the case gets no verdict, in its table either. On the poor link its width is reported as the network's own randomness, and the rounds the pilot shows are needed instead of a gate.
- A verdict needs at least 10 paired rounds; every link runs 10, and the pilot may raise a count before the real run, never lower one after seeing data. A wide interval is inconclusive: no case is run again to replace it, and the renderer refuses a case measured twice.
- A tool with a failed run in a case shows "failed n of m" instead of a ratio, and a timeout is a failure. A case where fetchloom failed a run gets no verdict.
- A run over the politeness budget keeps its measurements, but a comparison it is in gets no verdict, since the two tools were not equally polite. A case where fetchloom went over the budget gets no verdict at all.
- s6 and s9 show each tool's time beside its guarantee, a stat, a hash or a question to the server, never as a win at equal checking, and are outside the headline set.
- The page gives the seconds first, then the ratio and its interval. It leads with home broadband and the poor link, then lan, names each link in plain words with its shaping, and names the faster tool where fetchloom loses.

## Running the comparison

1. A smoke pilot, `--quick` with one round: every tool passes every case and the checker agrees, the server log shows the budget held, and s2r's runs show both host names. It gives no timings.
2. A timing pilot at full size, one round of each case and tool, sets the sizes, deadlines and cost before the real run, and freezes each headline case's comparator: `bench/render.py --comparators` reads it, and every run of the real campaign gets the result. 64 MiB at 10 Mbit/s takes about 54 seconds without loss, and pooch, one file at a time, spends over 120 seconds on the round trips of 200 redirected files on the poor link.
3. A noise pilot: the A/A arm on s2r at home and on s6, 10 rounds. Its width sets the round counts, and a wide A/A interval means fixing the machine, not the analysis.
4. The real run, on the build being released, with nothing else running on the machine.

## Budget

These estimates come from earlier sizes and run times, and the timing pilot may move them.

| Part | Time |
|---|---|
| A, before and after | About 2 hours |
| B, against other tools | About 5 hours |
| C, real services | About 1 hour |
| The timing and noise pilots | About 1.5 hours |
| Total | About 9.5 hours of an otherwise idle machine, plus preparing fixtures, so it runs overnight |

## Platforms

| Machine | Filesystems |
|---|---|
| Linux x86_64 | ext4 for every scenario, and xfs for s5 and s6 on lan, each a dedicated loop-mounted image |

The harness gives no Windows or macOS numbers: namespaces, `tc`, loop mounts and `wait4` need Linux, and WSL results describe its Linux filesystem, not NTFS or Defender. [The native comparison](native.md) measures Windows, macOS and other filesystems on the machine itself, labelled as such.

## Results and presentation

The full comparison has not been published. Do not use smoke timings as results or write the generated results page by hand.

Each run writes `meta.json` and `runs.jsonl` under `$FL_BENCH_ROOT/results/` (default `/srv/fl-bench/results/`) on the Linux filesystem; copy wanted results into the ignored `bench/results/` before teardown. `bench/render.py` turns them into the cards under `docs/assets/bench/` and the results page, so every published number traces back to a results file. A `hero-*` card repeats a headline verdict that is not inconclusive.

## Not measured

- macOS and Windows in the lab; see [platforms](#platforms).
- Services that need an account the lab cannot reproduce, beyond the live runs above, and private remotes such as S3 buckets behind credentials.
- Resolution in the lab. `fetchloom lock` and the other tools' setup run while preparing, so the lab times no lookup of a record; the live runs time it against the services. A local stand-in for a repository's API would time it offline.
- Public buckets through the S3 API against `aws s3 sync`, s5cmd or rclone, which needs an S3-compatible server in the lab's namespace.
- A distant server or a wifi-like link of its own: the poor link covers a long round trip with loss, and a synthetic wifi profile would stand for no particular home.

## Prior art

| Project | What this design takes from it |
|---|---|
| [uv benchmarks](https://github.com/astral-sh/uv/blob/main/BENCHMARKS.md) | Cold and warm scenarios, stated OS and filesystem caveats |
| [pnpm benchmarks](https://github.com/pnpm/benchmarks-of-javascript-package-managers) | Each tool provisioned on its own, a local registry, a warm server, measuring kept apart from rendering |
| [hyperfine](https://github.com/sharkdp/hyperfine) | Warmup rounds, prepare steps before each run, recorded commands |
| [dvc-bench](https://github.com/iterative/dvc-bench) | Dataset sizes as a parameter |

Related: [measurements](measurements/README.md), [budgets](budgets.md), [Windows Defender](measurements/historical.md#windows-defender).

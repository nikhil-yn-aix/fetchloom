# Benchmark design

The benchmark measures how fetchloom 0.1.0 compares with the tools researchers use today on the jobs they do with data, and what each job costs.

[bench/](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md) implements it; [running benchmarks](README.md#running-benchmarks) gives the contributor route.

## The question

For the jobs researchers do with data, how does fetchloom compare with the tools they use today, with the same checked result and the same politeness to servers, on home broadband, a poor connection and a local network?

There is one fetchloom: the release build at the freeze. The control arm runs that same build a second time, to measure the machine's own noise; it is never a rival. The lab (`bench/run.py`) answers the question against a local server on shaped links, and the live runs (`bench/live.py`) set fetchloom beside each service's own tool on the real services. Claims come only from the eight headline cases in the [analysis plan](#analysis-plan); every other result describes what was measured and claims nothing.

## Tools

Each tool runs its normal documented workflow and checks what it downloads.

| Kind | Tool | How it runs |
|---|---|---|
| Downloader | curl | `curl -q --parallel --parallel-max 4`, `-L` for each transfer, then `sha256sum -c` inside the clock; `-C -` in s7 and `-z` in s6 and s9 |
| Downloader | aria2c | `aria2c -j 4 -x 4` with `--checksum` for each file; `-x 4` splits only the large file. `--check-integrity=true` where it already has files: s6, s9 and s12 |
| Durability control | curl+sync | curl as above, then `sync -f data`, only where many files are written |
| Notebook and pipeline | pooch | `pooch.retrieve` with `known_hash`, timed inside an interpreter that imported pooch first, one file at a time, its default; `Unzip` in s4 and `Untar` in s8 |
| Data versioning | DVC | `dvc pull -j 4` from an HTTP remote with `verify = true`, and reflink, hardlink or copy as the cache type |
| Data versioning | datalad | `datalad get -J 4` in a `datalad clone` made before the clock starts, from links registered with git-annex under SHA-256 keys |
| Team storage | rsync | a plain copy into and out of the shared folder, in s11 |
| Integrity check | sha256sum | `sha256sum -c` over the files curl downloaded, in s13 |
| Live services | `hf download`, zenodo_get, kagglehub, datahugger | each service's own tool, against the real service |

fetchloom runs `fetchloom -q --yes sync` in a project with `data.toml` and `data.lock`, with `FETCHLOOM_HOST_JOBS=4`; s11 and s13 use its push, mirror and check commands, below.

aria2c checks each file's SHA-256 after it downloads, one file at a time, and a downloaded file keeps its download slot until its check is done. With many small files its own status lines show three of its four slots finished and waiting on that check, so it keeps to about 12 files a second whatever the link, its CPU nearly idle: 1,000 files of 100 KiB take it 70 to 85 s, at home and on the lan alike. That pace is aria2c's own, not the lab's: it runs as one process on an input file with a `checksum=` line for each file, its documented way to download and check a list.

Left out, with the reason on the results page:

- wget fetches one file at a time and shows nothing curl does not.
- git-lfs and the S3 tools, rclone and `aws s3`, need a git or S3 server in the lab.
- openneuro-py and osfclient: datalad covers openneuro.

## Scenarios

| ID | Job | Links | Tools | Headline |
|---|---|---|---|---|
| s1 | Reproduce a pinned large file: 256 MiB at home, 4 MiB on the poor link | home, poor | the six | home, poor |
| s2r | Reproduce a pinned dataset in a fresh clone: 1,000 files of 100 KiB behind redirects, 50 on the poor link | home, poor | the six and curl+sync | home, poor |
| s3 | 10,000 tiny files of 4 KiB | lan | the six and curl+sync | |
| s4 | A zip of 10,000 members of 4 KiB, unpacked | home | fetchloom, curl, curl+sync, aria2c, pooch | |
| s8 | A tar.gz of 100 csv tables of about 1 MiB, unpacked | home | fetchloom, curl, curl+sync, aria2c, pooch | |
| s5 | s2r's files in a second project: time and extra disk | lan, on ext4 and xfs | the six and curl+sync | ext4 |
| s6 | s2r's command again with nothing to do, 1,000 files | lan, on ext4 and xfs | the six | |
| s9 | s3's command again with nothing to do, 10,000 files | lan | the six | |
| s7 | ctrl-c 10 seconds into s1, then the same command | home | the six | home |
| s10 | The link drops 10 seconds into s1, for 10 seconds, with nothing stopped | home | the six | home |
| s11 | One teammate pushes 1,000 files of 100 KiB to the team's shared folder, and another gets them with an empty cache; the push and the get are timed apart | lan | fetchloom, DVC, datalad, rsync | |
| s12 | The dataset's next version, in which 100 of its 1,000 files of 100 KiB changed | home | the six | home |
| s13 | Check that every file is intact, from a cold disk cache: 1,000 files of 2 MiB, about 2 GiB | this machine's disk | fetchloom, sha256sum, pooch, datalad | |
| live | Real services: zenodo, hugging face, kaggle and dataverse | this machine's datacenter link | fetchloom against each service's own tool | none |

The six are fetchloom, curl, aria2c, pooch, DVC and datalad. DVC and datalad track files, not archives, so they sit out s4 and s8. With no cache of its own, a second project costs curl and aria2c the whole download again, which is their real cost in s5.

A cold run empties every tool's cache, writes out the lab's filesystems, drops the machine's page cache and reads the server's files back, so the server is warm like a real one. Before every run, cold or warm, the lab writes out the test filesystems and then the disk holding their images, in that order, so no fsync a run times waits on data another layer left unwritten. Every run, cold or warm, also starts without what Linux kept from earlier connections between client and server, such as its estimate of packet reordering: the lab clears the server namespace's TCP metrics and the host's entry for the server. A warm run keeps its caches. s5 and s6 first run s2r's workflow outside the clock, and s9 s3's. In s5 fetchloom and DVC get a new project and datalad a clone of the first, all made before the clock starts, and pooch returns paths in its shared cache.

In s7 every tool gets ctrl-c (SIGINT) 10 seconds after its command starts, and a tool still running 5 seconds later gets SIGKILL. pooch's 10 seconds start at its first call, where its clock starts, so its import does not shorten the download it is stopped in. The same command then runs again, and its time is the one reported; curl continues with `-C -`.

In s10 the link drops at both ends 10 seconds after the command starts, or after pooch's first call starts, and comes back 10 seconds later: netem's loss goes to 100% for new packets, while the packets already queued still arrive, and nothing is stopped or signalled. The report sets each tool's total time against its own s1 on the same link and file, and gives how long it sat idle once the link was back. A run that ends before the link comes back failed, since it measured no drop.

In s11 the first teammate fetches the files from the lab's server outside the clock, then pushes them to a folder on the measured disk: fetchloom with `fetchloom push --to` into a folder mirror, DVC with `dvc push -r shared` to a local remote, datalad with `datalad push --to shared` to a sibling made by `datalad create-sibling`, and rsync with `rsync -a`. The second teammate then starts with an empty home and cache and gets the files from that folder: fetchloom with `fetchloom sync --locked --mirror`, DVC with `dvc pull -r shared`, datalad with `datalad get` in a clone of the sibling, and rsync by copying them back. The page cache is dropped before each, and a run fails if either asks the lab's server for anything.

In s12 each tool first has the version before in place, fetched outside the clock, then learns of the next version its own documented way, also outside the clock: fetchloom and DVC get the next `data.toml` and `data.lock`, or `data.dvc`, as a git pull brings them, datalad runs `datalad update --how merge`, and pooch, aria2c and curl get the new list of files and hashes. The timed command is each tool's own update: `fetchloom sync`, `dvc pull`, `datalad get`, pooch's `retrieve` by the new hashes and aria2c's `--check-integrity`, which fetch only what changed. curl has no way to tell what changed, so it downloads every file again, and that is its real cost. Each version lives at its own links, as repositories publish versions.

In s13 each tool first downloads the files its own way, outside the clock, then the page cache is dropped and each runs its documented check of every file it has: `fetchloom status --verify`, `sha256sum -c`, pooch's `retrieve`, which hashes the copy it has against its known hash, and `git annex fsck`. DVC has no command that hashes every file again. aria2c's `--check-integrity` asks the server for each file before it checks it, so it is not a local integrity check, and the results page says so. A run fails if the check asks the lab's server for anything.

Not benchmarked, with the reason on the results page:

- Cache cleanup: nobody waits on it.
- Search, diff, audit and export: no other tool does the same job.
- Push to S3 and private buckets: there is no S3 server in the lab.

## Fairness rules

1. Same result. Every tool starts from the same bytes and must leave every expected file in its place with the right SHA-256, which a checker outside the clock reads. A wrong or missing file is a failed run, never a fast one.
2. Same checking. Every tool checks the bytes it downloads, by the methods in [tools](#tools).
3. Same politeness. No tool has more than 4 requests in flight to one host, fetchloom's own default, each held to it by its own flags. The server's log shows whether it held, run by run, and a run over the budget gets no verdict. A faster mode that is less polite, such as aria2c's `-x 16`, is out of scope.
4. Same conditions. Every tool runs on the same machine and emulated link, one at a time, in a new seeded order every round. A cold job starts from cold disk caches. Every arm and tool runs in work folders whose paths have one length, since a longer path costs every lookup more kernel time, and logs go by a run id of one length. The measured commands can be pinned to CPUs apart from the lab server's.
5. Same redirect hop. In s2r every link answers with a 302 to a second host name, as hugging face, figshare, dataverse and github releases do, and every tool follows it; DVC's remote and the links registered with git-annex start there too. A run whose server log does not show both host names failed.
6. Durability shown, not hidden. fetchloom flushes what it places before it says done, and most tools do not. curl+sync, curl followed by a flush of the filesystem it wrote, shows what that costs where many files are written. The no-op cases name what each tool checks: a stat, a hash again or a question to the server.
7. Each tool's own clock boundary. A command-line tool is timed from its process start to its exit, interpreter startup included. pooch, a library, is timed around its calls inside an interpreter that has already imported it, as a notebook uses it, with the import time reported beside.
8. Nothing is tuned after seeing data. The headline cases and their rival are fixed from the pilot before the confirmatory runs, and sizes can only shrink, equally for every tool, and only from the pilot.
9. Pinned and recorded. Versions, flags, commands, the kernel, the filesystems, the CPU and the network shaping are written into the results.

## Network profiles

`tc netem` shapes the link between the client and the server namespace: half the round trip and the loss at each end, and the rate at the server's end.

| Profile | Stands for | Round trip | Loss each way | Server rate | Queue at the server | Large file | Many files |
|---|---|---:|---:|---:|---:|---:|---:|
| lan | The local network | 0 ms | 0% | Unlimited | None | Not run | 1,000 |
| home | Home broadband | 20 ms | 0% | 100 Mbit/s | 0.5 s, 4,250 packets | 256 MiB | 1,000 |
| poor | A poor connection | 300 ms | 3% | 10 Mbit/s | 1 s, 959 packets | 4 MiB | 50 |

A queue is set in time, not packets: the netem limit holds the profile's queue time at its rate, counting the packets its half of the delay holds, in packets of 1,500 bytes. The client's end carries requests and acknowledgements, which never fill netem's default of 1,000 packets. `meta.json` records each profile with its limit. s13 runs on this machine's disk, a profile with no shaping at all.

The poor link drives the uncertainty. On its 3% loss a 64 MiB fetchloom download took 411 to 713 seconds in earlier measured runs, and tools with one connection take several times that, so the poor link's sizes come from those runs, not from its rate.

ext4 runs every scenario on every link it applies to. xfs adds s5 and s6 on lan, where reflinks change reuse and no-op work.

## Metrics

Every run of every tool records these.

| Metric | How |
|---|---|
| Wall time, the primary measure | The monotonic clock from a command's start to its exit; for pooch, around its calls |
| CPU time, user and system | From the cgroup v2 each run has to itself, `cpu.stat`, so helper processes count; the command waits at a gate until it is in the cgroup, so everything it starts is there too. For pooch, its calls' own CPU time |
| Peak memory | The most anonymous memory the run's processes held at once, read from the cgroup's `memory.stat` every 20 ms. GNU time's peak of the largest single process is kept beside it, which alone understates datalad's git-annex children and DVC's workers |
| Disk used | The free-space change of the dedicated filesystem, so a link or reflink counts once |
| Bytes written | What the run wrote to the measured disk, from the cgroup's `io.stat` once the filesystem is flushed |
| Requests, bytes, connections and protocols | The server's log for that run |
| Requests in flight | Per host, the most at once, HTTP/2 streams included: each request counts from its arrival until nginx logged it, and for at least the link's round trip, since a client sends its next request on a slot only after the last answer reaches it, less 2 ms for the log's millisecond clock. On lan a request answered within 2 ms does not count, so there the figure is an estimate with a 2 ms floor, and the results say so beside it. A run over 4 is flagged, not failed |
| Server body bytes / dataset size | In s7, s10 and s12, the body bytes of the server's whole and partial answers for the dataset's files, over its size: what a tool asked for again. The server cannot see packets the network sent twice |
| Idle time once the link is back | In s10, how long after the link came back the client went on to receive another 64 KiB |
| Time lost to a dropped link | In s10, each tool's total time against its own s1 on the same link |
| Correctness | A gate, not a measure: every expected file's SHA-256, outside the clock. Failures are counted, never timed |

## Analysis plan

This plan is fixed before any data is seen.

- The unit is one round of one case: every arm once, in a seeded shuffled order, after one warmup round that is left out.
- A ratio is another tool's time over fetchloom's in the same round. Each comparison reports the median ratio with a paired percentile bootstrap interval, from `bench/ab.py`: 20,000 resamples, seed 1729, a seed of its own for each case and tool.
- Every arm runs rounds 1 to 5. In the eight headline cases fetchloom, its control and each frozen rival also run rounds 6 to 10, so each headline comparison has 10 pairs. Every other row is descriptive, with its interval from 5 rounds and no verdict.
- The headline cases are s1 at home and on the poor link, s2r at home and on the poor link, s5 on ext4, and s7, s10 and s12 at home, all on ext4. Each sets fetchloom against the rival frozen before the confirmatory runs: the fastest other tool that passed every run of the pilot within the budget, and the next fastest too when its pilot time was within 5% of it, recorded in the `meta.json` of the run that measures the case. A rival recorded only by a later chunk does not count. Choosing the rival from the data that judges it would widen the family, so a rival that fails, or runs over the budget, leaves its comparison without a verdict rather than handing it to another tool, and a headline case with no frozen rival gets no verdict at all. The headline comparisons share a 5% error, bonferroni-adjusted for their real number: eight, one for each case, and one more for each second rival, so each headline interval is 99.375% with eight and 99.444% with nine. Only a frozen rival's row gets a verdict; every other row in a headline case is marked descriptive. Every other case shows 95% intervals with no verdict, and the line under its table sets fetchloom beside its fastest tool that passed every run.
- The verdict reads the interval: fetchloom faster when its lower end is above 1.05, the other tool faster when its upper end is below 0.95, even only when the whole interval sits inside 0.95 to 1.05, and inconclusive otherwise.
- The control arm runs only in the headline cases and is never a rival. On home, lan and this machine's disk it must pass every run, in the same rounds as fetchloom, with at least 10 pairs, and its 95% interval must sit inside 0.97 to 1.03. Otherwise the machine's noise is unmeasured or too large, and the case gets no verdict, in its table either. On the poor link its width is reported as the network's own randomness.
- Each run records its planned rounds per case and arm in `meta.json` before the case starts, a case's rows must come from the run that lists it, and fetchloom must have run exactly its planned rounds: a run cut short publishes no verdicts, even when every arm stopped at the same round and the rounds still pair. A wide interval is inconclusive: no case is run again to replace it, and the renderer refuses a case measured twice.
- A tool with a failed run in a case shows "failed n of m" instead of a ratio, and a timeout is a failure. A case where fetchloom failed a run gets no verdict.
- A run over the politeness budget keeps its measurements, but a comparison it is in gets no verdict, since the two tools were not equally polite. A case where fetchloom went over the budget gets no verdict at all.
- s6 and s9 show each tool's time beside its guarantee, a stat, a hash or a question to the server, never as a win at equal checking.
- The page gives absolute seconds first, then the ratio and its interval. It leads with home broadband and the poor link, then the local network and this machine's disk, names each link in plain words with its shaping, and says so where fetchloom loses.

## Running the comparison

1. A smoke run, `--quick` with one round, of every case on every link: every tool passes every case and the checker agrees, the server log shows the budget held, and s2r's runs show both host names. It gives no timings.
2. A pilot at full size, one round of every case and tool with no warmup and one live round, at the freeze, on the frozen build and a quiet machine. It checks correctness and the budget and projects the hours of the real run: each run's cost, its untimed work included, times the runs the real run gives its arm, and each live service's reference download and 10 rounds. Its rules are declared before any data, and `bench/project.py` applies them unattended:
    - If the projection is over 8 hours, the poor link's sizes shrink, equally for every tool: its large file and its file count shrink by the largest factor, in hundredths, that brings the projection to 8 hours, taking each poor run's timed part as proportional to the size that shrinks in it and its untimed work as fixed, rounded down to whole MiB and files, and never below 1 MiB and 10 files.
    - Each headline case's rival is the fastest other tool that passed every run within the budget.
    - If the next fastest tool's time is within 5% of that rival's, both are frozen: both run the 10 rounds, and the bonferroni family grows to the real number of headline comparisons.
    - If the projection is still over 9 hours after the poor sizes shrink, the live part moves to a separate daytime run.
    - If it is still over 9 hours without the live part, s3 and s9, the 10,000 tiny files, move to a separate block on the same frozen build.

    `bench/render.py --comparators` reads the pilot and writes the frozen rivals, and every part of the real run gets them. Rounds and tools never shrink after seeing data: descriptive arms keep their 5 rounds, and only the poor sizes shrink.
3. The real run, started right after the pilot on the same frozen build, with nothing else running on the machine. The live part and the s3 and s9 block, when the rules move them out, run later on the same build.

## Budget

These estimates come from link rates and earlier measured runs; the pilot fixes them.

| Step | Hours |
|---|---:|
| Fixtures: lab sets, DVC repositories, datalad datasets, archives, and the s11 to s13 sets | 0.3 |
| The pilot | 0.8 |
| Home broadband, 7 cases | 2.7 |
| The poor link, 2 cases | 1.5 |
| The local network and this machine's disk | 1.1 |
| Live services, 4 | 1.0 |
| Total, unattended | About 7.5 |

The release build comes from GitHub's release workflow, so the run never waits on a build; only the results page waits on the run.

## Real services

`bench/live.py` fetches public datasets with fetchloom and with each service's own tool, interleaved: 10 rounds, the first in a seeded random order and each after it the other way round (ABBA), every run with empty caches. The live run covers zenodo, hugging face, kaggle and dataverse; figshare stays available to `--services`.

| Service | Dataset | Other tool |
|---|---|---|
| zenodo | [8164711](https://zenodo.org/records/8164711), `pbmc.RData` | zenodo_get |
| hugging face | [stanfordnlp/imdb](https://huggingface.co/datasets/stanfordnlp/imdb), files matching `plain_text/*.parquet`, revision `e6281661ce1c48d982bc483cf8a173c1bbeb5d31` | `hf download` with hf_xet, then `sha256sum -c` |
| kaggle | [zynicide/wine-reviews](https://www.kaggle.com/datasets/zynicide/wine-reviews), version 4, `winemag-data-130k-v2.csv` | kagglehub, then a SHA-256 check |
| figshare | [10.6084/m9.figshare.5616403](https://api.figshare.com/v2/articles/5616403), files in the current article listing | datahugger |
| dataverse | [doi:10.7910/DVN/TJCLKP](https://dataverse.harvard.edu/dataset.xhtml?persistentId=doi:10.7910/DVN/TJCLKP), current published version and public raw files, recorded at preparation | datahugger |

`live.py` reads metadata, sizes and exact selections during preparation and downloads an independent reference before timing either client. Both clients keep archives packed; their file hashes are compared with the reference, and layout differences are reported apart from content differences. The hugging face and kaggle clients take no expected SHA-256, so their timed commands check it after the download, as curl's do. datahugger gets the explicit dataverse landing URL and an identified `fetchloom-benchmark/1 datahugger/0.13` User-Agent, since the [bench access notes](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md#run) record its DOI parser and default User-Agent being refused; its timed call uses `checksum=True` and `unzip=False`. In the full dataverse dataset fetchloom requests the original TSV and datahugger the ingested TAB, so the bytes differ and the run fails the shared check rather than counting as a speed result.

A live service's speed changes from minute to minute, but a slow minute slows both tools of a round alike, so the results pair the rounds: the service's own tool's time over fetchloom's, with a 95% paired bootstrap interval, on this machine's datacenter link. These ratios hold for that run alone. They get no verdict and confirm none of the lab's rankings, and a content difference stays a failure. Quick runs use the zenodo 3608990 iris record, hugging face `scikit-learn/iris` (`Iris.csv`), kaggle `uciml/iris` version 2 (`Iris.csv`), and files no larger than 1 KiB in the figshare and dataverse datasets.

## Platforms

| Machine | Filesystems |
|---|---|
| Linux x86_64 | ext4 for every scenario, and xfs for s5 and s6 on lan, each a dedicated loop-mounted image |

The images sit on the machine's local SSD, as on a researcher's laptop or workstation, never on a network-attached OS disk that the machine's logs, caches and temporary files also write to: an fsync inside an image ends in one of the image file, which waits on whatever that disk holds unwritten. Each run's `meta.json` names the disk under the lab and under each image, and the results page names it too.

The harness gives no Windows or macOS numbers: namespaces, `tc`, loop mounts and cgroups need Linux, and WSL results describe its Linux filesystem, not NTFS or Defender. [The native comparison](native.md) measures Windows, macOS and other filesystems on the machine itself, labelled as such.

## Results and presentation

The results have not been published. Do not use smoke timings as results or write the generated results page by hand.

Each run writes `meta.json` and `runs.jsonl` under `$FL_BENCH_ROOT/results/` (default `/srv/fl-bench/results/`) on the Linux filesystem; copy wanted results into the ignored `bench/results/` before teardown. `bench/render.py` turns them into the cards under `docs/assets/bench/` and the results page, so every published number traces back to a results file. A `hero-*` card repeats a headline verdict that is not inconclusive.

## Not measured

- macOS and Windows in the lab; see [platforms](#platforms).
- Services that need an account the lab cannot reproduce, beyond the live runs above, and private remotes such as S3 buckets behind credentials.
- Resolution in the lab. `fetchloom lock` and the other tools' setup run while preparing, so the lab times no lookup of a record; the live runs time it against the services. A local stand-in for a repository's API would time it offline.
- A distant server or a wifi-like link of its own: the poor link covers a long round trip with loss, and a synthetic wifi profile would stand for no particular home.

## Prior art

| Project | What this design takes from it |
|---|---|
| [uv benchmarks](https://github.com/astral-sh/uv/blob/main/BENCHMARKS.md) | Cold and warm scenarios, stated OS and filesystem caveats |
| [pnpm benchmarks](https://github.com/pnpm/benchmarks-of-javascript-package-managers) | Each tool provisioned on its own, a local registry, a warm server, measuring kept apart from rendering |
| [hyperfine](https://github.com/sharkdp/hyperfine) | Warmup rounds, prepare steps before each run, recorded commands |
| [dvc-bench](https://github.com/iterative/dvc-bench) | Dataset sizes as a parameter |

Related: [measurements](measurements/README.md), [budgets](budgets.md), [Windows Defender](measurements/historical.md#windows-defender).

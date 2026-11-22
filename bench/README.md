# Benchmarks

This suite runs the benchmark of fetchloom 0.1.0 against the tools researchers use today: time to checked files, cache reuse, updates, team sharing, integrity checks, no-op work and interrupted downloads. [The design](../docs/dev/performance/benchmark-design.md) defines the cases, the fairness rules and the analysis.

To compare fetchloom builds on Windows, macOS or any other filesystem without this lab, use `native.py` ([native comparison](../docs/dev/performance/native.md)). It records wall time and command success, not independent output hashes; the [native checks](../docs/dev/performance/native-checks.md) add correctness and platform probes.

## Requirements

Use Linux x86_64, Python 3.12, root, network namespaces, loop devices, ext4 and xfs, and cgroup v2 with the cpu, memory and io controllers. The client and nginx share a kernel but use separate network namespaces. All measured files, caches and logs live under `FL_BENCH_ROOT` (default `/srv/fl-bench`), not a Windows mount. Leave at least 30 GiB free. Do not run other benchmarks at the same time: dropping page caches affects the whole machine. Before a drop and around each run, the lab writes out only its own filesystems (`syncfs` on `FL_BENCH_ROOT` and the two test mounts), so a hung mount elsewhere on the machine cannot stall a run the way a global `sync` would.

On Ubuntu 24.04, these are the native package pins the smoke test used:

```sh
apt-get update
apt-get install -y \
  nginx=1.24.0-2ubuntu7.21 aria2=1.37.0+debian-1build3 \
  curl=8.5.0-2ubuntu10.16 rsync=3.2.7-1ubuntu1.5 \
  git-annex=10.20240129-1build1 git=1:2.43.0-1ubuntu7.4 \
  iproute2=6.1.0-1ubuntu6.4 xfsprogs=6.6.0-1ubuntu2.1 \
  e2fsprogs=1.47.0-2.4~exp1ubuntu4.1 unzip=6.0-28ubuntu4.1 \
  openssl=3.0.13-0ubuntu3.16 python3=3.12.3-0ubuntu2.1 \
  time=1.9-0.2build1
systemctl disable --now nginx
```

The lab starts its own nginx in a network namespace; disable the system service after installation. Use an Ubuntu package snapshot if a pin has left the mirror. Python clients share one environment, pinned in [pyproject.toml](pyproject.toml) and locked with every transitive version and hash in [uv.lock](uv.lock); each measured command has its own empty home and cache. The runner and the chart renderer import only the standard library. Install [uv](https://docs.astral.sh/uv/getting-started/installation/) where root can run it. uv creates the environment without `python3-venv`; `FL_BENCH_VENV` selects its path (default `/opt/fl-bench/venv`).

## Build the measured executable

The benchmark measures the release build at the freeze, which GitHub's release workflow makes. To smoke-test the harness with a build of your own, from the repository root in a Linux shell with the [build prerequisites](../docs/dev/engineering/building-and-testing.md#build-requirements), build as your normal user:

```sh
cargo build --locked --release --target-dir target
target/release/fetchloom --version
```

In WSL, open that shell from PowerShell at the repository root with `wsl --cd $PWD.Path`. Build a Linux executable there, not a Windows executable from PowerShell. The source may be on a Windows mount; measured state must stay on the Linux filesystem.

## Setup

Use a root shell in the same checkout for setup and runs. From PowerShell at the repository root, `wsl --cd $PWD.Path --user root` opens one in the default WSL distribution. On native Linux, enter the same checkout after switching to your root shell. Set the binary and lab paths in that shell. The two paths are canonicalized at startup, so a directory symlink names the same lab for setup, output checks and teardown. Keep both `FL_BENCH_` variables set for setup, tests, runs and teardown; with sudo, pass them explicitly through `sudo env`:

```sh
benchmark_binary="$(realpath target/release/fetchloom)"
"$benchmark_binary" --version
export FL_BENCH_ROOT="${FL_BENCH_ROOT:-/srv/fl-bench}"
export FL_BENCH_VENV="${FL_BENCH_VENV:-/opt/fl-bench/venv}"
python3 bench/lab.py setup
export REQUESTS_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
export TMPDIR="$FL_BENCH_ROOT/scratch"
UV_PROJECT_ENVIRONMENT="$FL_BENCH_VENV" UV_PYTHON_DOWNLOADS=never uv sync --project bench --locked
python3 -m unittest discover -s bench -p 'test_*.py'
```

Put `FL_BENCH_ROOT` on the machine's local SSD, never on a network-attached OS disk: the test filesystems are images in it, so every fsync a tool makes ends in an fsync of its image file there, which waits on whatever else that disk holds unwritten, and a cloud machine's OS disk also takes its logs, caches and temporary files. `meta.json` records the disk under the lab and under each image, with its model, transport and whether it is rotational.

Set `FL_BENCH_LAB_CHECKS=1` on the test command, as root inside a set-up lab, to check that switching network profiles leaves no earlier rate cap behind, that each capped link holds its queue limit, and that a run's cgroup counts every process it starts. Set `FL_BENCH_LIVE_CHECKS=1` on the test command, inside a set-up lab, to also check that the hugging face and kaggle wrappers reject deliberately wrong expected hashes, using tiny public files. `FL_BENCH_FETCHLOOM` names the binary they use, `/home/fl/bin/fetchloom` by default. CI runs the other checks on every pull request.

Setup creates `flb-srv`, `flb-client` and `flb-server`, adds `lab.flb` and `store.flb` to `/etc/hosts` on one line marked `# fl-bench`, installs a disposable CA for both names, starts nginx and mounts two 12 GiB sparse images. The server answers each path under `https://lab.flb/r/` with a 302 to the same path on `store.flb`, as a repository sends its downloads to a storage host. Setup replaces only a lab with its ownership marker. It refuses conflicting unowned resources. Setup also checks that HTTPS answers on both names, through the redirect.

## Run

The benchmark runs in three steps: a smoke run, a pilot, and the real run. ext4 runs every scenario on every link it applies to; xfs adds the two scenarios where reflinks change the result, the second project and the no-op. Pick CPU lists for your machine: `--cpus` holds the measured commands and `--server-cpus` the lab's nginx, apart from them.

### Smoke

Smoke runs use 12 files per set, csv tables of 64 KiB for s8, a 2 MiB large file, 12 files of 2 MiB for s13 and one measured round. s7 and s10 keep the whole 256 MiB file, so it is still downloading when SIGINT arrives or the link drops at 10 s. Every tool must pass every case:

```sh
python3 bench/run.py --fetchloom "$benchmark_binary" --quick --control \
  --profiles lan,home,poor,local --fs ext4 --out "$FL_BENCH_ROOT/results/quick/lab-ext4"
python3 bench/run.py --fetchloom "$benchmark_binary" --quick --control \
  --profiles lan --scenarios s5,s6 --fs xfs --out "$FL_BENCH_ROOT/results/quick/lab-xfs"
python3 bench/live.py --fetchloom "$benchmark_binary" --quick \
  --out "$FL_BENCH_ROOT/results/quick/live"
```

### Pilot

One full-size round of every case and tool, with no warmup round, and one live round, check correctness and the budget and give each run's cost, from which `project.py` projects the real run's hours and applies the pilot's rules, which the design's [running the comparison](../docs/dev/performance/benchmark-design.md#running-the-comparison) declares:

```sh
python3 bench/run.py --fetchloom "$benchmark_binary" --rounds 1 --no-warmup --control \
  --profiles lan,home,poor,local --fs ext4 --cpus 12-23 --server-cpus 6-11 \
  --out "$FL_BENCH_ROOT/results/pilot/lab-ext4"
python3 bench/run.py --fetchloom "$benchmark_binary" --rounds 1 --no-warmup --control \
  --profiles lan --scenarios s5,s6 --fs xfs --cpus 12-23 --server-cpus 6-11 \
  --out "$FL_BENCH_ROOT/results/pilot/lab-xfs"
python3 bench/live.py --fetchloom "$benchmark_binary" --rounds 1 \
  --services zenodo,huggingface,kaggle,dataverse --out "$FL_BENCH_ROOT/results/pilot/live"
python3 bench/project.py "$FL_BENCH_ROOT/results/pilot" > plan.json
python3 -c 'import json; print(json.dumps(json.load(open("plan.json"))["comparators"]))' > comparators.json
```

Each row's `run_seconds` is what its run cost the whole run, untimed work included, and each case in `meta.json` keeps its `prepared_seconds`. `project.py` projects nothing from a pilot that cannot stand for the real run: a smoke run, a case or live service of the real run it never measured, an arm with no run, more than one round or a warmup, or a run with no cost on record. It names each and exits 2, so no real run starts from it. `plan.json` holds the projected `hours` of home, poor, lan and live, the `comparators` frozen for each headline case, the `scale` for the poor link's sizes with the `poor_sizes` it gives, the hours after it, whether the `live` part runs with the lab or moves to a daytime run, and `apart`, the cases that leave the run as a block of their own, with their `apart_hours`; `total_hours` is the run's without what moved. `render.py --comparators "$FL_BENCH_ROOT/results/pilot"` gives the same comparators: for each headline case the pilot measured, the fastest other tool that passed every run within the budget, and the next fastest too when its time is within 5% of it.

### The real run

Give every run the pilot's comparators, and the scale from `plan.json` as `--poor-scale` when it is below 1; leave the live part for a daytime run when `plan.json` says so. When `apart` lists s3 and s9, the ext4 run takes `--scenarios s1,s2r,s4,s5,s6,s7,s8,s10,s11,s12,s13`, and the two run later as their own block on the same build, with the same comparators:

```sh
python3 bench/run.py --fetchloom "$benchmark_binary" --control --comparators comparators.json \
  --profiles lan,home,poor,local --fs ext4 --seed 1729 --cpus 12-23 --server-cpus 6-11 \
  --out "$FL_BENCH_ROOT/results/full/lab-ext4"
python3 bench/run.py --fetchloom "$benchmark_binary" --control --comparators comparators.json \
  --profiles lan --scenarios s5,s6 --fs xfs --seed 1729 --cpus 12-23 --server-cpus 6-11 \
  --out "$FL_BENCH_ROOT/results/full/lab-xfs"
python3 bench/live.py --fetchloom "$benchmark_binary" --services zenodo,huggingface,kaggle,dataverse \
  --seed 1729 --out "$FL_BENCH_ROOT/results/full/live"
python3 bench/run.py --fetchloom "$benchmark_binary" --control --comparators comparators.json \
  --profiles lan --scenarios s3,s9 --fs ext4 --seed 1729 --cpus 12-23 --server-cpus 6-11 \
  --out "$FL_BENCH_ROOT/results/full/lab-apart"
```

`--control` runs fetchloom a second time in each headline case, under `FETCHLOOM_EXP_NONE=1`, a setting it never reads: this control arm measures the machine's own noise, and its interval decides whether a headline case on a link without loss may have a verdict. `--comparators comparators.json`, given to every part of the run, records the frozen rivals in `meta.json`. A run refuses a comparator it does not measure, and the renderer takes a case's comparator only from the `meta.json` of the run that measured it, so one recorded by a later chunk never judges it.

Every arm runs 5 measured rounds after one warmup round. In a headline case fetchloom, its control and each frozen comparator run on to 10, so each verdict rests on 10 pairs. `--rounds N` gives every arm N rounds instead, and a smoke run gives each one. The runner prints a planning estimate before fixture preparation, assuming a minute a run, which overstates lan cases; the estimate is for planning, not a promised completion time.

### Details

- Every invocation needs a new output directory. A whole run may come in parts, one `--profiles` or `--scenarios` slice per `--out` folder under one results root; each case draws its own seeded order, and the renderer combines the parts as long as no case is measured twice.
- `--only-tools fetchloom,curl` narrows the lab clients and `--services zenodo,huggingface` the live services. A fetchloom-only run needs no other client or Python environment; only selected clients get native preparation and version checks.
- Each case runs the tools of the design's table. `curl+sync` is curl followed by a flush of what it wrote, to show what fetchloom's durable writes cost, where many files are written: s2r, s3, s4, s5 and s8. rsync runs only in s11 and sha256sum only in s13.
- Unsupported combinations are left out: s1 and s2r run at home and on the poor link, s3, s5, s6, s9 and s11 on lan, s4, s7, s8, s10 and s12 at home, and s13 on `local`, this machine's disk, a profile with no shaping.
- Each measured command has a 20-minute timeout by default. `--timeout` must be finite and positive, and invalid settings are refused before a client starts. Interruption and timeout deadlines include process startup, and a timer that fails to start kills and reaps the started group. pooch's interruption and s10's drop count from its first call instead, which its script marks in `calls.begin` on the machine's monotonic clock just before the calls; a run that has not marked it by the deadline timed out. s7's first attempt keeps `anchor_seconds` and s10's `link_drop` keeps `anchor_at`, how far into the command the calls began.
- A failed command, timeout or wrong hash is a failed record, not a short timing.
- In s10 the home link drops 10 s into s1's download, for 10 s: `tc qdisc change` sets netem's loss to 100% at both ends, so new packets are lost while those already queued still arrive, and nothing is stopped. Each s10 run starts on the whole profile again. A run that finishes before the link comes back failed, since it measured no drop. The row keeps `link_drop`, with `resumed_at`, when the client's end of the link had received another 64 KiB once the link was back; the renderer sets each tool's time against its own s1 on the same link and shows how long it sat idle.
- In s2r each file's link goes through the redirect, for every tool: curl gets `-L` for each transfer, and DVC's remote and datalad's registered links go through it too. A run whose server log does not show both host names failed.
- In s11 the row keeps the push under `push`, with its own wall, CPU, memory, bytes written and disk, and the get as the row's own figures. A run fails if the push fails or either asks the lab's server for anything.
- In s12 the case keeps the version before under `previous`, served at links of its own, and the row's `body_bytes_ratio` counts both versions' files over the next version's size. In s13 a run fails if the check asks the lab's server for anything.
- Every tool keeps to 4 requests in flight to one host by its own flags: fetchloom's `FETCHLOOM_HOST_JOBS=4`, curl `--parallel-max 4`, aria2c `-j 4 -x 4`, DVC `-j 4` and datalad `-J 4`; pooch fetches one file at a time. nginx logs each request's host, protocol, connection, its number on that connection, how long it took and when it ended. Each row records the connections, the most requests on one connection and, per host, the most requests and connections in flight at once, HTTP/2 streams included. A request counts as in flight from its arrival until nginx logged it, and for at least the link's round trip, less 2 ms for the log's millisecond clock, so on lan a request answered within 2 ms does not count. The lan figure is therefore an estimate with a 2 ms floor: each row records the floor as `in_flight_floor_ms`, 0 where every request counts, and the results mark the column as estimated and state the floor under each such table. A run over the budget has `over_budget` set and stays a passed run, with its measurements in the results, but no comparison it is in gets a verdict, since the two tools were not equally polite; when fetchloom's own run is over, its case gets none.

### Cgroups

Every run has a cgroup v2 of its own under `/sys/fs/cgroup/fl-bench`, which the runner makes with the cpu, memory and io controllers before the first run; a machine that cannot make it stops the run before it starts. The measured command starts in `sh` and waits on a pipe until the runner has moved it into the cgroup, then runs GNU time, so everything it starts is counted, helpers and workers included. Each row records:

- `user_seconds` and `sys_seconds`: the cgroup's `cpu.stat` over the command, with GNU time's wait, which sees only the processes it waited for, kept under `rusage`;
- `memory_bytes`: the most anonymous memory the cgroup held at once, read from `memory.stat` every 20 ms, and `memory_samples`, how many readings it rests on; GNU time's peak of the largest single process stays in `maxrss_bytes`;
- `written_bytes`: what the cgroup's `io.stat` counts as written to the measured disk once the runner has flushed it, so writes the kernel makes later count too.

s7's two attempts share one cgroup, and s11's push has its own. Whatever a command leaves running is killed with its cgroup when the run ends. `live.py` uses no cgroup and keeps GNU time's figures alone.

### Variants

To compare a setting the measured build accepts, add `--variant label:FETCHLOOM_X=1`, once per variant. Each variant syncs the same prepared project with its own environment, shuffled into the same rounds as the plain build; the runner tests cover twelve variants plus the base, and there is no limit. The case seed gives the same order for the same selected names, and each row records its variant settings; adding variants changes the shuffle and can lengthen the gap between paired runs. `python3 bench/ab.py <out>` reports wall, CPU, memory, disk, server bytes and requests from matched successful rounds: the median log ratio `ln(variant) - ln(plain)`, its exponentiated ratio, a paired percentile bootstrap 95% interval, wins and metric-specific pair counts. A metric enters a pair only when both sides have finite values; zero and negative pairs keep their win and tie counts but have no log ratio. In the example, `FETCHLOOM_SPLIT=1` asks eligible large downloads to split from the first answer.

```sh
python3 bench/run.py --fetchloom "$benchmark_binary" --only-tools fetchloom --profiles home \
  --scenarios s1 --fs ext4 --rounds 20 --variant split:FETCHLOOM_SPLIT=1 \
  --out "$FL_BENCH_ROOT/results/ab/split"
python3 bench/ab.py "$FL_BENCH_ROOT/results/ab/split" --threshold .03 \
  --json "$FL_BENCH_ROOT/results/ab/split/comparison.json"
```

The bootstrap uses 20,000 resamples and seed 1729 by default; `--resamples` and `--seed` change those analysis settings. Seeds are separate per case, variant and metric, so adding a variant does not change another interval. A win requires the entire interval below `1 - threshold`, at least 10 positive pairs (`--min-pairs`), and no failed, unmatched or excluded metric pairs. `--threshold .03` means an upper bound strictly below 0.97. Increase `--rounds` to the noise-floor recommendation; bootstrap draws do not replace measured rounds. A quick run cannot establish a win. Duplicate round identities and mixed workload sizes, quick modes or CPU masks are rejected; unmatched round numbers remain explicit in the results. Intervals are per comparison, without a multiple-comparison correction; confirm selected winners independently.

### Clients

pooch runs as a notebook uses it: its script imports pooch, then times its `retrieve` calls itself, wall time and the process's CPU around them, and writes them with the import time to `calls.json` before it exits, even when stopped. Those times replace the process's, which the row keeps under `process`, and `clock` says which a row's times cover. s5 fills that client's cache from s2r before each run. s6 runs the same s2r workflow once before timing, and s9 the s3 workflow. DVC uses one shared cache directory and pooch returns its shared cached paths. datalad's clone, of the prepared dataset or for s5 of the first project, is made before the clock starts, like DVC's project copy. Cache population is outside the clock. Prepared download inputs, DVC repositories and URL-registered annex datasets are built once, before the runs; for s12 they hold both versions, DVC's and datalad's under the git tags `previous` and `next`. Fixture manifests record their dimensions, seed and generator digest; preparation markers record fixture inputs, harness and lock digests and the fetchloom executable digest where applicable. A mismatch or incomplete marker rebuilds the owned preparation outside the clock.

Live runs check a separately downloaded reference against the public manifest, then compare the clients' file SHA-256s and layouts. Metadata is atomically saved and flushed before each measured command, so interrupted runs retain their inputs, orders and command wrappers. Preparation and checker exceptions become failed rows rather than disappearing from the record. Quick inputs are zenodo's iris record, one hugging face iris CSV, kaggle's iris dataset, the two sub-KiB figshare files and the sub-KiB dataverse file. Inaccessible services and anonymous kaggle refusals have recorded skip reasons. The hugging face and kaggle commands include the expected SHA-256 check because their download APIs have no such option. A full run runs 10 rounds per service and a quick one runs 1. The first round's order is seeded and each after it is the other way round (ABBA). The results give the other tool's time over fetchloom's, rounds paired, with a 95% interval and no verdict, since it holds for that run alone.

datahugger uses an explicit dataverse landing URL and an identified User-Agent because its DOI parser and the default Python User-Agent are rejected there. Its generated checksum reports are excluded from dataset comparisons, but still count toward time and disk.

The full dataverse TSV and TAB representations differ, which is reported as a content mismatch; the quick shared file has identical bytes.

Client processes do not inherit service credentials, native client configuration overrides, Git overrides or Python import and cache-location overrides. Git reads neither system nor global configuration; repository-local settings still apply. curl starts with `-q` to disable curlrc. fetchloom refuses a private address that a project names unless `allowed_origins` lists its origin, so every lab command gets `FETCHLOOM_ALLOWED_ORIGINS` naming the lab server's. These controls and the remaining flags are recorded. Continue to use a clean dedicated environment; proxy settings for public live services remain intentional inputs.

## Measurement island

On Linux, set `FL_BENCH_CPUS=0-7` or pass `--cpus 0-7` to `run.py` or `live.py`. The runner validates the list against its available CPUs, pins nginx's master and worker threads to it, or to `--server-cpus` when that is given, and gives every measured command the mask before it can execute. Children inherit it. `lab.py setup --cpus 0-7` also starts nginx on that mask. Every run records the effective command and nginx masks. An unset or empty setting uses the runner's available CPUs, including when resetting a server left pinned by an earlier run.

CPU affinity does not isolate shared cache, memory bandwidth, filesystem flushes or disk traffic. Run benchmarks alone on the machine until a controlled check shows that builds on other cores leave the noise where it was. Do not claim a quieter island from unit tests alone.

`island.py check` is an operator-only measurement. It measures full-size A/A runs first with a quiet island, then with a repeated release build on disjoint cores. `--case filesystem/profile/scenario` is repeatable and selects exactly those cases; the default is ext4/lan s3, s5 and s6. Use the unchanged executable from the quiet noise run. It compares both paired noise distributions with `noise.json` and reports the loaded/quiet-island wall shift. Default acceptance requires 20 complete pairs per case, A/A 95% intervals within 0.97 to 1.03, robust log spread no more than twice the noise run's spread (with a 0.01 log-scale floor), and a central loaded/quiet-island shift within 3%. The loaded interval width must stay within twice the larger quiet or noise-run width (with a 1-percentage-point floor), and the A/A median bias may shift by at most 1 percentage point. The width and bias floors are one third of `--effect`. These are explicit operational checks, not a proof of independence or an 80% power guarantee.

Run the check with nothing else on the machine: it creates its own compiler load on the build CPUs. Give it a new target directory used by no other process, because the load worker repeatedly cleans fetchloom in that target before rebuilding. Load failure aborts the measurement, and the worker group is stopped on exit. Source is read without modification. `--noise` names the `ab.py --json` output of an earlier quiet A/A run on the same machine and lab, with the same cases, measured like this:

```sh
python3 bench/run.py --fetchloom "$benchmark_binary" --only-tools fetchloom \
  --profiles lan --scenarios s3,s5,s6 --fs ext4 --rounds 20 \
  --variant aa:FETCHLOOM_EXP_NONE=1 --cpus 0-7 --out "$FL_BENCH_ROOT/results/noise"
python3 bench/ab.py "$FL_BENCH_ROOT/results/noise" --json "$FL_BENCH_ROOT/results/noise.json"
```

Then, as root, after sourcing the Rust toolchain environment:

```sh
sudo env PATH="$PATH" \
  FL_BENCH_ROOT="$FL_BENCH_ROOT" FL_BENCH_VENV="$FL_BENCH_VENV" \
  python3 -B bench/island.py check --fetchloom "$benchmark_binary" \
  --cpus 0-7 --build-cpus 8-23 --noise "$FL_BENCH_ROOT/results/noise.json" \
  --worktree "$PWD" --target "$FL_BENCH_ROOT/island-target" \
  --out "$FL_BENCH_ROOT/results/island-check"
```

Keep `island.json`, the results of both A/A runs and `build.log`. The target directory is left for inspection. `--effect`, `--noise-multiple`, `--rounds` and `--jobs` state the acceptance and load settings; choose them before measuring. A check failure means builds should remain excluded, not that the benchmark should discard its slower observations.

## Render and keep results

```sh
python3 bench/render.py "$FL_BENCH_ROOT/results/full"
mkdir -p bench/results
cp -a "$FL_BENCH_ROOT/results/full" bench/results/
```

Rendering requires a matching `meta.json` beside every `runs.jsonl`. It rejects repeated live-service runs, inconsistent dataset sizes and reference hashes, and mixed quick and full inputs before deleting any chart already there. Split service runs may be combined only when each service occurs once. Rendering writes `docs/dev/performance/benchmarks.md` and one SVG card per case in `docs/assets/bench/`, each with a `-dark.svg` twin for GitHub's dark theme. It first removes every SVG card already in that folder, so no card, including a `hero-*` card, outlives the results that produced it. The page leads with the links most people have, home broadband and a poor connection, then the local network and this machine's disk, and names each in plain words with its recorded shaping, such as `home broadband, about 100 Mbit and 20 ms`.

Each case's table gives absolute seconds first, then sets every other tool's time over fetchloom's, round by round, with `ab.py`'s paired percentile bootstrap: for a comparator frozen in one of the eight headline cases (s1 and s2r at home and on the poor link, s5 on ext4, and s7, s10 and s12 at home) a 99.375% interval, marked in its row, since those comparisons share a 5% error, or narrower odds still when a case froze two comparators and the family grows to their real number, and 95% for every other row, the control arm and a headline case's other tools included, which describe and claim nothing. A description in a headline case rests on its 5 rounds, paired with fetchloom's first 5. Only a headline case's frozen comparators get a verdict, never a tool chosen from the same data, and a headline case with none frozen gets none at all; every other row is marked descriptive, and a case outside the headline set only describes its fastest other tool that passed every run. A verdict needs at least 10 paired rounds: fetchloom faster when the interval's lower end is above 1.05, the other tool faster when its upper end is below 0.95, even when the whole interval sits inside 0.95 to 1.05, and inconclusive otherwise. The control arm is never a rival; on every link without loss it must pass every run, in the same rounds as fetchloom, with at least 10 pairs, and its 95% interval must sit inside 0.97 to 1.03, or the case gets no verdict, and the table withholds every tool's verdict in it. A case where fetchloom failed a run gets none either, nor does one where fetchloom did not run exactly the rounds its run's `meta.json` planned, so a run cut short publishes no verdicts, and a tool with a failed run shows `failed n of m` instead of a ratio. The verdict line gives the seconds first, then the ratio. A `hero-*` card repeats a headline verdict that is not inconclusive, its seconds above the ratio; no other case has one. Quick output is marked as smoke data; do not publish it. The ignored `bench/results/` copy is an archive, never a measured workspace.

`meta.json` records the machine, versions, seed, random orders, exact commands and package versions. Its `cases` list each case with its planned rounds, fetchloom's under `rounds` and every arm's under `arm_rounds`, before the case starts: the renderer reads a case's plan there and refuses a lab row of a case its own run does not list. `fetchloom_binary` records the supplied executable's resolved path, size and SHA-256; `repo_head` identifies the harness checkout, not a separately built product. Keep the supplied executable unchanged throughout a run. For a dispatcher executable the digest identifies the dispatcher; preserve the selected product binaries and their build provenance separately. Its inventories record missing native packages as `not installed` and a missing Python environment as `no python env`; neither stops a run that does not use them. `runs.jsonl` contains metrics, checker failures and exit statuses. Command logs retain errors; each row's `log` names its own, by a run id of one length. Every arm and tool runs in the same work folder, from its `project` folder, with its own cache at `cache/tool` there, so no arm's paths are longer than another's: past about 165 bytes, each path lookup costs more kernel time. s7 records both attempts and the second attempt's wall time. s7, s10 and s12 record `body_bytes_ratio`, server body bytes / dataset size: the body bytes of the server's 200 and 206 answers for the dataset's files, across every attempt, over its size. It says what a tool asked for again; nginx cannot see packets the network sent twice.

## Teardown

Copy wanted results first. Teardown removes the namespace, its nginx processes, hosts entry, CA, mounts, images, fixtures and every file under `FL_BENCH_ROOT`, including results.

```sh
python3 bench/lab.py teardown
python3 bench/lab.py setup
python3 bench/lab.py teardown
```

The second setup checks the lifecycle is repeatable. The package environment at `FL_BENCH_VENV` is not part of lab state and is kept for another run, as is the empty cgroup `/sys/fs/cgroup/fl-bench`, which `rmdir` removes.

## Add a client

Add one class in `tools.py`, register it in `CLASSES`, and implement `version`, `supports`, `prepare`, `command` and `outputs`. Put one-time native preparation in `fixtures.py`. Keep caches on the measured filesystem, do not time preparation, and return every required output with its expected SHA-256. Record all flags and use the client's documented workflow. Add focused tests in `test_bench.py`, then run the matching quick cases and render them before changing the design's comparison table.

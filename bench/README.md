# benchmarks

this suite measures time to checked files, cache reuse, no-op work and interrupted downloads. [the design](../docs/dev/performance/benchmark-design.md) defines the cases and fairness rules.

to compare two fetchloom builds on windows, macos or any other filesystem without this lab, use `native.py` ([native comparison](../docs/dev/performance/native.md)). it records wall time and command success, not independent output hashes; the [native checks](../docs/dev/performance/native-checks.md) add correctness and platform probes.

## requirements

use linux x86_64, python 3.12, root, network namespaces, loop devices, ext4 and xfs. the client and nginx share a kernel but use separate network namespaces. all measured files, caches and logs live under `FL_BENCH_ROOT` (default `/srv/fl-bench`), not a windows mount. leave at least 30 GiB free. do not run other benchmarks at the same time: dropping page caches affects the whole machine. before a drop and around each run, the lab writes out only its own filesystems (`syncfs` on `FL_BENCH_ROOT` and the two test mounts), so a hung mount elsewhere on the machine cannot stall a run the way a global `sync` would.

on Ubuntu 24.04, these are the native package pins used by the smoke test:

```sh
apt-get update
apt-get install -y \
  nginx=1.24.0-2ubuntu7.19 aria2=1.37.0+debian-1build3 \
  curl=8.5.0-2ubuntu10.16 \
  git-annex=10.20240129-1build1 git=1:2.43.0-1ubuntu7.4 \
  iproute2=6.1.0-1ubuntu6.4 xfsprogs=6.6.0-1ubuntu2.1 \
  e2fsprogs=1.47.0-2.4~exp1ubuntu4.1 unzip=6.0-28ubuntu4.1 \
  openssl=3.0.13-0ubuntu3.15 python3=3.12.3-0ubuntu2.1 \
  time=1.9-0.2build1
systemctl disable --now nginx
```

the lab starts its own nginx in a network namespace; disable the system service after installation. use an Ubuntu package snapshot if a pin has left the mirror. python clients share one environment, pinned in [pyproject.toml](pyproject.toml) and locked with every transitive version and hash in [uv.lock](uv.lock); each measured command has its own empty home and cache. the runner and the chart renderer import only the standard library. install [uv](https://docs.astral.sh/uv/getting-started/installation/) where root can run it. uv creates the environment without `python3-venv`; `FL_BENCH_VENV` selects its path (default `/opt/fl-bench/venv`).

## build the measured executable

from the repository root in a linux shell with the [build prerequisites](../docs/dev/engineering/building-and-testing.md#build-requirements), build as your normal user:

```sh
cargo build --locked --release --target-dir target
target/release/fetchloom --version
```

in WSL, open that shell from powershell at the repository root with `wsl --cd $PWD.Path`. build a linux executable there, not a windows executable from powershell. the source may be on a windows mount; measured state must stay on the linux filesystem.

## setup

use a root shell in the same checkout for setup and runs. from powershell at the repository root, `wsl --cd $PWD.Path --user root` opens one in the default WSL distribution. on native linux, enter the same checkout after switching to your root shell. set the binary and lab paths in that shell. the two paths are canonicalized at startup, so a directory symlink names the same lab for setup, output checks and teardown. keep both `FL_BENCH_` variables set for setup, tests, runs and teardown; with sudo, pass them explicitly through `sudo env`:

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

set `FL_BENCH_LAB_CHECKS=1` on the test command, inside a set-up lab, to check that switching network profiles leaves no earlier rate cap behind and that each capped link holds its queue limit. set `FL_BENCH_LIVE_CHECKS=1` on the test command, inside a set-up lab, to also check that the hugging face and kaggle wrappers reject deliberately wrong expected hashes, using tiny public files. `FL_BENCH_FETCHLOOM` names the binary they use, `/home/fl/bin/fetchloom` by default. CI runs the other checks on every pull request.

setup creates `flb-srv`, `flb-client` and `flb-server`, adds `lab.flb` and `store.flb` to `/etc/hosts` on one line marked `# fl-bench`, installs a disposable CA for both names, starts nginx and mounts two 12 GiB sparse images. the server answers each path under `https://lab.flb/r/` with a 302 to the same path on `store.flb`, as a repository sends its downloads to a storage host. setup replaces only a lab with its ownership marker. it refuses conflicting unowned resources. setup also checks that HTTPS answers on both names, through the redirect.

## run

full lab and live runs. ext4 runs every scenario on every link it applies to. xfs adds the two scenarios where reflinks change the result, the second project and the no-op:

```sh
python3 bench/run.py --fetchloom "$benchmark_binary" \
  --profiles lan,home,poor --scenarios s1,s2r,s3,s4,s5,s6,s7,s8,s9,s10 \
  --variant aa:FETCHLOOM_EXP_NONE=1 \
  --fs ext4 --seed 1729 --out "$FL_BENCH_ROOT/results/full/lab-ext4"
python3 bench/run.py --fetchloom "$benchmark_binary" \
  --profiles lan --scenarios s5,s6 --variant aa:FETCHLOOM_EXP_NONE=1 \
  --fs xfs --seed 1729 --out "$FL_BENCH_ROOT/results/full/lab-xfs"
python3 bench/live.py --fetchloom "$benchmark_binary" \
  --seed 1729 --out "$FL_BENCH_ROOT/results/full/live"
```

the a/a arm, `--variant aa:FETCHLOOM_EXP_NONE=1`, runs fetchloom again under a setting it never reads; on lan and home its interval decides whether a case may have a verdict, so a comparison includes it. each headline case is judged against one tool frozen from the timing pilot before the confirmatory runs: `python3 bench/render.py --comparators "$FL_BENCH_ROOT/results/pilot" > comparators.json` picks, for each headline case the pilot measured, the fastest other tool that passed every run within the budget, and `--comparators comparators.json`, given to every run of the campaign, records them in `meta.json`. a campaign refuses a comparator it does not measure, and the renderer takes a case's comparator only from the `meta.json` of the campaign that measured it, so one recorded by a later chunk never judges it. there are 10 measured rounds on every link, plus one warmup per case. `--rounds N` overrides that count. the runner prints a planning estimate before fixture preparation, assuming a minute per run, which overstates lan cases; the estimate is for planning, not a promised completion time.

smoke tests use 12 files per set, csv tables of 64 KiB for s8, a 2 MiB large file and one measured round. s7 and s10 keep their link's whole large file, 256 MiB at home and 64 MiB on the poor link, so it is still downloading when SIGINT arrives or the link drops at 10 s.

```sh
python3 bench/run.py --fetchloom "$benchmark_binary" --quick \
  --profiles lan,home,poor --fs ext4 --out "$FL_BENCH_ROOT/results/quick/lab"
python3 bench/live.py --fetchloom "$benchmark_binary" --quick \
  --out "$FL_BENCH_ROOT/results/quick/live"
```

- every invocation needs a new output directory. a campaign may run in parts, one `--profiles`/`--scenarios` slice per `--out` folder under one results root; each case draws its own seeded order, and the renderer combines the parts as long as no case is measured twice.
- `--only-tools fetchloom,curl` narrows the lab clients and `--services zenodo,huggingface` the live services. `curl+sync` is curl followed by a flush of what it wrote, to compare with fetchloom's durable writes, in the cases that write data: not the no-op cases s6 and s9. a fetchloom-only run needs no other client or python environment; only selected clients get native preparation and version checks.
- unsupported combinations are left out: s1, s2r and s7 run at home and on the poor link, s3 at home and on lan, s4, s8 and s10 at home, and s5, s6 and s9 on lan.
- each measured command has a 20-minute timeout by default. `--timeout` must be finite and positive, and invalid settings are refused before a client starts. interruption and timeout deadlines include process startup, and a timer that fails to start kills and reaps the started group. pooch's interruption and s10's drop count from its first call instead, which its script marks in `calls.begin` on the machine's monotonic clock just before the calls; a run that has not marked it by the deadline timed out. s7's first attempt keeps `anchor_seconds` and s10's `link_drop` keeps `anchor_at`, how far into the command the calls began.
- a failed command, timeout or wrong hash is a failed record, not a short timing.
- in s10 the home link drops 10 s into s1's download, for 10 s: `tc qdisc change` sets netem's loss to 100% at both ends, so new packets are lost while those already queued still arrive, and nothing is stopped. each s10 run starts on the whole profile again. a run that finishes before the link comes back failed, since it measured no drop. the row keeps `link_drop`, and the renderer sets each tool's time against its own s1 on the same link.
- in s2r each file's link goes through the redirect, for every tool: curl gets `-L` for each transfer, and DVC's remote and datalad's registered links go through it too. a run whose server log does not show both host names failed.
- every tool keeps to 4 requests in flight to one host by its own flags: fetchloom's `FETCHLOOM_HOST_JOBS=4`, curl `--parallel-max 4`, aria2c `-j 4 -x 4`, DVC `-j 4` and datalad `-J 4`; pooch fetches one file at a time. nginx logs each request's host, protocol, connection, its number on that connection, how long it took and when it ended. each row records the connections, the most requests on one connection and, per host, the most requests and connections in flight at once, HTTP/2 streams included. a request counts as in flight from its arrival until nginx logged it, and for at least the link's round trip, less 2 ms for the log's millisecond clock, so on lan a request answered within 2 ms does not count. the lan figure is therefore an estimate with a 2 ms floor: each row records the floor as `in_flight_floor_ms`, 0 where every request counts, and the results mark the column as estimated and state the floor under each such table. a run over the budget has `over_budget` set and stays a passed run, with its measurements in the results, but no comparison it is in gets a verdict, since the two tools were not equally polite; when fetchloom's own run is over, its case gets none.

to compare a setting the measured build accepts, add `--variant label:FETCHLOOM_X=1`, once per variant. each variant syncs the same prepared project with its own environment, shuffled into the same rounds as the plain build; the runner tests cover twelve variants plus the base, and there is no limit. the case seed gives the same order for the same selected names, and each row records its variant settings; adding variants changes the shuffle and can lengthen the gap between paired runs. `python3 bench/ab.py <out>` reports wall, cpu, memory, disk, server bytes and requests from matched successful rounds: the median log ratio `ln(variant) - ln(plain)`, its exponentiated ratio, a paired percentile bootstrap 95% interval, wins and metric-specific pair counts. a metric enters a pair only when both sides have finite values; zero and negative pairs keep their win and tie counts but have no log ratio. in the example, `FETCHLOOM_SPLIT=1` asks eligible large downloads to split from the first answer.

```sh
python3 bench/run.py --fetchloom "$benchmark_binary" --only-tools fetchloom --profiles home \
  --scenarios s1 --fs ext4 --rounds 20 --variant split:FETCHLOOM_SPLIT=1 \
  --out "$FL_BENCH_ROOT/results/ab/split"
python3 bench/ab.py "$FL_BENCH_ROOT/results/ab/split" --threshold .03 \
  --json "$FL_BENCH_ROOT/results/ab/split/comparison.json"
```

the bootstrap uses 20,000 resamples and seed 1729 by default; `--resamples` and `--seed` change those analysis settings. seeds are separate per case, variant and metric, so adding a variant does not change another interval. a win requires the entire interval below `1 - threshold`, at least 10 positive pairs (`--min-pairs`), and no failed, unmatched or excluded metric pairs. `--threshold .03` means an upper bound strictly below 0.97. increase `--rounds` to the noise-floor recommendation; bootstrap draws do not replace measured rounds. a quick run cannot establish a win. duplicate round identities and mixed workload sizes, quick modes or CPU masks are rejected; unmatched round numbers remain explicit in the results. intervals are per comparison, without a multiple-comparison correction; confirm selected winners independently.

pooch runs as a notebook uses it: its script imports pooch, then times its `retrieve` calls itself, wall time and the process's CPU around them, and writes them with the import time to `calls.json` before it exits, even when stopped. those times replace the process's, which the row keeps under `process`, and `clock` says which a row's times cover. s5 fills that client's cache from s2r before each run. s6 runs the same s2r workflow once before timing, and s9 the s3 workflow. DVC uses one shared cache directory and pooch returns its shared cached paths. datalad's clone, of the prepared dataset or for s5 of the first project, is made before the clock starts, like DVC's project copy. cache population is outside the clock. prepared download inputs, DVC repositories and URL-registered annex datasets are built once, before the runs. fixture manifests record their dimensions, seed and generator digest; preparation markers record fixture inputs, harness/lock digests and the fetchloom executable digest where applicable. a mismatch or incomplete marker rebuilds the owned preparation outside the clock.

live runs check a separately downloaded reference against the public manifest, then compare the clients' file SHA-256s and layouts. metadata is atomically saved and flushed before each measured command, so interrupted campaigns retain their inputs, orders and command wrappers. preparation and checker exceptions become failed rows rather than disappearing from the record. quick inputs are zenodo's iris record, one hugging face iris CSV, kaggle's iris dataset, the two sub-KiB figshare files and the sub-KiB dataverse file. inaccessible services and anonymous kaggle refusals have recorded skip reasons. the hugging face and kaggle commands include the expected SHA-256 check because their download APIs have no such option. a full campaign runs 10 rounds per service and a quick one runs 1. the first round's order is seeded and each after it is the other way round (ABBA). the results give the other tool's time over fetchloom's, rounds paired, with a 95% interval and no verdict, since it holds for that campaign alone.

datahugger uses an explicit dataverse landing URL and an identified User-Agent because its DOI parser and the default Python User-Agent are rejected there. its generated checksum reports are excluded from dataset comparisons, but still count toward time and disk.

the full dataverse TSV/TAB representation difference is reported as a content mismatch; the quick shared file has identical bytes.

client processes do not inherit service credentials, native client configuration overrides, Git overrides or Python import/cache-location overrides. Git reads neither system nor global configuration; repository-local settings still apply. curl starts with `-q` to disable curlrc. fetchloom refuses a private address that a project names unless `allowed_origins` lists its origin, so every lab command gets `FETCHLOOM_ALLOWED_ORIGINS` naming the lab server's. these controls and the remaining flags are recorded. continue to use a clean dedicated environment; proxy settings for public live services remain intentional inputs.

## measurement island

on linux, set `FL_BENCH_CPUS=0-7` or pass `--cpus 0-7` to `run.py` or `live.py`. the runner validates the list against its available CPUs, pins nginx's master and worker threads to it, and gives every measured command the mask before it can execute. children inherit it. `lab.py setup --cpus 0-7` also starts nginx on that mask. every run records the effective command and nginx masks. an unset or empty setting uses the runner's available CPUs, including when resetting a server left pinned by an earlier run.

CPU affinity does not isolate shared cache, memory bandwidth, filesystem flushes or disk traffic. run benchmarks alone on the machine until a controlled check shows that builds on other cores leave the noise where it was. do not claim a quieter island from unit tests alone.

`island.py check` is an operator-only measurement. it runs full-size A/A campaigns first with a quiet island, then with a repeated release build on disjoint cores. `--case filesystem/profile/scenario` is repeatable and selects exactly those cases; the default is ext4/lan s3, s5 and s6. use the unchanged executable from the quiet noise campaign. it compares both paired noise distributions with `noise.json` and reports the loaded/quiet-island wall shift. default acceptance requires 20 complete pairs per case, A/A 95% intervals within 0.97 to 1.03, robust log spread no more than twice the old spread (with a 0.01 log-scale floor), and a central loaded/quiet-island shift within 3%. the loaded interval width must stay within twice the larger quiet/historical width (with a 1-percentage-point floor), and the A/A median bias may shift by at most 1 percentage point. the width and bias floors are one third of `--effect`. these are explicit operational checks, not a proof of independence or an 80% power guarantee.

run the check with nothing else on the machine: it creates its own compiler load on the build CPUs. give it a new target directory used by no other process, because the load worker repeatedly cleans fetchloom in that target before rebuilding. load failure aborts the measurement, and the worker group is stopped on exit. source is read without modification. `--noise` names the `ab.py --json` output of an earlier quiet A/A campaign on the same machine and lab, with the same cases, measured like this:

```sh
python3 bench/run.py --fetchloom "$benchmark_binary" --only-tools fetchloom \
  --profiles lan --scenarios s3,s5,s6 --fs ext4 --rounds 20 \
  --variant aa:FETCHLOOM_EXP_NONE=1 --cpus 0-7 --out "$FL_BENCH_ROOT/results/noise"
python3 bench/ab.py "$FL_BENCH_ROOT/results/noise" --json "$FL_BENCH_ROOT/results/noise.json"
```

then, as root, after sourcing the Rust toolchain environment:

```sh
sudo env PATH="$PATH" \
  FL_BENCH_ROOT="$FL_BENCH_ROOT" FL_BENCH_VENV="$FL_BENCH_VENV" \
  python3 -B bench/island.py check --fetchloom "$benchmark_binary" \
  --cpus 0-7 --build-cpus 8-23 --noise "$FL_BENCH_ROOT/results/noise.json" \
  --worktree "$PWD" --target "$FL_BENCH_ROOT/island-target" \
  --out "$FL_BENCH_ROOT/results/island-new"
```

keep `island.json`, both campaigns and `build.log`. the target directory is left for inspection. `--effect`, `--noise-multiple`, `--rounds` and `--jobs` state the acceptance and load settings; choose them before measuring. a check failure means builds should remain excluded, not that the benchmark should discard its slower observations.

## render and keep results

```sh
python3 bench/render.py "$FL_BENCH_ROOT/results/full"
mkdir -p bench/results
cp -a "$FL_BENCH_ROOT/results/full" bench/results/
```

rendering requires a matching `meta.json` beside every `runs.jsonl`. it rejects repeated live-service campaigns, inconsistent dataset sizes and reference hashes, and mixed quick/full inputs before deleting any old chart. split service campaigns may be combined only when each service occurs once. rendering writes `docs/dev/performance/benchmarks.md` and one SVG card per case in `docs/assets/bench/`, each with a `-dark.svg` twin for github's dark theme. it first removes every SVG card already in that folder, so no card, including a `hero-*` card, outlives the results that produced it. the page leads with the links most people have, home broadband and a poor connection, then the local network, and names each in plain words with its recorded shaping, such as `home broadband, about 100 Mbit and 20 ms`. each case's table sets every other tool's time over fetchloom's, round by round, with `ab.py`'s paired percentile bootstrap: a 99.2% interval in the six headline cases (s1, s2r and s7 at home, s2r on the poor link, and s5 and s10 on ext4), which share a 5% error, and 95% in the rest, which describe and claim nothing. only a headline case's frozen comparator gets a verdict, never a tool chosen from the same data, and a headline case with none frozen gets none at all; every other row is marked descriptive, and a case outside the headline set only describes its fastest other tool that passed every run. a verdict needs at least 10 paired rounds: fetchloom faster when the interval's lower end is above 1.05, the other tool faster when its upper end is below 0.95, even when the whole interval sits inside 0.95 to 1.05, and inconclusive otherwise. the a/a arm, `--variant aa:FETCHLOOM_EXP_NONE=1`, is never a rival; on lan and home it must pass every run, in the same rounds as fetchloom, with at least 10 pairs, and its 95% interval must sit inside 0.97 to 1.03, or the case gets no verdict, and the table withholds every tool's verdict in it. a case where fetchloom failed a run gets none either, nor does one where fetchloom did not run exactly the rounds its campaign's `meta.json` planned, so a campaign cut short publishes no verdicts, and a tool with a failed run shows `failed n of m` instead of a ratio. the verdict line gives the seconds first, then the ratio. a `hero-*` card repeats a headline verdict that is not inconclusive, its seconds above the ratio; no other case has one. quick output is marked as smoke data; do not publish it. the ignored `bench/results/` copy is an archive, never a measured workspace.

`meta.json` records the machine, versions, seed, random orders, exact commands and package versions. its `cases` list each case with its planned rounds before the case starts: the renderer reads a case's plan there and refuses a lab row of a case its own campaign does not list. `fetchloom_binary` records the supplied executable's resolved path, size and SHA-256; `repo_head` identifies the harness checkout, not a separately built product. keep the supplied executable unchanged throughout a campaign. for a dispatcher executable the digest identifies the dispatcher; preserve the selected product binaries and their build provenance separately. its inventories record missing native packages as `not installed` and a missing python environment as `no python env`; neither stops a run that does not use them. `runs.jsonl` contains metrics, checker failures and exit statuses. command logs retain errors; each row's `log` names its own, by a run id of one length. every arm and tool runs in the same work folder, from its `project` folder, with its own cache at `cache/tool` there, so no arm's paths are longer than another's: past about 165 bytes, each path lookup costs more kernel time. s7 records both attempts and the second attempt's wall time. s7 and s10 record `body_bytes_ratio`, server body bytes / dataset size: the body bytes of the server's 200 and 206 answers for the file, across every attempt, over its size. it says what a tool asked for again; nginx cannot see packets the network sent twice.

## teardown

copy wanted results first. teardown removes the namespace, its nginx processes, hosts entry, CA, mounts, images, fixtures and every file under `FL_BENCH_ROOT`, including results.

```sh
python3 bench/lab.py teardown
python3 bench/lab.py setup
python3 bench/lab.py teardown
```

the second setup checks the lifecycle is repeatable. the package environment at `FL_BENCH_VENV` is not part of lab state and is kept for another run.

## add a client

add one class in `tools.py`, register it in `CLASSES`, and implement `version`, `supports`, `prepare`, `command` and `outputs`. put one-time native preparation in `fixtures.py`. keep caches on the measured filesystem, do not time preparation, and return every required output with its expected SHA-256. record all flags and use the client's documented workflow. add focused tests in `test_bench.py`, then run the matching quick cases and render them before changing the design's comparison table.

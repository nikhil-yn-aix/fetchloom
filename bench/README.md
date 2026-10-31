# benchmarks

this suite measures time to checked files, cache reuse, no-op work and interrupted downloads. [the design](../docs/dev/performance/benchmark-design.md) defines the cases and fairness rules.

## requirements

use linux x86_64, python 3.12, root, network namespaces, loop devices, ext4 and xfs. the client and nginx share a kernel but use separate network namespaces. all measured files, caches and logs live under `/srv/fl-bench`, not a windows mount. leave at least 30 GiB free. do not run other benchmarks at the same time: dropping page caches affects the whole machine.

on Ubuntu 24.04, these are the native package pins used by the smoke test:

```sh
apt-get update
apt-get install -y \
  nginx=1.24.0-2ubuntu7.19 aria2=1.37.0+debian-1build3 \
  wget=1.21.4-1ubuntu4.5 curl=8.5.0-2ubuntu10.16 \
  git-annex=10.20240129-1build1 git=1:2.43.0-1ubuntu7.4 \
  iproute2=6.1.0-1ubuntu6.4 xfsprogs=6.6.0-1ubuntu2.1 \
  e2fsprogs=1.47.0-2.4~exp1ubuntu4.1 unzip=6.0-28ubuntu4.1 \
  openssl=3.0.13-0ubuntu3.15 python3=3.12.3-0ubuntu2.1 python3-venv
```

use an Ubuntu package snapshot if a pin has left the mirror. python clients share the pinned environment below; each measured command has its own empty home and cache. `pathspec` is pinned for DVC's gitignore parser, and `chardet` for requests compatibility. DVC includes HTTP support without an extra. the runner itself imports only the standard library; matplotlib is used only by the renderer.

## setup

from the repository root, as root:

```sh
python3 bench/lab.py setup
export REQUESTS_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt
export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
export TMPDIR=/srv/fl-bench/scratch
python3 -m venv /opt/fl-bench/venv
/opt/fl-bench/venv/bin/pip install --no-cache-dir -r bench/requirements.txt
/opt/fl-bench/venv/bin/pip check
/opt/fl-bench/venv/bin/python -m unittest discover -s bench -p 'test_*.py'
```

set `FL_BENCH_LIVE_CHECKS=1` on the test command to also check that the hugging face and kaggle wrappers reject deliberately wrong expected hashes, using tiny public files.

setup creates `flb-srv`, `flb-client` and `flb-server`, adds `lab.flb` to `/etc/hosts`, installs a disposable CA, starts nginx and mounts two 12 GiB sparse images. setup replaces only a lab with its ownership marker. it refuses conflicting unowned resources. setup also checks that HTTPS is responsive.

in WSL, enter the existing worktree with:

```powershell
wsl -d Ubuntu-24.04 -u root -- bash -lc 'cd /mnt/c/Users/t-nikhilyn/AppData/Local/fetchloom-work/wt/bench; exec bash'
```

run the shell blocks above and below there. the source may be on `/mnt/c`; the measured state must not be.

## run

full lab and live runs. ext4 runs every scenario on every profile it applies to. xfs adds the two scenarios where reflinks change the result, the second project and the no-op:

```sh
python3 bench/run.py --fetchloom /home/fl/bin/fetchloom \
  --profiles lan,home,far,poor --scenarios s1,s2,s3,s4,s5,s6,s7 \
  --fs ext4 --seed 1729 --out /srv/fl-bench/results/full/lab-ext4
python3 bench/run.py --fetchloom /home/fl/bin/fetchloom \
  --profiles lan --scenarios s5,s6 \
  --fs xfs --seed 1729 --out /srv/fl-bench/results/full/lab-xfs
python3 bench/live.py --fetchloom /home/fl/bin/fetchloom \
  --seed 1729 --out /srv/fl-bench/results/full/live
```

there are 10 measured rounds on lan and 5 on shaped links, plus one warmup per case. `--rounds N` overrides that count. the runner prints a planning estimate before fixture preparation, assuming a minute per run, which overstates lan cases; the two lab commands above take about five hours on an 8-thread laptop.

smoke tests use 12 files per set, a 2 MiB large file and one measured round. s7 keeps the 256 MiB home file so it is still downloading when SIGINT arrives at 10 s.

```sh
python3 bench/run.py --fetchloom /home/fl/bin/fetchloom --quick \
  --profiles lan,home --fs ext4 --out /srv/fl-bench/results/quick/lab
python3 bench/live.py --fetchloom /home/fl/bin/fetchloom --quick \
  --out /srv/fl-bench/results/quick/live
```

`--only-tools fetchloom,curl` narrows the lab adapters. `--services zenodo,huggingface` narrows live services. a new output directory is required for every invocation. a failed command, timeout or wrong hash is a failed record, not a short timing. unsupported combinations are omitted: s3 to s6 use lan, and s7 uses home. a default 20-minute timeout applies to each measured command.

s5 fills that client's cache from s2 before each run. s6 runs the same s2 workflow once before timing. DVC uses one shared cache directory; datalad's second project clones the first project; pooch returns its shared cached paths. those setup operations are outside the clock. prepared download inputs, DVC repositories and URL-registered annex datasets are built once, before the runs.

live runs check a separately downloaded reference against the public manifest, then compare the clients' file SHA-256s and layouts. quick inputs are zenodo's iris record, one hugging face iris CSV, kaggle's iris dataset, the two sub-KiB figshare files and the sub-KiB dataverse file. inaccessible services and anonymous kaggle refusals have recorded skip reasons. the hugging face and kaggle commands include the expected SHA-256 check because their download APIs have no such option. live results have no speed ratios.

datahugger uses an explicit dataverse landing URL and an identified User-Agent because its DOI parser and the default Python User-Agent are rejected there. its generated checksum reports are excluded from dataset comparisons, but still count toward time and disk.

the full dataverse TSV/TAB representation difference is reported as a content mismatch; the quick shared file has identical bytes.

## render and keep results

```sh
/opt/fl-bench/venv/bin/python bench/render.py /srv/fl-bench/results/full
mkdir -p bench/results
cp -a /srv/fl-bench/results/full bench/results/
```

rendering writes `docs/dev/performance/benchmarks.md` and transparent SVGs in `docs/assets/bench/`. every chart has a `-dark.svg` variant. `hero-ext4.svg` and `hero-xfs.svg` show lan ratios for a README. quick output is marked as smoke data; do not publish it. the ignored `bench/results/` copy is an archive, never a measured workspace.

`meta.json` records the machine, binary and repository versions, seed, random orders, exact commands and package versions. `runs.jsonl` contains metrics, checker failures and exit statuses. command logs retain errors. s7 records both attempts, file-body bytes across them and the second attempt's wall time.

## teardown

copy wanted results first. teardown removes the namespace, its nginx processes, hosts entry, CA, mounts, images, fixtures and every file under `/srv/fl-bench`, including results.

```sh
python3 bench/lab.py teardown
python3 bench/lab.py setup
python3 bench/lab.py teardown
```

the second setup checks the lifecycle is repeatable. the package environment under `/opt/fl-bench/venv` is not part of lab state and is kept for another run.

## add a client

add one class in `tools.py`, register it in `CLASSES`, and implement `version`, `supports`, `prepare`, `command` and `outputs`. put one-time native preparation in `fixtures.py`. keep caches on the measured filesystem, do not time preparation, and return every required output with its expected SHA-256. record all flags and use the client's documented workflow. add focused tests in `test_bench.py`, then run the matching quick cases and render them before changing the design's comparison table.

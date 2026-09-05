# spike

Throwaway measurement workspace for the nine questions in the pre-phase-6 planning spike.
Nothing here ships. Nothing here is production code, and the main workspace's lint rules,
`deny.toml` allow-list and `unused_crate_dependencies` do not apply.

## Deleting it

Everything this spike wrote, downloaded or measured lives under `spike/`.

```
rm -rf D:/Desktop/Research/fetchloom/spike
```

Then remove the one line it added outside itself: `exclude = ["spike"]` in the root
`Cargo.toml`. That line is the only change this spike made to the repository. It exists so
`spike/` cannot join the main workspace and cannot perturb its lockfile or lints.

The spike also writes to a cache directory of its own, `spike/work/cache`, set through
`FETCHLOOM_CACHE_DIR`. It never touches the user's real cache at
`%LOCALAPPDATA%\Fetchloom\Cache`, which on this machine was written by a different build and
would have required a `cache clear` this spike is not entitled to perform.

## Layout

```
bin/          shell and python drivers
src/bin/      the local measurement binaries (experiments 1, 2, 3, 8)
http1/        ureq HTTP/1.1 client, separate workspace, own lockfile
http2/        reqwest HTTP/2 client, separate workspace, own lockfile
http3/        reqwest HTTP/3 client, separate workspace, own lockfile
ftp/          suppaftp + russh-sftp probes, separate workspace, own lockfile
python/       the fetchloom:// fsspec adapter and its harness
corpus/       the corpus, and corpus/manifest.json describing it
data/         every raw measurement, as JSON
work/         scratch destinations, member caches, spike-local fetchloom cache
```

The four network workspaces are separate workspaces on purpose: each carries its own
`Cargo.lock`, which is what makes "crates in the lock" a measurement rather than a guess.

## Rerunning everything

Prerequisites: Rust 1.98, Python 3.12 (this machine has no `python3` on `PATH` under Git
Bash; the interpreter is `C:\Users\Naveen\AppData\Local\Programs\Python\Python312\python.exe`
and `py -3` also works), `curl`, `tar`, `gzip`, `unzip`. There is no `zstd` or `7z` CLI on
this machine, so every zstd operation happens in Rust.

### 1. Corpus

```
bash bin/fetch_corpus.sh                      # ~2.6 GiB of downloads
py -3 bin/fetch_photos.py                     # real photographs from Wikimedia Commons
bash bin/build_corpus.sh                      # unpack, gunzip, build the object tars
py -3 bin/manifest.py                         # writes corpus/manifest.json
```

`bin/manifest.py` prints the corpus inventory and is the authority for what every other
experiment measures. `corpus/photos/_provenance.json` records the URL of every photograph.

### 2. Local experiments (1, 2, 3, 8)

```
cargo build --release
bash bin/run_local.sh                         # experiments 1 and 2, sequential
./target/release/exp3_layout.exe              # experiment 3
./target/release/exp8_hash.exe                # experiment 8, hardware SHA-256
RUSTFLAGS='--cfg sha2_backend="soft"' \
  cargo build --release --bin exp8_hash --target-dir target-soft
./target-soft/release/exp8_hash.exe           # experiment 8, software SHA-256
```

Experiment 1 takes a sub-command: `levels`, `probe`, `frames`, or `all`.

Record machine conditions beside any timing:

```
powershell -ExecutionPolicy Bypass -File bin/machine_state.ps1
```

### 3. Concurrency and splitting (4)

`bin/exp4_sweep.sh` touches real third-party servers. It bounds the request count per host and
drops a host as soon as it returns anything that is not 200 or 206.

```
cargo build --release --manifest-path http1/Cargo.toml
cargo build --release --bin faultserver
RUNS=7 bash bin/exp4_sweep.sh                 # real hosts: per-host and split sweeps
RUNS=7 bash bin/exp4_local.sh                 # local fault server: global ceiling and control
```

Both append to `data/exp4_raw.jsonl`; `py -3 bin/exp4_tables.py all` renders every table.
`bin/exp4_local.sh` touches no third-party server, which is why the global ceiling is swept
there: realising a global concurrency of 64 across three reachable hosts would mean 21 streams
per host, which is the hammering the experiment exists to avoid.

Do not raise `RUNS` or the per-host range counts without a reason. The politeness limit is
the experiment, not an obstacle to it.

`bin/net_sweep.sh` and `bin/alpn_probe.sh` were written for experiment 5 and are kept, but
experiment 5's throughput sweep was cancelled; see RESULTS.md.

### 4. FTP and SFTP (6)

```
cargo build --release --manifest-path ftp/Cargo.toml
./ftp/target/release/ftpspike.exe > data/exp6_ftp_sftp.json
```

Hits `ftp.gnu.org`, `ftp.ensembl.org`, `test.rebex.net` and `demo.wftpserver.com`, all of
which publish themselves as public or demo servers.

### 5. Python integration (7)

```
py -3 -m venv .venv
./.venv/Scripts/python.exe -m pip install fsspec pandas pyarrow
./.venv/Scripts/python.exe python/exp7_fsspec.py
```

`FETCHLOOM_BIN` and `FETCHLOOM_CACHE_DIR` override the binary and the cache the adapter uses.

### 6. Hint instrumentation (9)

```
bash bin/exp9_hint.sh
```

Drives the real release binary against a spike-local cache and counts how often a hint is
actually printed.

## Where the numbers live

`data/` holds one JSON or JSONL file per experiment, and every table in `RESULTS.md` is
derived from one of them. `data/machine_*.json` records the machine conditions a timing was
taken under. Timings taken under different conditions are not comparable and are not put in
the same table.

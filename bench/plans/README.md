# benchmark plans

the plans the harness runs for the method in [benchmarks](../../docs/benchmarks.md). the tools and their versions are in [tools](../tools.md); the wrappers that give a tool more than one command are in `bench/tools/`, one python file for both systems, run with each system's bench venv.

## lab

| plan | profile | workloads |
|---|---|---|
| `lab-lan-windows.toml`, `lab-lan-linux.toml` | `lan` | file-1g, file-100m, file-10m, files-10k-zip, files-10k-tar-zst |
| `lab-lan-windows-urls.toml`, `lab-lan-linux-urls.toml` | `lan` | many-1k |
| `lab-home-windows.toml`, `lab-home-linux.toml` | `home` | file-1g, file-100m, file-10m, files-10k-zip, files-10k-tar-zst |
| `lab-home-windows-urls.toml`, `lab-home-linux-urls.toml` | `home` | many-1k |
| `lab-far-windows.toml`, `lab-far-linux.toml` | `far` | file-10m |
| `lab-far-windows-urls.toml`, `lab-far-linux-urls.toml` | `far` | many-1k |
| `lab-poor-windows.toml`, `lab-poor-linux.toml` | `poor` | file-10m |

the 1,000 urls have plans of their own because the harness pairs every tool with the one named `fetchloom`, and fetchloom asks for many urls as a project, not with `get`. `fast` is not run; the method's changes say why.

| workload | tools | primary | floor |
|---|---|---|---|
| file-1g, file-100m | fetchloom `get`, curl-sha256, aria2c, aria2c-tuned, pooch, dvc, datalad on linux | yes | curl-sha256 |
| file-10m | the same | no | curl-sha256 |
| files-10k-zip | fetchloom `get`, curl-bsdtar, pooch with `Unzip` | yes | curl-bsdtar |
| files-10k-tar-zst | fetchloom `get`, curl-bsdtar | no | curl-bsdtar |
| many-1k | fetchloom, curl-parallel, aria2c-j16 | yes | |

- fetchloom: `get` into the run's folder, its cache in the run's home. for many-1k, `many_urls.py` writes a project whose `data.toml` lists one url dataset per file and whose `data.lock` pins each sha256 and size, as a committed project would, then runs `fetchloom sync`.
- curl-sha256: `curl_sha256.py`, curl then a sha256 of the file against the expected one.
- aria2c: `aria2c --no-conf` with its defaults; aria2c-tuned adds `-x 16 -s 16 -k 1M`.
- pooch: `pooch_fetch.py`, `pooch.retrieve` with the known sha256, and `pooch.Unzip` for the zip. its cache is in the run's home; the files are hardlinked into the run's folder, since pooch's users read them in its cache.
- dvc: `dvc_import.py`, `dvc init --no-scm`, its cache in the run's home, then `import-url`. the second project gets the first's `.dvc` file and runs `dvc checkout` on the same cache.
- datalad, linux only: `datalad_download.py`, `datalad create` and `download-url`, which hands the file to git-annex.
- curl-bsdtar: `curl_bsdtar.py`, curl then bsdtar, which is `tar.exe` on windows.
- curl-parallel and aria2c-j16: `many_urls.py` writes a curl config of 1,000 url and output pairs for `curl --parallel -K`, or an aria2c input file for `aria2c -i -j 16`. fetchloom's project comes from the same wrapper, so every tool pays the same start and list cost.

fetchloom, pooch and dvc keep a cache, so each also runs a second project on that cache. pooch cannot unpack the tar.zst: `pooch.Untar` reads with python's tarfile, which has no zstd before python 3.14, and the bench venv is python 3.13. git-annex on windows is limited, so the method runs datalad on linux only.

`bench/expect/` holds the sha256 of every file a correct run leaves, from the fixtures themselves: one line for each single file, 10,000 for the unpacked archives and 1,000 for many-1k.

## run the lab

start the lab server and trust its CA as in [the lab](../lab/README.md). the windows plans reach it at `https://172.31.233.169:8443`, the address WSL's eth0 had when they were written; `wsl -d Ubuntu-24.04 -- ip -4 -br addr show eth0` shows the current one, and a restart of WSL can change it, which needs `lab` in the windows plans' `[vars]` updated. the linux plans use `https://localhost:8443`.

one profile at a time, both systems, never both at once. from powershell in the repository:

```powershell
wsl -d Ubuntu-24.04 -u root -e bash bench/lab/net.sh home
uv run --no-config --no-index --find-links $env:LOCALAPPDATA\fetchloom-work\wheels --python 3.13 bench/harness.py bench/plans/lab-home-windows.toml
uv run --no-config --no-index --find-links $env:LOCALAPPDATA\fetchloom-work\wheels --python 3.13 bench/harness.py bench/plans/lab-home-windows-urls.toml
```

then in WSL as `fl`, in the repository at `/mnt/c/Users/t-nikhilyn/AppData/Local/fetchloom-work/wt/report`:

```sh
export PATH=$HOME/.local/bin:$PATH
/home/fl/bench-venv/bin/python bench/harness.py bench/plans/lab-home-linux.toml
/home/fl/bench-venv/bin/python bench/harness.py bench/plans/lab-home-linux-urls.toml
```

`$HOME/.local/bin` holds the linux fetchloom. each harness run writes one folder under `bench/results/` with every block of its plan.

`run-lab.ps1` does all of it unattended: for each profile it shapes the lab, runs the windows plans, then the linux plans through WSL, and clears the shaping at the end. it stops first if WSL's eth0 has another address than the windows plans name. the lab is one server and one link, so nothing else may use it during a block, other people's runs included.

## estimated time

for 5 blocks: each run's time in the `lan` dry runs of 2026-10-30, plus, for a first fetch, the profile's transfer time: the bytes at the one-stream rate measured in the lab's README (93 Mbit/s at `home`, 2.1 at `far`, 0.6 at `poor`), capped by the link for tools with several streams (aria2c-tuned 16, curl-parallel 50, aria2c-j16 16, fetchloom's 8 jobs for many-1k), plus a round trip per request on each stream. second projects take their `lan` time. the windows dry run of the archives did not finish within the time set for this work, so four of its times are assumed: fetchloom 55 s to fetch and unpack, 10 s for its second project, curl-bsdtar 12 s and pooch's second project 20 s.

| plans | windows, min | linux, min |
|---|---|---|
| `lab-lan-<system>` | 34 | 12 |
| `lab-lan-<system>-urls` | 12 | 4 |
| `lab-home-<system>` | 86 | 72 |
| `lab-home-<system>-urls` | 15 | 6 |
| `lab-far-<system>` | 20 | 22 |
| `lab-far-<system>-urls` | 25 | 16 |
| `lab-poor-<system>` | 63 | 73 |
| total | 4.2 h | 3.4 h |

`poor` is the largest part: each one-stream tool takes about 140 s for 10 MiB. on windows the timed runs pass the 4 h aim by about 12 minutes.

the harness adds time the table leaves out: after each run it hashes every file the run left and reads each file's id for the disk count. on linux that takes seconds. on windows, defender scans a new file the first time it is opened, and after one run that left 10,000 files the harness took about 12 minutes. `lab-lan-windows` and `lab-home-windows` have 8 such runs per block, so they would take about 13 hours more over 5 blocks. hashing in parallel in the harness, or a Dev Drive or a defender exclusion for the scratch folder, both of which need an administrator, would cut this; until one of them is in place, the windows archive workloads do not fit the time.
## live

`live-<source>.toml`, one per source, compare fetchloom with the source's own client, 3 blocks each, in WSL only. the method's changes say why.

| plan | tools |
|---|---|
| `live-url.toml` | fetchloom, curl-sha256, pooch |
| `live-doi.toml` | fetchloom, curl-sha256 on the file's zenodo url, pooch with the DOI |
| `live-zenodo.toml` | fetchloom, zenodo_get, datahugger, pooch with the DOI |
| `live-figshare.toml` | fetchloom, datahugger, pooch with the DOI |
| `live-dataverse.toml` | fetchloom, datahugger, pooch with the DOI |
| `live-osf.toml` | fetchloom, datahugger; pooch has no OSF support |
| `live-hf.toml` | fetchloom, hf (`hf download`, with xet) |
| `live-kaggle.toml` | fetchloom, kagglehub |
| `live-drive.toml` | fetchloom, gdown |
| `live-dropbox.toml` | fetchloom, curl-sha256 |

every tool keeps the files as the source serves them, so fetchloom runs with `--no-extract` and datahugger with `--no-unzip`; unpacking is compared in the lab. fetchloom, pooch, hf and kagglehub keep a cache and also run a second project, which downloads no file bodies.

the subset, from `bench/datasets.toml`, every source with its small datasets and one of 100 MB or more where the budget allows. counted at its worst, every tool's first fetch in all 3 blocks receives the whole dataset, 8.98 GB in all, under the 10 GB set aside for live comparisons; second projects download no file bodies. the first verified download adds 1.24 GB.

| dataset | cell | files | bytes | tools | bytes x tools x 3 |
|---|---|---|---|---|---|
| url-s1 | S1 | 1 | 3,858 | 3 | 34,722 |
| url-mzip | MZIP | 1 | 170,498,071 | 3 | 1,534,482,639 |
| doi-s1 | S1 | 1 | 4,177 | 3 | 37,593 |
| zenodo-s100 | S100 | 800 | 20,287,660 | 4 | 243,451,920 |
| figshare-s | S | 2 | 321,136 | 3 | 2,890,224 |
| figshare-m | M | 36 | 171,356,647 | 3 | 1,542,209,823 |
| dataverse-szip | SZIP | 3 | 303,511 | 3 | 2,731,599 |
| dataverse-mzip | MZIP | 1 | 124,549,762 | 3 | 1,120,947,858 |
| osf-s | S | 5 | 42,288,309 | 2 | 253,729,854 |
| hf-s1 | S1 | 3 | 91,617 | 2 | 549,702 |
| hf-szip | SZIP | 4 | 17,948,151 | 2 | 107,688,906 |
| hf-m100 | M100 | 181 | 270,035,224 | 2 | 1,620,211,344 |
| kaggle-s1 | S1 | 1 | 125,204 | 2 | 751,224 |
| kaggle-m1000 | M1000 | 10,032 | 228,459,385 | 2 | 1,370,756,310 |
| drive-file | S1 | 1 | unknown, small | 2 | |
| dropbox-s1 | S1 | 1 | 775,543 | 2 | 4,653,258 |
| dropbox-szip | SZIP | 1 | 8,645,770 | 2 | 51,874,620 |
| dropbox-m1 | M1 | 1 | 187,839,752 | 2 | 1,127,038,512 |
| total | | | 1,243,533,777 | | 8,984,040,108 |

left out: zenodo-m1 (2.63 GB with 4 tools and 3 blocks), zenodo-m1000 (3.54 GB), zenodo-l1 (16.1 GB), hf-m1 (1.27 GB), kaggle-m1 (1.43 GB) and kaggle-mzip (2.14 GB), which would pass 10 GB, and drive-folder, which now holds only a google doc that fetchloom refuses because its exported bytes can change. the url and dropbox datasets are archives or single files fetched as one file each; zenodo's many-file case is zenodo-s100.
before the first live block, write the expect files with a first verified download, which counts in the ledger too:

```sh
/home/fl/bench-venv/bin/python bench/expect-live.py
/home/fl/bench-venv/bin/python bench/harness.py bench/plans/live-zenodo.toml
```

`expect-live.py` downloads each dataset the live plans name once, with fetchloom, into a temporary folder with an empty cache, checks that every file hashes to what fetchloom recorded, and writes `bench/expect/live/<id>.sha256`. a dataset whose files change between blocks invalidates the blocks around the change: run `expect-live.py --force --only <id>` and compare.

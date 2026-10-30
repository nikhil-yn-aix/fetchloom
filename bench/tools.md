# benchmark tools

the comparison tools and their versions, installed on 2026-10-30 for the method in [benchmarks](../docs/benchmarks.md). the harness also records each tool's `version` command with every block.

| tool | windows | linux (WSL) |
|---|---|---|
| fetchloom | 0.1.0, the installed release in `%USERPROFILE%\.cargo\bin` | 0.1.0, built with `cargo build --release --locked` from branch `report` at `fd64035` |
| curl | 8.21.0, the system's `curl.exe`, Schannel | 8.5.0, OpenSSL 3.0.13, apt |
| bsdtar | 3.8.9, the system's `tar.exe`, with libzstd 1.5.7 | 3.7.2, `libarchive-tools` from apt, with libzstd 1.5.5 |
| aria2c | 1.37.0, winget `aria2.aria2` | 1.37.0, apt `aria2` |
| python | 3.13.18, uv's build | 3.13.18, uv's build |
| pooch | 1.9.0 | 1.9.0 |
| huggingface_hub (`hf`) | 2.4.0, with hf_xet 1.7.1 | 2.4.0, with hf_xet 1.7.1 |
| zenodo_get | 3.1.0 | 3.1.0 |
| kagglehub | 1.0.3 | 1.0.3 |
| gdown | 6.5.1 | 6.5.1 |
| datahugger | 0.13 | 0.13 |
| dvc | 3.67.1, with dvc-http 2.32.0 | 3.67.1, with dvc-http 2.32.0 |
| datalad | | 1.7.2 |
| git-annex | | 10.20240129, apt |
| git | | 2.43.0, apt |

the python packages sit in one virtual environment per system: `%LOCALAPPDATA%\fetchloom-work\bench-venv` on windows and `/home/fl/bench-venv` in WSL. every package in them is pinned in [requirements-windows.txt](tools/requirements-windows.txt) and [requirements-linux.txt](tools/requirements-linux.txt). the two agree on every shared package except python-dateutil, which datalad's dependencies hold at 2.9.0.post0 on linux.

the lab server is nginx 1.24.0 with OpenSSL 3.0.13 in ubuntu 24.04, WSL 2 kernel 6.18.54.1, on windows 11 build 26200.

## install

in WSL, as root:

```sh
apt-get install -y aria2 git-annex libarchive-tools python3-pip
```

in WSL, as `fl`, with uv 0.13.6 in `~/.local/bin`:

```sh
uv python install 3.13
uv venv --seed --python 3.13 /home/fl/bench-venv
uv pip install --python /home/fl/bench-venv/bin/python -r bench/tools/requirements-linux.txt
ln -sf /home/fl/target/report-release/release/fetchloom ~/.local/bin/fetchloom
```

windows cannot reach files.pythonhosted.org on this network, so its wheels come through WSL. `uv pip compile` resolves the windows environment, since `pip download --platform` evaluates markers such as `sys_platform == 'win32'` for the machine it runs on and would leave out windows-only packages. antlr4-python3-runtime 4.9.3 and gitignorefile 1.1.2 publish no wheels, so pure python wheels are built from their sources in WSL:

```sh
uv pip compile --python-platform x86_64-pc-windows-msvc --python-version 3.13 requirements.in -o bench/tools/requirements-windows.txt
python -m pip wheel --no-deps --no-binary=:all: -w ~/win-wheels antlr4-python3-runtime==4.9.3 gitignorefile==1.1.2
python -m pip download --no-deps --platform win_amd64 --python-version 3.13 --only-binary=:all: --find-links ~/win-wheels -d ~/win-wheels -r bench/tools/requirements-windows.txt
cp ~/win-wheels/*.whl /mnt/c/Users/t-nikhilyn/AppData/Local/fetchloom-work/wheels/
```

`requirements.in` lists the tools: `pooch==1.9.0`, `huggingface_hub[hf_xet]==2.4.0`, `zenodo_get==3.1.0`, `kagglehub==1.0.3`, `gdown==6.5.1`, `datahugger==0.13`, `dvc==3.67.1` and `psutil==7.2.2`. pip downloads into a linux folder first, since it cannot set file modes on `/mnt/c`.

on windows:

```powershell
winget install aria2.aria2 --version 1.37.0 --exact
uv venv --python 3.13 $env:LOCALAPPDATA\fetchloom-work\bench-venv
uv pip install --no-index --find-links $env:LOCALAPPDATA\fetchloom-work\wheels --python $env:LOCALAPPDATA\fetchloom-work\bench-venv\Scripts\python.exe -r bench\tools\requirements-windows.txt
```

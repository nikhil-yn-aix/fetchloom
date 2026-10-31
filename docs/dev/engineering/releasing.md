# releasing

releases package one executable for linux and windows on x86_64 and arm64, then attach checksums and provenance.

## workflow

[Cargo.toml](../../../Cargo.toml) owns the version; [pyproject.toml](../../../pyproject.toml) reads it dynamically. [release.yml](../../../.github/workflows/release.yml) runs on tags matching `v*.*.*` and rejects a tag that disagrees with Cargo. default permissions are empty; each job requests only what it needs.

| job | permissions | work |
|---|---|---|
| `build`, four matrix entries | contents read | build with `--locked --release`, check version and linkage, package an archive, build and install a wheel, exercise its executable and python module |
| `attest`, after all builds | contents read, id-token write, attestations write | gather archives and wheels, add installers, write `SHA256SUMS`, attest build provenance and upload the combined artifact |
| `release`, after attestation | contents write | create a draft GitHub release with every file |

| runner | target | linkage |
|---|---|---|
| `windows-latest` | `x86_64-pc-windows-msvc` | static C runtime, checked with dumpbin |
| `windows-11-arm` | `aarch64-pc-windows-msvc` | static C runtime; dumpbin check is skipped with a notice if the arm64 image lacks it |
| `ubuntu-latest` | `x86_64-unknown-linux-musl` | static musl binary, native dependencies built with musl-gcc; checked with ldd |
| `ubuntu-24.04-arm` | `aarch64-unknown-linux-musl` | same static-musl checks on native arm64 |

build jobs have a 60-minute timeout. attestation and release each have 15 minutes. maturin 1.16.0 is installed from pinned wheel hashes and reuses the checked binary. wheel smoke tests run `fetchloom --version` and verify that the python helper reports `project.invalid` outside a project.

## artifacts

for the current Cargo version, 0.1.0, the release contains four archives, four wheels, two installers and a checksum file.

| artifact | contents |
|---|---|
| `fetchloom-0.1.0-x86_64-pc-windows-msvc.zip` | `fetchloom.exe`, LICENSE, NOTICE and README.md in a matching top-level folder |
| `fetchloom-0.1.0-aarch64-pc-windows-msvc.zip` | same layout for arm64 windows |
| `fetchloom-0.1.0-x86_64-unknown-linux-musl.tar.gz` | `fetchloom`, LICENSE, NOTICE and README.md in a matching folder |
| `fetchloom-0.1.0-aarch64-unknown-linux-musl.tar.gz` | same layout for arm64 linux |
| `fetchloom-0.1.0-py3-none-win_amd64.whl` | windows executable and typed python module |
| `fetchloom-0.1.0-py3-none-win_arm64.whl` | same for arm64 windows |
| `fetchloom-0.1.0-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.musllinux_1_2_x86_64.whl` | static linux executable and python module, tagged for glibc and musl |
| `fetchloom-0.1.0-py3-none-manylinux2014_aarch64.manylinux_2_17_aarch64.musllinux_1_2_aarch64.whl` | same for arm64 linux |
| `install.sh`, `install.ps1` | release installers |
| `SHA256SUMS` | SHA-256 of every archive, wheel and installer |

## publish a version

1. set the Cargo version, refresh `Cargo.lock` through cargo and commit on main. wait for [CI](ci.md) on that commit.
2. create the matching tag and publish it. for version 0.1.0, the commands are `git tag v0.1.0` and `git push origin v0.1.0`.
3. watch the workflow with `gh run watch`. if it fails, fix the cause before retrying. a failed prerequisite does not create a release. retag only an unpublished failed release; published version tags must remain stable.
4. inspect the draft's four archives, four wheels, installers and checksums. download and check provenance:

   ```sh
   gh release download v0.1.0 --pattern '*.tar.gz' --dir check
   gh attestation verify check/fetchloom-0.1.0-x86_64-unknown-linux-musl.tar.gz --repo nikhil-yn-aix/fetchloom
   ```

5. edit release notes and publish the draft. installer `latest` URLs begin serving it then.
6. upload wheels manually to PyPI with a token scoped to the fetchloom project:

   ```sh
   gh release download v0.1.0 --pattern '*.whl' --dir wheels
   python -m pip install twine
   python -m twine check wheels/*
   python -m twine upload wheels/*
   ```

in a new virtual environment, verify `pip install fetchloom==0.1.0`, `fetchloom --version` and the python helper. trusted publishing is not configured; [roadmap](../roadmap.md#publishing) tracks it.

to exercise build jobs without a tag, temporarily add a branch trigger for `release-check` to a test branch of the workflow. `attest` and `release` remain tag-only. remove the test trigger and branch after the check.

## installers

[install.sh](../../../install.sh) detects x86_64 or arm64 linux and installs into `$HOME/.local/bin`, updating supported shell profiles when needed. [install.ps1](../../../install.ps1) detects 64-bit windows, including arm64, and installs into `%LOCALAPPDATA%\Programs\fetchloom\bin`, updating the user PATH.

both read `SHA256SUMS`, select the architecture's archive, verify its digest and stage beside the destination before moving the executable into place. explicit versions and latest releases are supported. `FETCHLOOM_DOWNLOAD_URL` replaces the release directory for local tests.

## local fake-release test

use a folder named `release` holding a locally packaged archive with the layout above and its matching `SHA256SUMS`. an archive from a successful build-job artifact also works; no published release is needed. on linux, from the folder holding those archives, write the checksum file with `sha256sum -- *.tar.gz > SHA256SUMS`. on windows, from the repository root:

```powershell
Get-ChildItem release\*.zip | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name
} | Set-Content -Encoding ascii release\SHA256SUMS
python -m http.server 8000 --bind 127.0.0.1 --directory release
```

serve the same folder on linux with `python3 -m http.server 8000 --bind 127.0.0.1 --directory release`. keep that terminal open. from another linux terminal at the repository root, isolate the install and profile changes:

```sh
mkdir -p .release-test/home
HOME="$PWD/.release-test/home" FETCHLOOM_DOWNLOAD_URL=http://127.0.0.1:8000 sh install.sh
.release-test/home/.local/bin/fetchloom --version
```

on windows, changing `LOCALAPPDATA` alone does not isolate the persistent user PATH. save and restore both the user PATH and process settings:

```powershell
$oldUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$oldPath = $env:Path
$oldLocal = $env:LOCALAPPDATA
$oldUrl = $env:FETCHLOOM_DOWNLOAD_URL
New-Item -ItemType Directory -Force .release-test\home | Out-Null
try {
    $env:LOCALAPPDATA = (Resolve-Path .release-test\home).Path
    $env:FETCHLOOM_DOWNLOAD_URL = 'http://127.0.0.1:8000'
    .\install.ps1
    & .\.release-test\home\Programs\fetchloom\bin\fetchloom.exe --version
} finally {
    [Environment]::SetEnvironmentVariable('Path', $oldUserPath, 'User')
    $env:Path = $oldPath
    $env:LOCALAPPDATA = $oldLocal
    $env:FETCHLOOM_DOWNLOAD_URL = $oldUrl
}
```

stop the local server with ctrl-c when finished and remove only the test folders you created. also test a deliberately wrong checksum: installation must fail before replacing the installed executable.

contributions and distributions use [Apache-2.0](../../../LICENSE). include [NOTICE](../../../NOTICE), which names Nikhil Y N and KairosLab India Private Limited. [decisions](../decisions.md#apache-20) explains the license boundary.

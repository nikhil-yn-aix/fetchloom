# releasing

the release workflow packages one executable for linux, windows and macos on x86_64 and arm64, with checksums and provenance. publishing the draft it creates sends the same files to PyPI and crates.io and rebuilds the apt repository on github pages.

no release has been published yet. use [source installation](../../user/getting-started.md#install) until the first release. the artifacts and installers below describe what a release will ship.

## workflow

[Cargo.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/Cargo.toml) owns the version; [pyproject.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/pyproject.toml) reads it dynamically. [release.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/release.yml) runs on tags matching `v*.*.*` and rejects a tag that disagrees with Cargo. default permissions are empty; each job requests only what it needs.

| job | permissions | work |
|---|---|---|
| `build`, six matrix entries | contents read | build with `--locked --release`, check version and linkage, package an archive, on linux a `.deb` installed and run once, build and install a wheel, exercise its executable and python module |
| `attest`, after all builds | contents read, id-token write, attestations write | gather archives and wheels, add installers, write `SHA256SUMS`, attest build provenance and upload the combined artifact |
| `release`, after attestation | contents write | create a draft GitHub release with every file |

| runner | target | linkage |
|---|---|---|
| `windows-latest` | `x86_64-pc-windows-msvc` | static C runtime, checked with dumpbin |
| `windows-11-arm` | `aarch64-pc-windows-msvc` | static C runtime; dumpbin check is skipped with a notice if the arm64 image lacks it |
| `ubuntu-latest` | `x86_64-unknown-linux-musl` | static musl binary, native dependencies built with musl-gcc; checked with ldd |
| `ubuntu-24.04-arm` | `aarch64-unknown-linux-musl` | same static-musl checks on native arm64 |
| `macos-15-intel` | `x86_64-apple-darwin` | links only libraries under `/usr/lib` and `/System/Library`, checked with `otool -L`; runs on macos 11 and later, checked with `otool -l` |
| `macos-latest` | `aarch64-apple-darwin` | same checks on native arm64 |

`MACOSX_DEPLOYMENT_TARGET` is 11.0, the first macos that runs on apple silicon, for the binaries and the wheels alike. `COPYFILE_DISABLE` keeps macos tar from adding `._` files that carry extended attributes. the macos binaries are not signed or notarized; a file downloaded in a browser carries the quarantine flag, while `install.sh` and `pip` download with tools that do not set it.

build jobs have a 60-minute timeout. attestation and release each have 15 minutes. maturin 1.16.0 is installed from pinned wheel hashes and reuses the checked binary. wheel smoke tests run `fetchloom --version` and verify that the python helper reports `project.invalid` outside a project.

[publish.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/publish.yml) runs when a draft is published. nothing reaches a package index before someone has looked at the draft.

| job | needs | work |
|---|---|---|
| `pypi` | the `pypi` environment and a PyPI trusted publisher naming `publish.yml` and that environment | download the release wheels, check them against `SHA256SUMS`, upload them |
| `crates` | the `CARGO_REGISTRY_TOKEN` secret, scoped to publishing fetchloom | `cargo publish --locked` from the tag |

## apt repository

github pages serves one site per repository, and a deployment replaces the whole site. [pages.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/pages.yml) is therefore the only workflow that deploys it: the documentation at the root and the apt repository under `/apt`. it runs when a release is published and when documentation changes on main, and each run rebuilds `/apt` from the latest published release.

the apt part downloads that release's `.deb` files and `SHA256SUMS`, checks them, then writes a flat repository signed with the `APT_SIGNING_KEY` secret, an armored ed25519 key without passphrase. it serves `https://nikhil-yn-aix.github.io/fetchloom/apt` with `Packages`, `Release`, `Release.gpg`, `InRelease` and the public key `fetchloom.gpg`. when no release is published, the run logs a notice and deploys the documentation without `/apt`.

the `github-pages` environment must allow `main` and tags matching `v*.*.*`, and pages must deploy from github actions. losing the signing key means generating a new one, replacing the secret and asking apt users to fetch `fetchloom.gpg` again. [CI](ci.md#documentation-site) describes the documentation half.

## artifacts

for the current Cargo version, 0.1.0, the workflow will produce six archives, two debian packages, six wheels, two installers and a checksum file.

| artifact | contents |
|---|---|
| `fetchloom-0.1.0-x86_64-pc-windows-msvc.zip` | `fetchloom.exe`, LICENSE, NOTICE and README.md in a matching top-level folder |
| `fetchloom-0.1.0-aarch64-pc-windows-msvc.zip` | same layout for arm64 windows |
| `fetchloom-0.1.0-x86_64-unknown-linux-musl.tar.gz` | `fetchloom`, LICENSE, NOTICE and README.md in a matching folder |
| `fetchloom-0.1.0-aarch64-unknown-linux-musl.tar.gz` | same layout for arm64 linux |
| `fetchloom-0.1.0-x86_64-apple-darwin.tar.gz` | same layout for x86_64 macos |
| `fetchloom-0.1.0-aarch64-apple-darwin.tar.gz` | same layout for arm64 macos |
| `fetchloom_0.1.0_amd64.deb`, `fetchloom_0.1.0_arm64.deb` | the static linux executable in `/usr/bin`, license and notice in `/usr/share/doc/fetchloom` |
| `fetchloom-0.1.0-py3-none-win_amd64.whl` | windows executable and typed python module |
| `fetchloom-0.1.0-py3-none-win_arm64.whl` | same for arm64 windows |
| `fetchloom-0.1.0-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.musllinux_1_2_x86_64.whl` | static linux executable and python module, tagged for glibc and musl |
| `fetchloom-0.1.0-py3-none-manylinux2014_aarch64.manylinux_2_17_aarch64.musllinux_1_2_aarch64.whl` | same for arm64 linux |
| `fetchloom-0.1.0-py3-none-macosx_11_0_x86_64.whl` | x86_64 macos executable and python module |
| `fetchloom-0.1.0-py3-none-macosx_11_0_arm64.whl` | same for arm64 macos |
| `install.sh`, `install.ps1` | release installers |
| `SHA256SUMS` | SHA-256 of every archive, package, wheel and installer |

## publish a version

1. set the Cargo version, refresh `Cargo.lock` through cargo and commit on main. wait for [CI](ci.md) on that commit.
2. create the matching tag and publish it. for version 0.1.0, the commands are `git tag v0.1.0` and `git push origin v0.1.0`.
3. watch the workflow with `gh run watch`. if it fails, fix the cause before retrying. a failed prerequisite does not create a release. retag only an unpublished failed release; published version tags must remain stable.
4. inspect the draft's archives, debian packages, wheels, installers and checksums. download and check provenance:

   ```sh
   gh release download v0.1.0 --pattern '*.tar.gz' --dir check
   gh attestation verify check/fetchloom-0.1.0-x86_64-unknown-linux-musl.tar.gz --repo nikhil-yn-aix/fetchloom
   ```

5. edit release notes and publish the draft. installer `latest` URLs begin serving it then, and `publish.yml` and `pages.yml` start. watch them with `gh run watch`.
6. in a new virtual environment, verify `pip install fetchloom==0.1.0`, `fetchloom --version` and the python helper. check `cargo install fetchloom` and both installers on a clean machine or container.
7. after the first release, rewrite the install text that still says no release exists: the [README](https://github.com/nikhil-yn-aix/fetchloom/blob/main/README.md#install), [getting started](../../user/getting-started.md#install), [macos](../../user/macos.md#install) and the [python helper](../../user/scripting.md#python-helper).
8. update winget. the first version is submitted by hand from windows with `wingetcreate new` and the x86_64 and arm64 zip URLs, as a portable package `KairosLab.fetchloom` with the command `fetchloom`. later versions use `wingetcreate update KairosLab.fetchloom --version 0.1.1 --urls <x86_64 zip> <arm64 zip> --submit`. microsoft reviews each pull request.

to exercise build jobs without a tag, temporarily add a branch trigger for `release-check` to a test branch of the workflow. `attest` and `release` remain tag-only. remove the test trigger and branch after the check.

## installers

the planned release will include [install.sh](https://github.com/nikhil-yn-aix/fetchloom/blob/main/install.sh), which detects x86_64 or arm64 linux and macos and installs into `$HOME/.local/bin`, updating supported shell profiles when needed. on macos it also writes `~/.zshrc`, since zsh, the default shell there, never reads `~/.profile`, and a shell running under rosetta on apple silicon gets the arm64 build. [install.ps1](https://github.com/nikhil-yn-aix/fetchloom/blob/main/install.ps1) detects 64-bit windows, including arm64, and installs into `%LOCALAPPDATA%\Programs\fetchloom\bin`, updating the user PATH.

both read `SHA256SUMS`, select the architecture's archive, verify its digest and stage beside the destination before moving the executable into place. an explicit version takes precedence over `FETCHLOOM_VERSION`; otherwise the installer requests the latest release. `FETCHLOOM_NO_MODIFY_PATH=1` leaves shell profiles and the user PATH alone. `FETCHLOOM_DOWNLOAD_URL` replaces the release directory for local tests.

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

contributions and distributions use [Apache-2.0](https://github.com/nikhil-yn-aix/fetchloom/blob/main/LICENSE). include [NOTICE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/NOTICE), which names Nikhil Y N and KairosLab India Private Limited. [decisions](../decisions.md#apache-20) explains the license boundary.

# releasing

the release workflow packages one executable for linux, windows and macos on x86_64 and arm64, with checksums and provenance. publishing the draft it creates sends the same files to PyPI and crates.io and rebuilds the apt repository on github pages.

no release has been published yet. use [source installation](../../user/getting-started.md#install) until the first release. the artifacts and installers below describe what a release will ship.

## workflow

[Cargo.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/Cargo.toml) owns the version; [pyproject.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/pyproject.toml) reads it dynamically. [release.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/release.yml) runs on tags matching `v*.*.*` and rejects a tag that disagrees with Cargo. default permissions are empty; each job requests only what it needs.

| job | permissions | work |
|---|---|---|
| `build`, reusable artifact workflow | contents read | run the same six-target builds, installed artifact journeys and exact inventory as [CI](ci.md#installed-artifacts), plus the source-package installation |
| `attest`, after all builds | contents read, id-token write, attestations write | gather archives, packages, wheels and the sbom, add installers, write `SHA256SUMS`, attest build provenance and upload the combined artifact |
| `release`, after attestation | contents write | create a draft GitHub release with every file |

| runner | target | linkage |
|---|---|---|
| `windows-2025` | `x86_64-pc-windows-msvc` | static C runtime, checked by reading the dlls the executable imports, delay-loaded ones included, from its own import tables; the check fails when it cannot read them or finds no `kernel32.dll` |
| `windows-11-arm` | `aarch64-pc-windows-msvc` | same static C runtime check on native arm64; it needs no visual studio tool, so the arm64 image cannot skip it |
| `ubuntu-24.04` | `x86_64-unknown-linux-musl` | static musl binary, native dependencies built with musl-gcc; checked with ldd |
| `ubuntu-24.04-arm` | `aarch64-unknown-linux-musl` | same static-musl checks on native arm64 |
| `macos-15`, checked on `macos-15-intel` | `x86_64-apple-darwin` | links only libraries under `/usr/lib` and `/System/Library`, checked with `otool -L`; runs on macos 11 and later, checked with `otool -l` |
| `macos-15` | `aarch64-apple-darwin` | same checks on native arm64 |

`MACOSX_DEPLOYMENT_TARGET` is 11.0, the first macos that runs on apple silicon, for the binaries and the wheels alike. `COPYFILE_DISABLE` keeps macos tar from adding `._` files that carry extended attributes. the macos binaries are not signed or notarized; a file downloaded in a browser carries the quarantine flag, while `install.sh` and `pip` download with tools that do not set it. the windows executables are not signed either. signing and notarizing need the owner's accounts: an apple developer id with `notarytool`, and a windows code-signing certificate. none is set up, so the workflow has no step for them.

rustc writes the paths of the files it compiles into the binary, for panic messages among other uses. before building, each job adds `--remap-path-prefix` flags to its target's `CARGO_TARGET_<triple>_RUSTFLAGS`, naming the cargo home `/cargo` and the checkout `/src`, so registry sources and generated files no longer carry the runner's paths. the step fails if either path holds whitespace, since cargo splits these flags at whitespace. the workflow must never set `RUSTFLAGS` or `CARGO_ENCODED_RUSTFLAGS`: cargo uses the first of those that is set in place of every per-target variable, so the windows binaries would lose `+crt-static` and every binary its remapping. the per-target flags belong to artifacts.yml.

build jobs have a 60-minute timeout and the intel check job 20 minutes. attestation and release each have 15 minutes. maturin 1.16.0 is installed from pinned wheel hashes and reuses the checked binary. wheel tests run the [installed loopback journey](ci.md#installed-artifacts), including `fl --version`, `python -m fetchloom --version`, successful helper calls and structured errors, in fresh venvs at the documented Python endpoints.

`artifacts.yml` has only contents-read permission and also accepts manual dispatch. dispatching `release.yml` runs that same builder as a dry run: attestation and draft creation require a tag push and are skipped even when dispatch names a tag. neither rehearsal publishes packages or receives publishing secrets. the fourteen candidate files and their check receipts remain run artifacts.

[publish.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/publish.yml) runs when a draft is published. nothing reaches a package index before someone has looked at the draft.

| job | needs | work |
|---|---|---|
| `pypi` | the `pypi` environment and a PyPI trusted publisher naming `publish.yml` and that environment | download the release wheels, check them against `SHA256SUMS`, upload them |
| `crates` | the `crates` environment holding the `CARGO_REGISTRY_TOKEN` secret, scoped to publishing fetchloom | `cargo publish --locked` from the tag |

both environments deploy only from tags matching `v*.*.*`, and each waits for the owner to approve the run. keep the crates token in the `crates` environment, not among the repository's secrets, which a workflow on any branch can read.

`python/check_release.py` checks the downloaded package inventory before publication: all six wheels or both debs must have one checksum entry and matching bytes. it rejects missing packages, unlisted downloads, duplicate entries and malformed checksum lines. entries for other release artifact types may remain undownloaded. update the expected counts with the release matrix when adding a platform. this check detects incomplete or inconsistent assets; it does not independently authenticate a release whose assets and checksums were both replaced.

## apt repository

github pages serves one site per repository, and a deployment replaces the whole site. [pages.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/pages.yml) is therefore the only workflow that deploys it: the documentation at the root and the apt repository under `/apt`. it runs when a release is published and when documentation changes on main, and each run rebuilds `/apt` from the latest published release.

the apt part downloads that release's `.deb` files and `SHA256SUMS`, checks them, then writes a flat repository signed with the `APT_SIGNING_KEY` secret, an armored ed25519 key without passphrase. it serves `https://nikhil-yn-aix.github.io/fetchloom/apt` with `Packages`, `Release`, `Release.gpg`, `InRelease` and the public key `fetchloom.gpg`. when no release is published, the run logs a notice and deploys the documentation without `/apt`.

the `github-pages` environment must allow `main` and tags matching `v*.*.*`, and pages must deploy from github actions. losing the signing key means generating a new one, replacing the secret and asking apt users to fetch `fetchloom.gpg` again. [CI](ci.md#documentation-site) describes the documentation half.

## artifacts

for the current Cargo version, 0.1.0, the workflow will produce six archives, two debian packages, six wheels, an sbom, two installers and a checksum file.

the archives are packed so the same files give the same bytes. the build job exports `SOURCE_DATE_EPOCH`, the tag commit's committer time, and every file and folder in an archive gets that time. tar archives list their entries in byte order, record owner and group as 0 without names and pass through `gzip -n`, so the gzip header holds no file name or time; macos writes them in ustar format. windows zips are written by 7-Zip from the same sorted list, with `-mtc=off`, which leaves out the ntfs creation and access times.

| artifact | contents |
|---|---|
| `fetchloom-0.1.0-x86_64-pc-windows-msvc.zip` | `fetchloom.exe`, LICENSE, NOTICE, README.md, THIRD-PARTY-LICENSES and the manual page `fetchloom.1` in a matching top-level folder |
| `fetchloom-0.1.0-aarch64-pc-windows-msvc.zip` | same layout for arm64 windows |
| `fetchloom-0.1.0-x86_64-unknown-linux-musl.tar.gz` | `fetchloom`, LICENSE, NOTICE, README.md, THIRD-PARTY-LICENSES and `fetchloom.1` in a matching folder |
| `fetchloom-0.1.0-aarch64-unknown-linux-musl.tar.gz` | same layout for arm64 linux |
| `fetchloom-0.1.0-x86_64-apple-darwin.tar.gz` | same layout for x86_64 macos |
| `fetchloom-0.1.0-aarch64-apple-darwin.tar.gz` | same layout for arm64 macos |
| `fetchloom_0.1.0_amd64.deb`, `fetchloom_0.1.0_arm64.deb` | the static linux executable in `/usr/bin`, with `/usr/bin/fl` a link to it, license, notice and third-party licenses in `/usr/share/doc/fetchloom`, and the manual page, which `man fetchloom` and `man fl` open |
| `fetchloom-0.1.0-py3-none-win_amd64.whl` | windows executable and typed python module |
| `fetchloom-0.1.0-py3-none-win_arm64.whl` | same for arm64 windows |
| `fetchloom-0.1.0-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.musllinux_1_2_x86_64.whl` | static linux executable and python module, tagged for glibc and musl |
| `fetchloom-0.1.0-py3-none-manylinux2014_aarch64.manylinux_2_17_aarch64.musllinux_1_2_aarch64.whl` | same for arm64 linux |
| `fetchloom-0.1.0-py3-none-macosx_11_0_x86_64.whl` | x86_64 macos executable and python module |
| `fetchloom-0.1.0-py3-none-macosx_11_0_arm64.whl` | same for arm64 macos |
| `fetchloom-0.1.0.cdx.json` | the CycloneDX sbom maturin writes into every wheel, here the x86_64 linux wheel's: every crate fetchloom depends on, for any platform, with its version, license and checksum |
| `install.sh`, `install.ps1` | release installers |
| `SHA256SUMS` | SHA-256 of every archive, package, wheel, sbom and installer |

THIRD-PARTY-LICENSES holds the license texts of the crates the six release binaries are built from, which most of those licenses ask to travel with every copy. each build job writes it with cargo-about 0.9.3, from [about.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/about.toml), which accepts the licenses deny.toml allows, and the [about.hbs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/about.hbs) template. it reads only the crates `cargo fetch --locked` downloaded, so every job writes the same bytes, and it fails on a crate whose license it cannot read or that deny.toml does not allow. the archives and the debian packages carry it, and each wheel lists it among its license files; a wheel built from a checkout, which has no such file, leaves it out. to write it locally:

```sh
cargo fetch --locked
cargo about generate --frozen --fail about.hbs -o THIRD-PARTY-LICENSES
```

the manual page is [docs/man/fetchloom.1](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/man/fetchloom.1): the top command's synopsis, description and options, then each command with its one-line summary. a unit test renders it from the command line's definition with clap_mangen, a dev-dependency, and fails when the committed page differs, so the release copies a page that matches its help; [building and testing](building-and-testing.md#update-snapshots) says how to write it again. the debian packages keep it compressed in `/usr/share/man/man1`.

a pre-release such as 0.1.0-rc.1 keeps that spelling in the archive and debian package file names. its wheels carry pep 440's spelling, 0.1.0rc1, as maturin writes it, with alpha and beta shortened to a and b. its debian packages record 0.1.0~rc.1 as their version, which dpkg sorts before 0.1.0, so the release replaces the pre-release on upgrade.

every wheel the release builds also has `fl`, a console script. maturin refuses console scripts in a wheel that carries an executable, so the build job adds it after `maturin build` with `python/wheel_scripts.py`: an `entry_points.txt` naming the launcher, which pip and uv turn into a command, `fl.exe` on windows, and its line in RECORD, with every other file left as maturin wrote it. the launcher, `python/fetchloom/__main__.py`, finds the executable the wheel installed beside it, as the helper does, and hands it the command line. on unix it replaces itself with the executable through `os.execv`; windows has no exec, so there it runs the executable, leaves ctrl-c to it and exits with its code. `python -m fetchloom` runs the same launcher. wheels have no links, and a second copy of the executable would double the wheel, so `fl` costs a python start instead. a wheel maturin builds from source, as `pip install` from git does, has no `fl`; `python -m fetchloom` works there too.

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
6. in a new virtual environment, verify `pip install fetchloom==0.1.0`, `fetchloom --version`, `fl --version` and the python helper. check `cargo install fetchloom` and both installers on a clean machine or container.
7. after the first release, rewrite the install text that still says no release exists: the [README](https://github.com/nikhil-yn-aix/fetchloom/blob/main/README.md#install), [getting started](../../user/getting-started.md#install), [macos](../../user/macos.md#install) and the [python helper](../../user/scripting.md#python-helper).
8. update winget. the first version is submitted by hand from windows with `wingetcreate new` and the x86_64 and arm64 zip URLs, as a portable package `KairosLab.fetchloom` with the command `fetchloom`. later versions use `wingetcreate update KairosLab.fetchloom --version 0.1.1 --urls <x86_64 zip> <arm64 zip> --submit`. microsoft reviews each pull request.

to rehearse without a tag, dispatch `release.yml` against the branch being checked. only its read-only artifact workflow runs; `attest` and `release` remain tag-only. no branch trigger, tag or publication is needed.

## installers

the planned release will include [install.sh](https://github.com/nikhil-yn-aix/fetchloom/blob/main/install.sh), which detects x86_64 or arm64 linux and macos and installs into `$HOME/.local/bin`, updating supported shell profiles when needed. on macos it also writes `~/.zshrc`, since zsh, the default shell there, never reads `~/.profile`, and a shell running under rosetta on apple silicon gets the arm64 build. [install.ps1](https://github.com/nikhil-yn-aix/fetchloom/blob/main/install.ps1) detects 64-bit windows, including arm64, and installs into `%LOCALAPPDATA%\Programs\fetchloom\bin`, updating the user PATH.

the shell installer refuses an existing directory or directory symlink at `$HOME/.local/bin/fetchloom` before staging, and checks again before the final move. it leaves the directory and its contents alone. move the conflicting directory aside before installing.

`install.sh` also makes `fl` in that folder, a link to `fetchloom` beside it, so an upgrade carries the short name along with nothing more to download. it never hides or replaces another `fl`: it leaves alone an `fl` already there that is not such a link, and makes none when `command -v fl` finds another program, earlier or later on PATH. either way it prints one line saying so and how to make the short name by hand. a reinstall keeps the link and puts back a missing one.

`install.ps1` makes `fl.exe` in its `bin` folder as a hardlink to `fetchloom.exe`, or a copy where the drive makes no hardlinks. a hardlink keeps the old bytes once `fetchloom.exe` is replaced, so every install deletes `fl.exe` and makes it again. when `Get-Command fl -CommandType Application -All` finds another `fl` on PATH, it makes none and says how to get the short name in powershell instead. powershell runs an alias before any program, and `fl` is the alias of `Format-List` in windows powershell 5.1 and powershell 7, so `fl` there keeps meaning `Format-List`; the installer says to type `fl.exe`. a failure to make `fl.exe` is a warning, since `fetchloom.exe` is in place by then.

both read `SHA256SUMS`, select the architecture's archive, verify its digest and stage it beside the destination. they run the staged executable's `--version` before moving it into place; when it cannot run, `install.sh` prints an `error:` line, `install.ps1` throws, and the installed executable stays as it was. an explicit version takes precedence over `FETCHLOOM_VERSION`; otherwise the installer requests the latest release. `FETCHLOOM_NO_MODIFY_PATH=1` leaves shell profiles and the user PATH alone. `FETCHLOOM_DOWNLOAD_URL` replaces the release directory for local tests.

`install.sh` downloads with curl when it can, held to https and TLS 1.2 or later by `--proto =https --tlsv1.2`. otherwise it uses wget with `--https-only --secure-protocol=TLSv1_2`, the options rustup's installer passes; `--secure-protocol=TLSv1_2` makes wget speak TLS 1.2 only, and gnu wget applies `--https-only` to recursive downloads alone, so wget does not refuse a redirect to http the way curl does. busybox wget takes neither option, so the installer refuses it before any download and says to install curl: a download it cannot hold to https would let anyone on the way swap the archive and `SHA256SUMS` alike. a `FETCHLOOM_DOWNLOAD_URL` test folder may be served over http, so neither downloader insists on https for it.

## local fake-release test

`python3 -m unittest discover -s python -p test_install.py` runs the shell installer against a loopback HTTP server on linux. it uses a fake executable and a private home to check matching and mismatched digests, failed version checks, the refusal of a wget that cannot hold to https, idempotent profile changes, the `fl` link, its reinstall and the skip beside another `fl`. its PATH holds only the folders of the programs the installer runs, so an `fl` elsewhere on the machine cannot change the result. it contacts no release service and leaves the real home and installed program alone. the digest regression also checks that a mismatched archive is never executed.

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
.release-test/home/.local/bin/fl --version
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

stop the local server with ctrl-c when finished and remove only the test folders you created. also test a deliberately wrong checksum and an archive whose executable cannot run: installation must fail before replacing the installed executable.

contributions and distributions use [Apache-2.0](https://github.com/nikhil-yn-aix/fetchloom/blob/main/LICENSE). include [NOTICE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/NOTICE), which names Nikhil Y N and KairosLab India Private Limited. [decisions](../decisions.md#apache-20) explains the license boundary.

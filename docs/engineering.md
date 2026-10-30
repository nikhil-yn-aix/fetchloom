# engineering

## build

`rust-toolchain.toml` pins rust 1.99.1 with rustfmt and clippy. `Cargo.toml` uses edition 2024 and declares a minimum rust version of 1.99. one crate builds a library and two executables, `fetchloom` and `fl`.

```sh
cargo build --locked
cargo run -- --version
```

the compression dependencies need native build tools. keep `Cargo.lock` when building this application. [README](../README.md#install) covers source installation.

## code boundaries

parsers, digest types, glob matching and destination expansion can be tested as functions over data. project and cache modules own disk records. the CLI orders their effects; the HTTP layer also writes through the cache. [architecture](architecture.md#modules) maps the modules.

`Api` is the source-listing test seam. tests also inject environment readers and archive limits. retry timing still uses the real clock and randomness. dependencies are not all hidden behind private adapters: `url::Url`, for example, crosses module boundaries.

the crate forbids unsafe code. cargo configures warnings for missing public docs, unreachable public items and clippy's pedantic group, plus unwraps, expects, panics, debug prints and unfinished-code macros. errors use a hand-written `Error` and `Kind`, not `thiserror`. dead code fails the build like any other warning.

keep modules focused and comments useful. there is no enforced file-length or function-length limit. lint exceptions need reasons. writing and commit rules are in [style](style.md).

## checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

after a dependency change, also run `cargo deny check`, installed with `cargo install --locked cargo-deny`.

run the smallest relevant selector while working, then the full suite when the change crosses command boundaries:

```sh
cargo test --locked --test cli
cargo test --locked --test cli basics::the_options_scope_names_are_the_ones_add_and_get_take
cargo test --locked source::zenodo
cargo test --locked archive::
```

the first command runs the real-binary integration target. the second checks the option sentence in `docs/scope.md`. other markdown examples are not executed by the test suite.

## test layers

| location | checks |
|---|---|
| module `#[cfg(test)]` tests | parsing, rendering, hashes, paths, glob matching, limits and storage behavior |
| `src/source/*` tests | source-specific responses through the in-memory `Recorded` implementation of `Api` |
| `tests/fixtures/sources` | captured JSON listings and errors used by source tests |
| `tests/fixtures/archives` | zip and tar files from several writers, including unsafe and damaged cases |
| `tests/cli` | the compiled binary against a custom `TcpListener` server |
| `tests/cli/snapshots` | human output and selected JSON or lock output checked with insta |

CLI cases cover repeat downloads, retries, resume, source-link recovery, offline use, project edits, archives, cache upkeep, concurrent processes, proxies and secret-bearing links. source tests do not all run a common conformance suite.

the CLI harness creates a private working directory, cache and home. it removes inherited fetchloom settings, source token variables, proxy variables and synced-folder settings before starting the binary. it retains normal OS inputs such as `PATH`, `CI` and `TERM`. tests use local servers or recorded answers, not live repositories.

snapshot filters replace sandbox paths, ports and durations, normalize path separators, and remove variable rates. review the unfiltered output when changing wording; a filtered snapshot does not prove every displayed value is right. snapshots and JSON counters check different contracts.

for a bug fix, first add a test that fails for the reported behavior. use the lowest layer that demonstrates it. add a CLI case when exit codes, stdout, files left on disk or cross-command recovery are part of the bug. do not weaken an assertion to hide a failure.

## fuzzing

five parsers have fuzz targets, written as [bolero](https://github.com/camshaft/bolero) tests inside their modules so the parsers stay private. each asserts what must hold for any input: nothing panics, what a parser accepts keeps its own rules, and where a writer exists, what it writes reads back the same.

| target | reads |
|---|---|
| `archive::names::tests::fuzz_member_names` | archive member names |
| `project::lock::canonical::tests::fuzz_lock_reader` | `data.lock`, fast reader and TOML reader |
| `project::manifest::tests::fuzz_manifest` | `data.toml`, with adding and removing datasets |
| `source::drive::folder::tests::fuzz_drive_pages` | drive folder pages, download pages and their HTML |
| `net::headers::tests::fuzz_response_headers` | response headers and the file name a server gives |

`cargo test` runs each for one second of random input; `cargo test --locked --lib fuzz_` runs only them. to fuzz one with libfuzzer, on linux with a nightly toolchain:

```sh
rustup toolchain install nightly
cargo install --locked cargo-bolero --no-default-features --features libfuzzer
cargo bolero test --toolchain nightly -T 10min --corpus-dir ../fuzz/manifest --crashes-dir ../fuzz/manifest-crashes project::manifest::tests::fuzz_manifest
```

cargo bolero builds with the `fuzz` profile in `Cargo.toml`. without the two folders it keeps its corpus in `__fuzz__` beside the source, which git ignores. a crash becomes a regression test in the module's tests before it is fixed.

## automation

[ci.yml](../.github/workflows/ci.yml) runs the three checks above on linux and windows. on linux it also runs `cargo deny check`, which fails on a crate with an open security advisory, a license [deny.toml](../deny.toml) does not list or a source other than crates.io, and warns when two versions of a crate are built. it then runs every test again with its folders on other filesystems, all required: refs, exfat and fat32 volumes on windows, and btrfs and xfs on linux. there are no nightly live-source checks. [release.yml](../.github/workflows/release.yml) builds a release from a version tag, as [releases](#releases) describes.

nextest, wiremock, proptest, cargo-fuzz, cargo-mutants, cargo-shear, typos, vale, zizmor and a justfile are not configured in this repository. fuzzing uses bolero, described above. `bench/` holds the benchmark harness and the live source matrix; [benchmarks](benchmarks.md) is their method. add tooling with the job it will run; do not document it as an existing gate first.

## releases

`Cargo.toml` holds the version, and `pyproject.toml` takes it from there. `publish = false` keeps the crate off crates.io.

[release.yml](../.github/workflows/release.yml) runs when a tag such as `v0.1.0` is pushed. each job has only the permissions it needs:

| job | permissions | work |
|---|---|---|
| `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-musl` | read the repository | check that the tag matches `Cargo.toml`; build both executables with `--locked` and the release profile, on windows with a static C runtime and on linux static against musl, with musl-gcc for the C dependencies; check that each prints its version, that `ldd` calls the linux ones static and that the windows ones load no C runtime DLL; pack them with LICENSE, NOTICE and README.md; wrap the same binaries in a wheel with maturin, install it, and run both commands and the python module |
| `attest` | read the repository, request an OIDC token, write attestations | write `SHA256SUMS` over the archives, wheels and installers, and attest their build provenance |
| `release` | write contents | create a draft GitHub release holding every file |

a release holds:

| file | contents |
|---|---|
| `fetchloom-<version>-x86_64-pc-windows-msvc.zip` | `fetchloom.exe`, `fl.exe`, LICENSE, NOTICE and README.md, in a folder named like the archive |
| `fetchloom-<version>-x86_64-unknown-linux-musl.tar.gz` | `fetchloom`, `fl` and the same three files |
| `fetchloom-<version>-py3-none-win_amd64.whl` | both executables and the `fetchloom` python module |
| `fetchloom-<version>-py3-none-manylinux2014_x86_64.manylinux_2_17_x86_64.musllinux_1_2_x86_64.whl` | the same for linux. the binaries are static, so the wheel carries the glibc and musl tags |
| `install.sh`, `install.ps1` | the installers the [README](../README.md#install) runs |
| `SHA256SUMS` | the SHA-256 of every other file, which the installers check |

maturin packs every `[[bin]]` target, so each wheel holds `fl` as well as `fetchloom`.

to make a release:

1. set `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, and commit on main. wait for CI to pass on that commit.
2. tag the commit and push the tag:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

3. watch the run with `gh run watch`. a failed run creates no release. fix the cause, delete the tag with `git tag -d v0.1.0` and `git push origin :refs/tags/v0.1.0`, and tag again.
4. open the draft release on GitHub and check that it holds the seven files above. check the provenance of a download:

   ```sh
   gh release download v0.1.0 --pattern '*.tar.gz' --dir check
   gh attestation verify check/fetchloom-0.1.0-x86_64-unknown-linux-musl.tar.gz --repo nikhil-yn-aix/fetchloom
   ```

5. edit the notes and publish the draft. the installers' `latest` links serve it from then on.
6. upload the wheels to PyPI by hand, with an API token scoped to the `fetchloom` project:

   ```sh
   gh release download v0.1.0 --pattern '*.whl' --dir wheels
   python -m pip install twine
   python -m twine check wheels/*
   python -m twine upload wheels/*
   ```

   then, in a new virtual environment, run `pip install fetchloom==0.1.0` and `fetchloom --version`. PyPI trusted publishing is not set up, so this upload stays manual; [plan](plan.md#next) tracks it.

to try the build jobs without a tag, add `branches: [release-check]` under `push` in release.yml and push that branch. `attest` and `release` run only for tags. delete the branch after the run.

to try the installers against local files, serve a folder that holds a release's files in one terminal, and point `FETCHLOOM_DOWNLOAD_URL` at it from another:

```sh
python -m http.server 8000 --directory release
FETCHLOOM_DOWNLOAD_URL=http://127.0.0.1:8000 sh install.sh
```

on windows, set `$env:FETCHLOOM_DOWNLOAD_URL` and run `.\install.ps1`. to keep your own install apart, set `HOME` on linux or `LOCALAPPDATA` on windows to a scratch folder; the windows installer still adds that folder to your user PATH.

`data.lock` and JSON each use format version 1. format readers and recovery behavior are in [formats](formats.md); a version field alone is not a migration system.

## license

contributions use apache-2.0. keep [LICENSE](../LICENSE) and [NOTICE](../NOTICE) with distributions. NOTICE names Nikhil Y N and KairosLab India Private Limited. the [license decision](decisions.md#26-apache-20) records why.

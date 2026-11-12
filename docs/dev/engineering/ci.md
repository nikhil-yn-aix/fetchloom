# CI

CI checks native behavior, filesystem paths and installed release artifacts before merge.

## triggers and the required check

[ci.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/ci.yml) runs on pushes to main, every pull request, merge groups and manual dispatch. every run is full: there are no path filters or docs-only skips. repository permission is read-only. newer PR commits cancel their predecessors; main and dispatched revisions have separate concurrency groups. matrices do not fail fast.

the one required check is `ci`, with `if: always()` and `needs: [quality, docs, test, windows, windows-build, windows-fs, snapshots, artifacts]`. every prerequisite must report `success`; a failure, cancellation or skip fails the gate. require `ci` in branch protection, not individual platform names. no step may turn a failed test into success to get this check green.

## native tests

| runner | coverage |
|---|---|
| `ubuntu-24.04` | x86_64 linux, rustdoc, python tests, btrfs, xfs, a chmod-refusing FAT mount and a folder that ignores case |
| `ubuntu-24.04-arm` | native arm64 linux |
| `windows-2025` | native x86_64 windows on NTFS |
| `windows-11-arm` | native arm64 windows |
| `macos-15` | native arm64 macos on APFS, python tests and HFS+ |

all five install the toolchain pinned in rust-toolchain.toml. clippy runs with warnings denied on x86_64 linux, x86_64 windows and the mac; the crate has no code that differs by processor, so the arm64 linux and windows runners would lint the same code. linux x86_64 also runs `cargo clippy --release --locked -- -D warnings` and `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps`. the intel cross-check is replaced by a native `macos-15-intel` artifact journey; intel does not repeat the source test suite.

nextest 0.9.151 runs the ordinary unit and CLI tests, including the short Bolero properties. [.config/nextest.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.config/nextest.toml) sets four concurrent tests, one retry and `flaky-result = "fail"`. a test that passes only on retry still fails CI. a slow test is reported after 30 seconds; six periods terminate a hung test as failure. this is a hang guard, not a wall-clock performance assertion. the crate has no doc tests, so CI runs none; the change that adds the first one also adds a `cargo test --locked --doc` step to each native job.

`INSTA_UPDATE=no` forbids snapshot updates. insta 1.49.0 records the snapshot paths each run references through `INSTA_SNAPSHOT_REFERENCES_FILE`; the runner converts them to repository-relative reports. the `snapshots` job checks their union against every tracked `.snap` file and rejects unused, unknown or pending snapshots. it needs all native and filesystem jobs: a reflink-only snapshot is legitimately unused on ext4, but must be exercised on a cloning filesystem. this check adds no second test run. review snapshots locally as [building and testing](building-and-testing.md#update-snapshots) describes; CI never accepts them.

`cargo test` still works locally. the recipe step is built before CI starts tests; a test reuses it without invoking cargo. a local cargo test still asks cargo to refresh it once, so edits to the example are not hidden by a stale executable. test executable and fixture paths prefer Cargo's runtime paths, so an archived build can run from a different checkout directory. new fixture readers use `support::workspace()` rather than a compile-time manifest path, which still names the producer's checkout. no test changes the process environment.

## selected-test proof and reports

[python/ci_tests.py](https://github.com/nikhil-yn-aix/fetchloom/blob/main/python/ci_tests.py) compares Cargo's test-binary inventory with nextest's. it asks those libtest binaries for their ordinary and ignored test IDs, both with and without the old filesystem `--skip` arguments. before each nextest run, its selected nonignored IDs must equal that recorded selection and must not be empty. the chmod case and the alias tests have exact selections of their own. doctests remain separate from this comparison.

reports under `target/ci` contain the selections and each run's JUnit XML. failures and retry output remain in JUnit; slowest cases also appear in the step summary. reports upload even after a failure, with OS, filesystem and run attempt in their artifact names, and remain for seven days. a successful test run without a JUnit report or with different executed IDs fails the job.

## filesystem runs

every native runner executes the full ordinary suite on its default disk. extra filesystems repeat the existing disk-touching selection, excluding the same substrings the old `DISKLESS` list excluded. a new disk test must not accidentally match one of those exclusions; the selection report makes that decision inspectable.

linux reruns the built tests on btrfs and xfs loop images. their mounted filesystem types are checked first. a separate root-mounted FAT image lets the runner write but refuses chmod; `FETCHLOOM_TEST_NOOP_CHMOD` names it for the one writable-copy regression. an ext4 image made with casefold holds a folder flagged to ignore case, where the alias tests run with `FETCHLOOM_TEST_REQUIRE=aliases`, so one that skips there fails. macos attaches an HFS+ image for decomposed-unicode and no-reflink behavior. cleanup traps detach the images. macos disables Spotlight indexing immediately after attachment, records the owned device from hdiutil's plist, and makes three normal detach attempts with two-second pauses before forcing that device. it uses the device rather than the mountpoint: a failed eject can already have removed the mountpoint. cleanup preserves a failed test's exit code and fails a passing run if the owned device still cannot be detached. btrfs/xfs and HFS+ each have a 12-minute step cap; the linux step logs provisioning, mount, test and detach phases.

a second `windows-2025` job builds and archives the test build once, including `recipe_step` and the libtest selection record, while the native job runs its suite. three jobs independently create ReFS, exFAT and FAT32 volumes, verify `Get-Volume` reports the requested filesystem, and point `TMP` and `TEMP` at that volume. each runs the entire disk selection, not one third. they depend only on the windows producer, not the slower linux/mac matrix. they check out into `source/` and use nextest's workspace remapping, exercising relocation of both executable and fixture paths. no Rust build is needed in these consumers. `finally` restores the environment and detaches the VHDX. the transfer archive is retained for one day.

[building and testing](building-and-testing.md#xfs-loop-mount) gives local filesystem reproduction commands. warning assertions count their exact archive hint, and the name-collision check ignores only the known no-link copy warning; FAT/exFAT must still exercise the same cases. capability-sensitive assertions still follow the filesystem's available operations; this matrix is not a promise about every NAS or cloud-file-provider implementation.

## quality

one linux job has separately named checks. each check runs even after an earlier one fails, without `continue-on-error` hiding its result:

| check | tool |
|---|---|
| rust formatting | the pinned rustfmt |
| advisories, licenses, duplicate crates and sources | cargo-deny 0.20.2 with deny.toml |
| workflow security and malformed workflow input | zizmor 1.32.0, offline with strict collection |
| unused dependencies | cargo-machete 0.9.2 with metadata, followed by a Cargo.lock cleanliness check |
| spelling | typos 1.52.0, excluding raw fixtures, snapshots, generated benchmark results and lockfiles |
| shell installer | ShellCheck 0.11.0 with `-s dash` |
| PowerShell installer | PSScriptAnalyzer 1.25.0, warnings/errors except Write-Host progress on supported PowerShell 5.1+ |
| benchmark harness logic | `python3 -m unittest discover -s bench`, using only the standard library |

the benchmark itself never runs here. cargo-deny's three migration notices remain ignored with their existing reasons; duplicate versions warn. machete findings require inspection before removing dependencies. zizmor's offline mode does not audit remote action history. actions remain pinned to full commits with checkout credentials disabled; their monthly Dependabot updates have a seven-day cooldown. cargo minor/patch updates form a separate monthly group with the same cooldown; major updates remain separate, with no auto-merge. the existing scoped crates.io publishing token has a narrow zizmor exception until trusted publishing is configured. rust-cache 2.9.3 keeps dependency builds; PR runs restore but do not save caches.

## installed artifacts

[artifacts.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/artifacts.yml) is the same read-only builder called by CI and release. manual dispatch is a rehearsal: it uploads run artifacts only, with no publishing secrets or write permissions. the old version-only musl jobs are replaced, not duplicated. release tags start without restored build output.

| runner | packaged target | wheel interpreters |
|---|---|---|
| `ubuntu-24.04` | `x86_64-unknown-linux-musl` | 3.15.2 and 3.10.22 |
| `ubuntu-24.04-arm` | `aarch64-unknown-linux-musl` | 3.15.2 |
| `windows-2025` | `x86_64-pc-windows-msvc` | 3.15.2 and 3.10.11 |
| `windows-11-arm` | `aarch64-pc-windows-msvc` | native 3.15.2 |
| `macos-15-intel`, built on `macos-15` | `x86_64-apple-darwin` | the image's 3.14 |
| `macos-15` | `aarch64-apple-darwin` | 3.15.2 |

the arm64 `macos-15` runner cross-builds `x86_64-apple-darwin`, since the same release build takes nearly twice as long on the intel runner; `macos-15-intel` downloads that archive, wheel and binary and runs every check below natively, with no rust toolchain. all six release archives are unpacked, checked for architecture, expected files and the build's executable hash, then run against a stdlib loopback server. [python/ci_journey.py](https://github.com/nikhil-yn-aix/fetchloom/blob/main/python/ci_journey.py) checks downloaded bytes and SHA-256 pins, offline restoration from a shared cache, writable-copy edit preservation, checksum refusal without publication, and archive extraction. it checks the exact version and commit. macos also checks its default cache location, the no-hardlink decision for a recognized CloudStorage path, and loading bash/zsh completions. this folder-name test does not run a real cloud provider. static-musl, Windows CRT and macos system-library/deployment-target checks remain.

unpacked candidates, wheel venvs, source-install trees and receipts live under `.release/`, outside the cached Rust `target/`. a restored build cache must never supply an old installation or a successful receipt. only source-package compilation output is reused, through its explicit target directory.

each wheel is built once. its bundled executable must hash to the checked binary; fresh venvs install that wheel with `--no-index --no-deps` and run `fl --version` (`fl.exe` on Windows) and `python -m fetchloom --version`. the installed helper runs real successful `sync` and `path` calls and a structured failure outside the source checkout, with a decoy global executable on PATH. its selected executable, module path, typed marker, metadata and third-party licenses are checked. the isolated unittest folder receives only `test_helper.py` and its `wheel_scripts.py` support module, never the source `fetchloom` package. Python 3.10 is exercised only on linux and Windows x64; there is no native Windows-arm 3.10 claim. the intel mac uses the newest Python its runner image has, 3.14, since installing 3.15.2 there takes about 90 seconds; the other five targets cover 3.15.2.

installers consume the candidate archives and checksums from loopback through `FETCHLOOM_DOWNLOAD_URL`. linux and both macs run `install.sh`; Windows x64 runs `install.ps1` under both Windows PowerShell 5.1 and PowerShell 7. installations in paths with spaces run the same journey and the installed `fl --version`; Windows explicitly runs `fl.exe` to avoid PowerShell's built-in `fl` alias for Format-List. reinstall, bad checksum, missing checksum entry, non-runnable payload and idempotent PATH updates are checked. Windows restores the raw user PATH and marker, including their registry types, in `finally`. both debs are installed, exercised through `/usr/bin/fetchloom`, then removed.

linux x64 runs the printed `unrar` command, mac arm runs `unar`, and Windows x64 runs the printed absolute 7-Zip command in PowerShell, against a licensed solid-RAR fixture. spaces and an apostrophe in the filename must survive the emitted quoting. independently recorded extracted hashes and the documented destination beside the archive in `data/sample/`, not a reconstruction of the expected command, decide success. reader versions are recorded in the job log.

a separate linux job runs `cargo package --locked --no-verify`, installs from the unpacked `.crate` and exercises that installed program. that release install compiles the packaged sources, so cargo's own verify build would only repeat it. it uses the pinned toolchain, not an additional MSRV matrix. the final inventory requires all six named targets, all required check receipts, the source install, and exactly fourteen package files with matching hashes. it then takes the release sbom from the x86_64 linux wheel with `python/release_sbom.py`, which checks that it is a CycloneDX sbom of this version, and keeps it as the `sbom` artifact. release downloads only `packages-*`; reports cannot leak into published assets. packages stay seven days, as receipts do: the inventory and the release's attestation read them in the same run. [releasing](releasing.md) owns publication.

every apt install in CI and release goes through `.github/scripts/apt-install.sh`, which retries and, when the runner's resolver cannot find Ubuntu's archive, asks public resolvers instead; apt verifies the signed package lists, so the resolver cannot change what installs. an outage of the archive itself still fails provisioning, and the required gate still does not waive that job. each resolver attempt, including update and install together, has a two-minute hard limit and a ten-second forced-stop grace period. both phases and timeout/failure outcomes are logged. a stalled runner-resolver attempt therefore reaches the existing public-resolver fallback without waiting for the whole job timeout; a failed fallback remains red. the CI helper tests use a one-second limit and a deliberately blocked child, without changing this machine's resolver.

## documentation site

the required docs job runs the existing python markdown file, heading and sidebar checks, builds with mdBook 0.5.4, and checks generated HTML links and anchors with lychee 0.24.2 offline. it renders under `site/fetchloom/` so absolute `/fetchloom/` paths resolve against the local site root. missing HTML or local targets fail. external service availability is not part of this offline check.

[python/build_site.py](https://github.com/nikhil-yn-aix/fetchloom/blob/main/python/build_site.py) is the shared builder for CI and [pages.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/pages.yml). it runs mdBook and writes the same `llms.txt`, `llms-full.txt` and skills exports. [book.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/book.toml) and [SUMMARY.md](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/SUMMARY.md) still own the site layout. default preprocessors stay off, so README.md becomes README.html.

pages remains the only deploying workflow. it builds the default branch and adds the signed apt repository from the latest published release before deploying the complete site. PR checks receive no apt key, pages-write or OIDC permission. [releasing](releasing.md#apt-repository) describes that publication boundary.

## scheduled deep checks

[deep.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/deep.yml) runs weekly on Tuesday and accepts manual dispatch. it has no publishing permissions or secrets. dispatch `check=coverage`, `fuzz` or `links` for one lane; `all` runs those three, not mutation testing. no deep job soft-fails, and none runs the benchmark lab.

| check | bound and result |
|---|---|
| linux coverage | cargo-llvm-cov 0.9.1, the pinned Rust with llvm-tools-preview, and nextest 0.9.151; 45-minute job cap |
| all eight [Bolero targets](fuzzing.md#targets) | cargo-bolero 0.13.5, nightly-2026-11-03, five minutes per target with one fuzz worker, 10-second input timeout and 64 KiB maximum input; one job builds once and fuzzes the targets in turn, 18 minutes each at most, the first including the build, under a 75-minute job cap |
| external documentation links | lychee 0.24.2, four concurrent requests, 20-second request timeout and one retry; 15-minute job cap |
| manual mutation pilot | cargo-mutants 27.1.0; one existing `src/**/*.rs` file, at most ten explicitly listed mutants, two workers; lib tests unless dispatched with `tests=all`; each mutant's tests stop at three times the baseline's measured test time, at least 20 seconds; 600-second build timeout, 28-minute command and 35-minute job caps, since a run of ten mutants measured 18 minutes |

coverage has a separate build cache and retains LCOV, JSON and nextest's JUnit for fourteen days. its step summary reports lines, functions and regions, with no percentage threshold. the check requires executed functions in this checkout's `src/main.rs` and `src/lib.rs`: a report without CLI subprocess profiles misses the first, and one whose binary linked an uninstrumented library misses the second, since only the binary calls `fetchloom::main`. nextest builds `recipe_step` in the same instrumented build as the tests. a plain `cargo build` of it first would give the binary that uninstrumented library, because cargo does not rebuild a library when only cargo-llvm-cov's compiler wrapper changes. compare reports only for the same configuration; there is no external uploader or automated historical comparison.

nightly-2026-11-01 cannot compile zvariant_utils because of [Rust regression 165263](https://github.com/rust-lang/rust/issues/165263); the fuzz lane uses the fixed dated nightly above and explicitly selects the GNU target rather than cargo-bolero's build-host default. fuzzing has its own nightly build cache. each target's log and crashes are retained for fourteen days, including on failure, and a failing target does not stop the ones after it. corpus input is not restored or uploaded; each weekly run starts fresh. dependencies are fetched with the lockfile first, then the fuzz run is offline and checks that Cargo.lock stayed unchanged. an outer timeout also bounds compilation plus fuzzing; timeout or a crash remains a failure, never a no-crash success.

mutation testing is manual only: dispatch `check=mutants` and `file=src/cli/sync/place.rs`, or another one-file scope. JSON discovery selects a consecutive slice of at most ten mutants. the tool previews that slice before execution, and its planned and completed inventories must match the saved names. a name regex is not used: cargo-mutants 27.1.0 does not apply it to every struct-field mutation. the unmutated baseline is mandatory, using the ordinary Cargo test runner over the lib tests, or every test target when dispatched with `tests=all` for a file only the binary reaches. the baseline has no time limit of its own, so a slow runner cannot turn a run into one that tests nothing; each mutant's limit follows from the baseline's measured test time. surviving mutants, baseline failures and timeouts stay red. scope, logs and outcomes upload for fourteen days. investigate a missed mutation before deciding whether it warrants a test; do not broaden to the whole crate to chase a score.

external links stay outside PR correctness: an unavailable service can make the weekly check red without implying a product regression. its report is kept in the step summary and run artifacts. the required docs job still checks rendered local links offline.

## live-source checks

[live.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/live.yml) runs the ignored public-service tests weekly on linux, Windows and macos, outside the required result. `--nocapture` keeps credential-dependent skips visible; their lines also appear in the step summary. no credentials are supplied by the workflow. logs are retained for fourteen days, and all jobs have a 45-minute timeout. each job restores the build cache ci's test job keeps for its platform and saves none of its own.

macos additionally runs the small public Zenodo add/restore/get and catalog-search journey in `python/mac_live.py`, with the debug executable the live tests built; every push checks the release executables in the artifact journeys. the standalone mac workflow is removed. its deterministic cache-location, shared-cache, CloudStorage-path, offline-auth and completion checks now belong to the [artifact journey](#installed-artifacts), so they no longer depend on service uptime. live failures need classification as service, configuration or product failures; they are not retried until green.

related: [engineering loop](README.md), [building and testing](building-and-testing.md), [releasing](releasing.md), [placing](../architecture/placing.md).

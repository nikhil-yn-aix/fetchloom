# CI

CI checks native behavior and filesystem paths before merge; release packaging remains a separate workflow.

## triggers and the required check

[ci.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/ci.yml) runs on pushes to main, every pull request, merge groups and manual dispatch. every run is full: there are no path filters or docs-only skips. repository permission is read-only. newer PR commits cancel their predecessors; main and dispatched revisions have separate concurrency groups. matrices do not fail fast.

the one required check is `ci`, with `if: always()` and `needs: [quality, docs, test, windows, windows-fs, snapshots, musl]`. every prerequisite must report `success`; a failure, cancellation or skip fails the gate. require `ci` in branch protection, not individual platform names. no step may turn a failed test into success to get this check green.

## native tests

| runner | coverage |
|---|---|
| `ubuntu-24.04` | x86_64 linux, rustdoc, python tests, btrfs, xfs and a chmod-refusing FAT mount |
| `ubuntu-24.04-arm` | native arm64 linux |
| `windows-2025` | native x86_64 windows on NTFS; produces the test archive for the three extra volumes |
| `windows-11-arm` | native arm64 windows |
| `macos-15` | native arm64 macos on APFS, python tests and HFS+ |

all five install the toolchain pinned in rust-toolchain.toml and run clippy with warnings denied. linux x86_64 also runs `cargo clippy --release --locked -- -D warnings` and `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps`. the mac still checks the intel release target with `cargo check --locked --release --target x86_64-apple-darwin` and deployment target 11.0: that is compilation, not intel execution. it also checks the debug executable's system-library linkage. these checks stay until artifact journeys replace them.

nextest 0.9.151 runs the ordinary unit and CLI tests, including the short Bolero properties. [.config/nextest.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.config/nextest.toml) sets four concurrent tests, one retry and `flaky-result = "fail"`. a test that passes only on retry still fails CI. a slow test is reported after 30 seconds; six periods terminate a hung test as failure. this is a hang guard, not a wall-clock performance assertion. `cargo test --locked --doc` runs doctests separately on every native runner.

`INSTA_UPDATE=no` forbids snapshot updates. insta 1.49.0 records the snapshot paths each run references through `INSTA_SNAPSHOT_REFERENCES_FILE`; the runner converts them to repository-relative reports. the `snapshots` job checks their union against every tracked `.snap` file and rejects unused, unknown or pending snapshots. it needs all native and filesystem jobs: a reflink-only snapshot is legitimately unused on ext4, but must be exercised on a cloning filesystem. this check adds no second test run. review snapshots locally as [building and testing](building-and-testing.md#update-snapshots) describes; CI never accepts them.

`cargo test` still works locally. the recipe step is built before CI starts tests; a test reuses it without invoking cargo. a local cargo test still asks cargo to refresh it once, so edits to the example are not hidden by a stale executable. test executable and fixture paths prefer Cargo's runtime paths, so an archived build can run from a different checkout directory. no test changes the process environment.

## selected-test proof and reports

[python/ci_tests.py](https://github.com/nikhil-yn-aix/fetchloom/blob/main/python/ci_tests.py) compares Cargo's test-binary inventory with nextest's. it asks those libtest binaries for their ordinary and ignored test IDs, both with and without the old filesystem `--skip` arguments. before each nextest run, its selected nonignored IDs must equal that recorded selection and must not be empty. the chmod case has its own exact selection. doctests remain separate from this comparison.

reports under `target/ci` contain the selections and each run's JUnit XML. failures and retry output remain in JUnit; slowest cases also appear in the step summary. reports upload even after a failure, with OS, filesystem and run attempt in their artifact names, and remain for seven days. a successful test run without a JUnit report or with different executed IDs fails the job.

## filesystem runs

every native runner executes the full ordinary suite on its default disk. extra filesystems repeat the existing disk-touching selection, excluding the same substrings the old `DISKLESS` list excluded. a new disk test must not accidentally match one of those exclusions; the selection report makes that decision inspectable.

linux reruns the built tests on btrfs and xfs loop images. their mounted filesystem types are checked first. a separate root-mounted FAT image lets the runner write but refuses chmod; `FETCHLOOM_TEST_NOOP_CHMOD` names it for the one writable-copy regression. macos attaches an HFS+ image for decomposed-unicode and no-reflink behavior. cleanup traps detach the images. macos first tries a normal detach, then force-detaches only its owned test image if it remains busy.

the windows x64 job archives its checked test build once, including `recipe_step` and the libtest selection record. three jobs independently create ReFS, exFAT and FAT32 volumes, verify `Get-Volume` reports the requested filesystem, and point `TMP` and `TEMP` at that volume. each runs the entire disk selection, not one third. they depend only on the windows producer, not the slower linux/mac matrix. they check out into `source/` and use nextest's workspace remapping, exercising relocation of both executable and fixture paths. no Rust build is needed in these consumers. `finally` restores the environment and detaches the VHDX. the transfer archive is retained for one day.

[building and testing](building-and-testing.md#xfs-loop-mount) gives local filesystem reproduction commands. capability-sensitive assertions still follow the filesystem's available operations; this matrix is not a promise about every NAS or cloud-file-provider implementation.

## quality

one linux job has separately named checks. each check runs even after an earlier one fails, without `continue-on-error` hiding its result:

| check | tool |
|---|---|
| rust formatting | the pinned rustfmt |
| advisories, licenses, duplicate crates and sources | cargo-deny 0.20.2 with deny.toml |
| workflow security and malformed workflow input | zizmor 1.32.0, offline with strict collection |
| unused dependencies | cargo-machete 0.9.2 with metadata, followed by a Cargo.lock cleanliness check |
| spelling | typos 1.52.2, excluding raw fixtures, snapshots, generated benchmark results and lockfiles |
| shell installer | ShellCheck 0.11.0 with `-s dash` |
| PowerShell installer | PSScriptAnalyzer 1.25.0, warnings/errors except Write-Host progress on supported PowerShell 5.1+ |
| benchmark harness logic | `python3 -m unittest discover -s bench`, using only the standard library |

the benchmark itself never runs here. cargo-deny's three migration notices remain ignored with their existing reasons; duplicate versions warn. machete findings require inspection before removing dependencies. zizmor's offline mode does not audit remote action history. actions remain pinned to full commits with checkout credentials disabled; their monthly Dependabot updates have a seven-day cooldown. the existing scoped crates.io publishing token has a narrow zizmor exception until trusted publishing is configured. rust-cache 2.9.3 keeps dependency builds; PR runs restore but do not save caches.

## musl checks

two required jobs retain the release-mode musl builds on ubuntu-24.04 and ubuntu-24.04-arm, with musl-tools and mimalloc, then check `--version`. this checks startup, not a release journey. all six target artifacts, installed wheels and installer journeys belong to the next CI slice; this gate does not pretend they already ran. [releasing](releasing.md) owns the current tag workflow and package inventory.

every apt install in CI and release goes through `.github/scripts/apt-install.sh`, which retries and, when the runner's resolver cannot find Ubuntu's archive, asks public resolvers instead; apt verifies the signed package lists, so the resolver cannot change what installs. an outage of the archive itself still fails provisioning, and the required gate still does not waive that job.

## documentation site

the required docs job runs the existing python markdown file, heading and sidebar checks, builds with mdBook 0.5.4, and checks generated HTML links and anchors with lychee 0.24.2 offline. it renders under `site/fetchloom/` so absolute `/fetchloom/` paths resolve against the local site root. missing HTML or local targets fail. external service availability is not part of this offline check.

[python/build_site.py](https://github.com/nikhil-yn-aix/fetchloom/blob/main/python/build_site.py) is the shared builder for CI and [pages.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/pages.yml). it runs mdBook and writes the same `llms.txt`, `llms-full.txt` and skills exports. [book.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/book.toml) and [SUMMARY.md](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/SUMMARY.md) still own the site layout. default preprocessors stay off, so README.md becomes README.html.

pages remains the only deploying workflow. it builds the default branch and adds the signed apt repository from the latest published release before deploying the complete site. PR checks receive no apt key, pages-write or OIDC permission. [releasing](releasing.md#apt-repository) describes that publication boundary.

## live-source checks

[live.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/live.yml) still runs the ignored public-service tests weekly on linux and windows, outside the required result. [mac-e2e.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/mac-e2e.yml) remains manually dispatched until its unique checks are moved into deterministic artifact and weekly live journeys. neither is replaced by this slice.

related: [engineering loop](README.md), [building and testing](building-and-testing.md), [releasing](releasing.md), [placing](../architecture/placing.md).

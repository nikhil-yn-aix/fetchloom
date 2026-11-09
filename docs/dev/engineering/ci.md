# CI

CI checks the same crate across processor architectures and filesystems that expose different storage behavior.

## triggers and permissions

[ci.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/ci.yml) runs on pushes to `main`, pull requests and manual dispatch. it has read-only repository permission. concurrency is grouped by ref, with older in-progress runs cancelled. the matrix does not fail fast, so another platform can still report its result. the summary job `ci` has `needs: [test, musl]` and `if: always()`. it passes only when both results are `success`, so failed, cancelled or skipped prerequisites cannot appear green. require the single `ci` check in branch protection rather than individual platform names.

## test job

one `test` job expands to five required runners, each with a 45-minute timeout. every runner installs the pinned rust toolchain, restores the rust cache, runs clippy with warnings denied and runs all rust tests.

| runner | architecture | why it runs |
|---|---|---|
| `ubuntu-latest` | x86_64 linux | default linux behavior, shared formatting and dependency checks, python helper and extra linux filesystems |
| `windows-latest` | x86_64 windows | windows handles, permissions, path behavior and extra windows filesystems |
| `ubuntu-24.04-arm` | arm64 linux | native arm64 compilation and behavior before shipping its linux release |
| `windows-11-arm` | arm64 windows | native arm64 windows behavior before shipping its release |
| `macos-latest` | arm64 macos | apfs clones, a disk that ignores case and unicode composition, the python helper and an extra hfs+ run |

## checks within the job

| check | runner | purpose |
|---|---|---|
| `cargo fmt --check` | x86_64 linux | one formatting result for platform-independent source |
| `cargo clippy --all-targets --locked -- -D warnings` | all five | every target and test must satisfy configured lints |
| `cargo clippy --release --locked -- -D warnings` | x86_64 linux | release-only code and items used only by debug builds must satisfy the lints |
| `cargo check --locked --release --target x86_64-apple-darwin` | macos | the x86_64 macos release target, C dependencies included, which the arm64 runner otherwise never compiles; `MACOSX_DEPLOYMENT_TARGET` is 11.0 as in the release |
| `cargo test --locked` | all five | unit, CLI, documentation and short fuzz tests |
| `cargo deny --locked check advisories licenses bans sources` | x86_64 linux | one policy check for the shared lockfile; CI installs cargo-deny 0.20.2 |
| `python3 -m unittest discover -s python` | x86_64 linux, x86_64 windows and macos | subprocess-wrapper arguments and report handling |
| `python3 -m unittest discover -s bench` | x86_64 linux | benchmark harness: fixtures, runs, live comparison and report rendering |
| disk-touching tests on ReFS, exFAT and FAT32 | x86_64 windows | block cloning, absence of links and read-only removal behavior |
| disk-touching tests on btrfs and xfs | x86_64 linux | reflink behavior absent from the default ext4 filesystem |
| disk-touching tests on hfs+ | macos | a disk that lists every name decomposed and cannot clone files |

[deny.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/deny.toml) rejects open advisories, licenses outside its allowlist and sources outside crates.io. duplicate crate versions warn. three advisories are ignored, each with its reason: zvariant, zvariant_derive and zbus_names were folded into zbus 6, which keyring's linux secret service store does not use yet. they are notices that the crates moved, not flaws, and go once keyring moves.

## musl job

the linux release ships `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, static binaries that use mimalloc in place of musl's allocator for every allocation, libc's own too, while the `test` job builds for glibc. the `musl` job builds each of those targets on its own architecture, `x86_64-unknown-linux-musl` on `ubuntu-latest` and `aarch64-unknown-linux-musl` on `ubuntu-24.04-arm`, the way [release.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/release.yml) does: it installs musl-tools for the C dependencies and runs `cargo build --locked --release` for the target. its runs are named after the targets and do not fail fast. it then checks that the binary's `--version` prints `fetchloom` and the Cargo version. it has a 45-minute timeout and restores the rust cache. it does not run the tests or the release workflow's linkage checks.

## filesystem runs

windows creates 4096 MB expandable VHDX images with diskpart and assigns R, X and Y. it points `TMP` and `TEMP` at a folder on each volume and runs `cargo test --locked --no-fail-fast`. runner admin access permits diskpart; normal contributor tests should not require admin access.

linux installs btrfs and xfs utilities, creates 4G images, mounts them with loop devices and makes each mount writable by the runner. `TMPDIR` routes test-created folders there. every filesystem result is collected and any failure fails the job. arm64 linux and windows runners run the ordinary full tests, not these extra mount loops.

macos makes a 4 GB hfs+ disk image with `hdiutil`, attaches it without showing it in finder and points `TMPDIR` at it. apfs, the default disk, keeps each name in the unicode composition it was given, while hfs+ lists every name decomposed; both ignore case by default. tests that need such a disk return early on a disk that tells the spellings apart.

[building and testing](building-and-testing.md#xfs-loop-mount) gives a focused xfs reproduction. its [WSL gate](building-and-testing.md#wsl-gate) verifies a local branch without a push.

## live-source checks

[live.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/live.yml) runs every monday at 04:17 UTC and through manual dispatch. its linux x86_64 and windows x86_64 jobs run:

```sh
cargo test --locked -- --ignored
```

this exercises the ignored live source tests. jobs have 45-minute timeouts and do not fail fast. the workflow is separate from the required `ci` summary because public services can change or be unavailable.

default CI uses recorded metadata and local servers; work counters catch extra requests and copies. there is no wall-clock performance gate. action references are pinned to commits in every workflow.

[mac-e2e.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/mac-e2e.yml) runs only through manual dispatch. on `macos-latest` it builds a release executable and runs it as a person would against live zenodo and the search catalogs: `add zenodo:3608990`, `status`, `sync` after deleting `data/`, `get -o`, `search iris --json`, a second project that must clone or hardlink from the shared cache, a project in `~/Library/CloudStorage` that must not hardlink, the default cache in `~/Library/Caches/fetchloom`, `auth --offline`, and the bash and zsh completion scripts. every check runs even after one fails, and the job uploads everything the commands printed as the `mac-e2e-log` artifact.

## documentation site

[pages.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/pages.yml) publishes `docs/` as an [mdBook](https://rust-lang.github.io/mdBook/) site at `https://nikhil-yn-aix.github.io/fetchloom/`, so the site always matches the markdown on main. it runs on pushes to main that change `docs/**`, `book.toml`, `llms.txt`, `skills/**` or the workflow, when a release is published and through manual dispatch. it checks out the default branch even when a release tag starts it. the job has contents read, pages write and id-token write, a 15-minute timeout, the `github-pages` environment and the `pages` concurrency group, which lets a running deployment finish.

the job downloads mdbook 0.5.4 from its github release, checks the archive's SHA-256 and runs `mdbook build`. [book.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/book.toml) reads `docs/` and writes `site/`. [SUMMARY.md](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/SUMMARY.md) sets the sidebar order; a page missing from it is not published, and a listed page that does not exist fails the build. default preprocessors are off, so each `README.md` becomes `README.html` and links between pages keep working. links to files outside `docs/` use absolute github URLs because the site cannot serve them.

| site path | source |
|---|---|
| `/` | the mdbook output, with `index.html` rendered from `docs/README.md` |
| `/llms.txt` | `llms.txt` from the repository root, with links pointing at site pages or github and a line naming `llms-full.txt` |
| `/llms-full.txt` | every user guide page in `SUMMARY.md` order, each preceded by its site URL |
| `/skills/` | the `skills/` folder, unchanged |
| `/apt/` | the signed apt repository from the latest published release; [releasing](releasing.md#apt-repository) describes it |

preview the site from the repository root with mdbook 0.5.4 installed: `mdbook serve --open`. `mdbook build` writes the same files to `site/`, which git ignores.

related: [engineering loop](README.md), [releasing](releasing.md), [performance](../performance/README.md), [placing](../architecture/placing.md).

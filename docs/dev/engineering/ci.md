# CI

CI checks the same crate across processor architectures and filesystems that expose different storage behavior.

## triggers and permissions

[ci.yml](../../../.github/workflows/ci.yml) runs on pushes to `main` and `rebuild`, pull requests and manual dispatch. it has read-only repository permission. concurrency is grouped by ref, with older in-progress runs cancelled. the matrix does not fail fast, so another platform can still report its result.

## test job

one `test` job expands to four required runners, each with a 45-minute timeout. every runner installs the pinned rust toolchain, restores the rust cache, runs clippy with warnings denied and runs all rust tests.

| runner | architecture | why it runs |
|---|---|---|
| `ubuntu-latest` | x86_64 linux | default linux behavior, shared formatting and dependency checks, python helper and extra linux filesystems |
| `windows-latest` | x86_64 windows | windows handles, permissions, path behavior and extra windows filesystems |
| `ubuntu-24.04-arm` | arm64 linux | native arm64 compilation and behavior before shipping its linux release |
| `windows-11-arm` | arm64 windows | native arm64 windows behavior before shipping its release |

## checks within the job

| check | runner | purpose |
|---|---|---|
| `cargo fmt --check` | x86_64 linux | one formatting result for platform-independent source |
| `cargo clippy --all-targets --locked -- -D warnings` | all four | every target and test must satisfy configured lints |
| `cargo test --locked` | all four | unit, CLI, documentation and short fuzz tests |
| `cargo deny --locked check advisories licenses bans sources` | x86_64 linux | one policy check for the shared lockfile; CI installs cargo-deny 0.20.2 |
| `python3 -m unittest discover -s python` | x86_64 linux | subprocess-wrapper arguments and report handling |
| full tests on ReFS, exFAT and FAT32 | x86_64 windows | block cloning, absence of links and read-only removal behavior |
| full tests on btrfs and xfs | x86_64 linux | reflink behavior absent from the default ext4 filesystem |

[deny.toml](../../../deny.toml) rejects open advisories, licenses outside its allowlist and sources outside crates.io. duplicate crate versions warn. the advisory ignore list is empty; changes to exceptions belong with their reasons.

## filesystem runs

windows creates 4096 MB expandable VHDX images with diskpart and assigns R, X and Y. it points `TMP` and `TEMP` at a folder on each volume and runs `cargo test --locked --no-fail-fast`. runner admin access permits diskpart; normal contributor tests should not require admin access.

linux installs btrfs and xfs utilities, creates 4G images, mounts them with loop devices and makes each mount writable by the runner. `TMPDIR` routes test-created folders there. every filesystem result is collected and any failure fails the job. arm64 runners run the ordinary full tests, not these extra mount loops.

[building and testing](building-and-testing.md#xfs-loop-mount) gives a focused xfs reproduction. its [WSL gate](building-and-testing.md#wsl-gate) verifies a local branch without a push.

there are no nightly live-source checks or wall-clock performance gates. tests use recorded metadata and local servers; work counters catch extra requests and copies. action references are pinned to commits in the workflow.

related: [engineering loop](README.md), [releasing](releasing.md), [performance](../performance/README.md), [placing](../architecture/placing.md).

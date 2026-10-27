# engineering

## code

- rust, edition 2024, on the stable toolchain pinned in `rust-toolchain.toml`.
- one crate with one binary. modules follow [architecture](architecture.md). a module becomes its own crate only when a second consumer needs it.
- `unsafe` is forbidden.
- no `unwrap`, `expect` or `panic!` outside tests. errors are typed with `thiserror` and rendered in one place.
- clippy runs with `-D warnings` and the pedantic group. any lint we allow sits in a short list with its reason.
- files stay under about 400 lines and functions under about 60. past that, split by responsibility.
- no dead code, no unused dependency, no feature flag nothing uses, no compatibility shim. replaced code is deleted in the same change.
- comments, docstrings and names follow [style](style.md).
- a new dependency names its reason in the commit, passes `cargo deny`, and is the smallest crate that does the job well.

## design rules

- a pure core with IO at the edges. parsing, planning and comparing are plain functions over data, tested without a network or a disk.
- parse, don't validate. a reference, a digest, a dataset path or a URL becomes a type once, where it enters, and stays valid after.
- a trait only where there are two real implementations or a test needs a seam: the clock and the network.
- one way to do each thing, and no option without a user who needs it.

## tests

tests follow FIRST:

- fast. the default suite runs in under a minute, and a test over one second has a reason.
- independent. each test gets its own temp folder, cache and home directory, and reads nothing from the developer's machine.
- repeatable. the default suite never touches the live network. sources are served from recorded responses by a local fake server, and time and randomness are injected.
- self-validating. every test asserts. CLI output is checked with snapshots, with paths and durations filtered out.
- timely. a test is written with its code, first when the behavior is already specified. a bug fix starts with a test that fails for the reported reason.

| kind | covers | runs |
|---|---|---|
| unit | pure functions and types | every commit |
| property | parsers, lock round trips, path rules, planning | every commit |
| end to end | the real binary against a fake server, one test per scenario in [scope](scope.md#done-means) | every commit, three platforms |
| conformance | every source against the same recorded cases: a small dataset, a paged listing, a missing dataset, a rate limit, a redirect, a wrong checksum | every commit |
| counters | requests, bytes and file operations for each scenario | every commit |
| live | each source once against the real service | nightly, reports and never blocks |
| fuzz | `data.toml`, `data.lock`, archive readers, what sources send back | before each release and whenever a parser changes |

- one test per behavior, at the lowest level that shows it. nothing the type system already guarantees, nothing another test already covers.
- a flaky test is a bug, fixed before merge. it is never retried until green.
- mutation testing with cargo-mutants runs on core modules before a release to find tests that pass no matter what. it never blocks a merge.
- coverage is looked at, never gated.

## tools

| job | tool |
|---|---|
| tests | cargo-nextest with `--no-fail-fast` |
| snapshots | insta |
| fake servers | wiremock |
| property tests | proptest |
| fuzzing | cargo-fuzz, on linux |
| lint and format | clippy, rustfmt |
| dependencies | cargo-deny for licenses, advisories and sources, cargo-shear for unused crates |
| spelling and prose | typos, vale |
| tasks | a justfile, so local runs and CI run the same commands |
| python | ruff, pytest against the built wheel |

## CI

- every pull request runs format, lint, prose, deny and the whole test suite on windows, macos and linux, in under 10 minutes.
- every night, live source checks and the advisory database run. a failure opens one issue, or comments on the one already open.
- before a release: fuzzing, mutation testing of core modules, every budget measured.
- workflow actions are pinned by commit SHA and checked with zizmor.
- a platform counts as supported only when its tests run in CI.

## release

- one version, set in `Cargo.toml`. a `v*` tag starts two workflows, the way uv and ruff ship:
  - dist builds binaries for windows, macos and linux with shell and powershell installers, attaches them to the github release, and attests each build.
  - maturin builds wheels that carry the binary and a small python module, published to PyPI through trusted publishing.
- `data.lock` and the JSON document carry a format version. before 1.0, a format change bumps it and an older file is refused with the command that rebuilds it.
- release notes come from commit messages, which is why those are written for people.

## license

apache-2.0, with a NOTICE that names fetchloom, Nikhil Y N and KairosLab India Private Limited. contributions come in under the same license. the reasons are in [decisions](decisions.md#26-apache-20).

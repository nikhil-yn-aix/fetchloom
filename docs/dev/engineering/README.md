# engineering

start with the smallest test that demonstrates your change, then widen coverage before merging.

## the fast loop

for a bug, add a failing regression test at the lowest layer that shows it. run that exact test, make the change, then run its module. use a CLI case when exit status, stdout, files left on disk or recovery across commands is part of the behavior.

```sh
cargo test --locked --lib threads::tests::archives_share_the_cap_between_readers_and_writers -- --exact
cargo test --locked --lib threads::
cargo test --locked --test cli caps::
```

choose the corresponding module from the [architecture pages](../architecture/README.md#components). a substring filter can accidentally select no tests; check the runner's selected-test count.

at a milestone, before merging, or after changes across command boundaries, run the full gate:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

## commands

| command | when to use it |
|---|---|
| `cargo build --locked` | first build or executable smoke test |
| `cargo run --locked -- --version` | check executable dispatch |
| `cargo test --locked --lib source::zenodo::` | one adapter's parsing and recorded responses |
| `cargo test --locked --lib archive::` | decoder, guard or staging changes |
| `cargo test --locked --test cli placing::` | placement behavior through the executable |
| `cargo test --locked --test cli` | changes that need the full command scenario suite |
| `cargo test --locked --doc` | runnable rust documentation examples |
| `cargo fmt --all -- --check` | formatting gate |
| `cargo clippy --all-targets --locked -- -D warnings` | warnings and configured rust/clippy lints |
| `cargo test --locked` | full rust gate, including short fuzz runs |
| `cargo deny --locked check advisories licenses bans sources` | dependency changes and the CI dependency policy |
| `python3 -m unittest discover -s python` | python wrapper arguments and report handling; use `py -3` on windows |
| `cargo test --locked -- --ignored` | live service checks and the larger archive test; [details](building-and-testing.md#python-and-live-tests) |
| `cargo test --locked --lib fuzz_` | parser fuzz properties in their ordinary test mode |

## workflows

| page | covers |
|---|---|
| [building and testing](building-and-testing.md) | toolchain, test layers, WSL and filesystem reproduction |
| [running benchmarks](benchmarks.md) | linux lab setup, smoke runs, full comparisons and result records |
| [profiling](profiling.md) | [reported-problem reproduction](profiling.md#reproduce-a-reported-problem), work counters, traces, CPU samples and memory |
| [fuzzing](fuzzing.md) | parser targets and longer linux runs |
| [CI](ci.md) | required platforms, filesystems, dependency policy and python tests |
| [releasing](releasing.md) | artifacts, installers, wheels and publication |

[style](../style.md) covers prose and commits. [decisions](../decisions.md) records boundaries that a change should preserve. [performance](../performance/README.md) records evidence for changes that claim a speed or memory improvement.

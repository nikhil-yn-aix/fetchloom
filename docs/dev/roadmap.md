# roadmap

pending work concerns remaining performance costs and publishing tested releases.

## measurement work

1. remeasure the historical windows warm-cache [budget miss](performance/budgets.md) on native filesystems before choosing another optimization. the [native checks](performance/native-checks.md) cover placement, durability and resource use; linux results do not establish native latency.
2. investigate retained allocations and filesystem costs with the [profiling workflow](engineering/profiling.md). use the [recorded experiments](performance/measurements.md) and [defender study](performance/windows-defender.md) to avoid repeating discarded changes without new evidence.
3. run the implemented [benchmark suite](performance/benchmark-design.md) in full, retain its raw records and publish the generated results.

## publishing

1. check that a source install from the default branch builds the release. exercise binaries, installers and wheels with the maintainer and two or three researchers on real projects before publishing 0.1.0 through the [release workflow](engineering/releasing.md).
2. after the first crates.io release, replace the `CARGO_REGISTRY_TOKEN` secret with crates.io trusted publishing for [publish.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/publish.yml).
3. sign the windows binaries. defender's ransomware rule blocked an unsigned release executable on a managed windows machine.

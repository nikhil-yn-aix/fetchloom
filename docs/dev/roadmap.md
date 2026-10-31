# roadmap

pending work concerns remaining performance costs and publishing tested releases.

## pending code

1. close the remaining [budget misses](performance/budgets.md): warm-cache placement on windows under load and large-project memory on linux.
2. investigate retained allocations and filesystem costs with the [profiling workflow](engineering/profiling.md). use the [recorded experiments](performance/measurements.md) and [defender study](performance/windows-defender.md) to avoid repeating discarded changes without new evidence.
3. build and run the [benchmark suite](performance/benchmark-design.md), then publish its results.

## publishing

1. check that a source install from the default branch builds the release. exercise binaries, installers and wheels with the maintainer and two or three researchers on real projects before publishing 0.1.0 through the [release workflow](engineering/releasing.md).
2. upload the first wheels to PyPI manually, then configure a trusted publisher for [release.yml](../../.github/workflows/release.yml).
3. sign the windows binaries. defender's ransomware rule blocked an unsigned release executable on a managed windows machine.

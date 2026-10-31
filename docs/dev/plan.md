# plan

## pending code

1. flush copied payloads before writing the state that vouches for them, if measurements show the cost is small. [placement](architecture.md#place) records the current durability gap; change [place.rs](../../src/place.rs) and cover the ordering through [sync placement](../../src/cli/sync/place.rs).
2. close the remaining [budget misses](performance.md#budgets): large local downloads, warm-cache placement on windows under load, and large-project memory on linux. use the [memory measurements](performance.md#memory) and [windows scan measurements](performance.md#small-files-on-windows) to avoid repeating discarded changes.

## publishing

1. check that a source install from the default branch builds the release. exercise the built binaries, installers and wheels with the maintainer and two or three researchers on real projects before publishing 0.1.0 through [releases](engineering.md#releases).
2. upload the first wheels to PyPI by hand, then configure a trusted publisher for [release.yml](../../.github/workflows/release.yml).
3. sign the windows binaries. on a managed windows machine, defender's ransomware rule blocked an unsigned release `fl.exe` while `fetchloom.exe` ran.

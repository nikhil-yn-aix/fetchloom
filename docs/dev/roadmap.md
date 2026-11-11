# roadmap

pending work covers the features planned for 0.1.0, remaining performance costs and publishing tested releases.

## features

each line goes when its feature lands, in the same change that documents it in the [user guide](../user/README.md).

1. `fetchloom get <link> -o <file>` writes one writable, verified file and never replaces an existing one without `--force`; python gains `fetchloom.get`.
2. `fetchloom sync <names>` and `fetchloom path <name>` sync only the named datasets.
3. `path = "."` places a dataset at the root of the data folder, and datasets share a folder when none of their files clash.
4. `fetchloom export` writes SHA-256 sums, links or an aria2 input for placed files and source objects, and `list --json` gives each file's SHA-256 and link.
5. `fetchloom lock` resolves a conflicted `data.toml` and `data.lock` offline, with `--ours` or `--theirs` for two changes to one dataset.
6. `fetchloom add ./folder` pins local bytes, and a list of links with their SHA-256 imports in one step.
7. recipes: datasets a command makes from other datasets, with `fetchloom build` and its `--check`, `--accept`, `--prepare` and `--locked`, and a peak-space preview.
8. mirrors: reading from mirrors in settings or `--mirror`, a mirror-only mode and `fetchloom push` to a folder mirror.
9. `cache gc --inputs` frees inputs that a verified mirror holds, and `fetchloom cache` estimates needed, unneeded, linked and copied bytes.
10. member filters for archives and `fetchloom drop <path>`.
11. private S3 buckets, with keys from the environment or shared profiles, and S3-compatible hosts.
12. rar after a decoder trial, a hint for shards of numbered `.tar` files and a refusal before copying a file too big for FAT32.
13. a migration and exit guide that says what carries over from dvc, git-lfs and datalad, and what does not.

deferred until a need shows: a data root on another disk, a link mode for build inputs, finding results across projects, a cache budget, STS and `credential_process` for S3, signed google cloud storage, azure and FTP, split archives beyond plain byte splits, adopting the hugging face cache and relinking copies to the cache.

## measurement work

1. remeasure the historical windows warm-cache [budget miss](performance/budgets.md) on native filesystems before choosing another optimization. the [native checks](performance/native-checks.md) cover placement, durability and resource use; linux results do not establish native latency.
2. investigate retained allocations and filesystem costs with the [profiling workflow](engineering/profiling.md). use the [recorded experiments](performance/measurements.md) and [defender study](performance/windows-defender.md) to avoid repeating discarded changes without new evidence.
3. run the implemented [benchmark suite](performance/benchmark-design.md) in full, retain its raw records and publish the generated results.

## publishing

1. check that a source install from the default branch builds the release. exercise binaries, installers and wheels with the maintainer and two or three researchers on real projects before publishing 0.1.0 through the [release workflow](engineering/releasing.md).
2. after the first crates.io release, replace the `CARGO_REGISTRY_TOKEN` secret with crates.io trusted publishing for [publish.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/publish.yml).
3. sign the windows binaries. defender's ransomware rule blocked an unsigned release executable on a managed windows machine.

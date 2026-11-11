# roadmap

pending work covers the features planned for 0.1.0, remaining performance costs and publishing tested releases.

## features

each line goes when its feature lands, in the same change that documents it in the [user guide](../user/README.md).

1. recipes beyond running them: `build --check`, `--accept`, `--prepare` and `--locked`, `place = false`, a peak-space preview and a disk floor, SIGTERM handled as ctrl-c, gc of results nothing pins, and job objects that end everything a step started on windows.
2. `cache gc --inputs` frees inputs that a verified mirror holds.
3. archives split into numbered or lettered parts, such as `scans.zip.001` or `train.tar.gz.aa`, unpack as one archive.
4. the [other tools](../user/other-tools.md) guide says what carries over from dvc, git-lfs and datalad, and what does not.

deferred until a need shows: a data root on another disk, a link mode for build inputs, finding results across projects, a cache budget, STS and `credential_process` for S3, signed google cloud storage, azure and FTP, split archives beyond plain byte splits, adopting the hugging face cache and relinking copies to the cache.

## measurement work

1. remeasure the historical windows warm-cache [budget miss](performance/budgets.md) on native filesystems before choosing another optimization. the [native checks](performance/native-checks.md) cover placement, durability and resource use; linux results do not establish native latency.
2. investigate retained allocations and filesystem costs with the [profiling workflow](engineering/profiling.md). use the [recorded experiments](performance/measurements.md) and [defender study](performance/windows-defender.md) to avoid repeating discarded changes without new evidence.
3. run the implemented [benchmark suite](performance/benchmark-design.md) in full, retain its raw records and publish the generated results.

## publishing

1. check that a source install from the default branch builds the release. exercise binaries, installers and wheels with the maintainer and two or three researchers on real projects before publishing 0.1.0 through the [release workflow](engineering/releasing.md).
2. after the first crates.io release, replace the `CARGO_REGISTRY_TOKEN` secret with crates.io trusted publishing for [publish.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/publish.yml).
3. sign the windows binaries. defender's ransomware rule blocked an unsigned release executable on a managed windows machine.

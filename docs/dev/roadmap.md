# roadmap

pending work covers the features planned for 0.1.0, remaining performance costs and publishing tested releases.

## features

every feature planned for 0.1.0 has landed. a new one gets a line here, which goes in the same change that documents it in the [user guide](../user/README.md).

deferred until a need shows: a data root on another disk, a link mode for build inputs, finding results across projects, a cache budget, AWS keys from instance and container metadata or a web identity outside a profile, google credentials from a metadata server, FTP, split archives beyond plain byte splits, adopting the hugging face cache, relinking copies to the cache, and letting `cache gc --inputs` free raw input whose source lists its pinned hash, with no mirror.

## speed after 0.1.0

both were measured and left out of 0.1.0 for a reason other than speed.

1. hardlinks before reflinks, as a choice a person makes on a filesystem that clones, for the slow unpack on xfs [issue 35](https://github.com/nikhil-yn-aix/fetchloom/issues/35) reports. on xfs it took a sync of a zip of 10,000 small files to 0.776 of the time and placing 10,000 cached files to 0.358, but a hardlink is the cache's object, so an edit to a placed file reaches the cache and every project; the [placement order](decisions.md#storage-and-requests) keeps reflinks first for that isolation ([measurements](performance/measurements.md#hardlinks-before-reflinks-on-xfs-2026-11-13)).
2. a committed profile for profile-guided optimization. it made manifest-heavy commands 5 to 10% faster and the musl binary 2.5 MiB smaller, short of the 10% bar, and a profile trained in the release job would keep a rebuild from giving the same bytes, so it needs a profile committed for each of the six release targets and trained again as the code moves ([measurements](performance/measurements.md#profile-guided-optimization-2026-11-13)).

## measurement work

1. remeasure the historical windows warm-cache [budget miss](performance/budgets.md) on native filesystems before choosing another optimization. the [native checks](performance/native-checks.md) cover placement, durability and resource use; linux results do not establish native latency.
2. investigate retained allocations and filesystem costs with the [profiling workflow](engineering/profiling.md). use the [recorded experiments](performance/measurements.md) and [defender study](performance/windows-defender.md) to avoid repeating discarded changes without new evidence.
3. run the implemented [benchmark suite](performance/benchmark-design.md) in full, retain its raw records and publish the generated results.

## publishing

1. check that a source install from the default branch builds the release. exercise binaries, installers and wheels with the maintainer and two or three researchers on real projects before publishing 0.1.0 through the [release workflow](engineering/releasing.md).
2. after the first crates.io release, replace the `CARGO_REGISTRY_TOKEN` secret with crates.io trusted publishing for [publish.yml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/.github/workflows/publish.yml).
3. sign the windows binaries, and sign and notarize the macos ones, which needs the owner's apple developer id and a windows code-signing certificate, as [releasing](engineering/releasing.md) says. defender's ransomware rule blocked an unsigned release executable on a managed windows machine, and a macos binary downloaded in a browser carries the quarantine flag.

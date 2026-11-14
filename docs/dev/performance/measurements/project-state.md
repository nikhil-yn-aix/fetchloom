# Project state

## a/b passes from a profile of nine jobs, 2026-11-12

profiling nine everyday jobs against the plain tools people use for them turned up eight changes. each ran behind a switch in one release build, against the same build with the switch off, every arm once a round in a seeded shuffled order, at least ten measured rounds a case, on ext4, xfs and btrfs over nvme with a local server; the static musl build unless glibc is named. a ratio is the median of new over old with its 95% paired bootstrap interval from `bench/ab.py`. a change stayed only if an interval where it aimed ended below 0.97, no run failed and no measured case's interval started above 1.02; the changes to how the state is read and to the step's wake were also held to no rise in peak memory. the machine rebooted midway, and no pair mixes runs from before and after it.

| change | result | outcome |
|---|---|---|
| a hashing read's buffer sized to the file, up to 1 MiB, rather than 1 MiB for every file | importing 100,000 files 0.812 [0.787, 0.823], on glibc 0.939 [0.933, 0.962]; `cache verify` of 100,000 objects 0.784 [0.725, 0.826]; hashing 8 GiB, a 1 GiB download, 10,000 small downloads and a zip of 10,000 members flat | kept |
| hashing read buffers started at a page | `cache verify` of 8 GiB on glibc 0.697 [0.693, 0.699] warm and 0.949 [0.944, 0.954] cold; flat on musl, whose allocator already starts them at a page | kept |
| a large download written to the disk while it arrives, every 32 MiB after its first 64 MiB | `get -o` of 10 GiB 0.531 [0.530, 0.540], at 500 MB/s 0.711 [0.704, 0.727]; 1 GiB 0.553 on ext4 and 0.913 on xfs; a resumed download 0.738; 1,000 and 10,000 small downloads flat | kept |
| a step's exit wakes the build's watch | a recipe that writes 10,000 files 0.889 [0.886, 0.896], but on musl peak memory 1.061 [1.058, 1.078], with the step's output threads joined first or not; on glibc 1.004 [0.999, 1.009] | dropped |
| the state's map built in one pass from a list of its files | a settled sync of 100,000 files 0.784 [0.771, 0.791], but peak memory 1.148 [1.130, 1.185] | dropped |
| the same map built in chunks of 4,096 or 1,024 files that merge | a settled sync of 100,000 files 0.85 with no more memory, but on musl peak memory 1.06 at 10,000 files and 1.05 for `status` of 100,000; on glibc 1,000,000 files 0.997 and 1.017 | dropped |
| one pool of scan workers for the whole walk | a settled sync of 100,000 files 0.902 [0.890, 0.917], but of 10,000 1.052 [1.046, 1.063] | dropped |
| a push's unfinished files spread over 16 folders | a first push of 100,000 files 0.729 [0.725, 0.748], but a push of 50 files 1.046 [1.034, 1.060] | dropped |
| a new object published on btrfs by one rename | a zip on btrfs 0.970 [0.956, 0.986] | dropped, no win |

with the three kept changes and the step's wake on together, against all off: importing 100,000 files 0.803 [0.797, 0.813], `cache verify` of 8 GiB on glibc 0.691 [0.686, 0.693], `get -o` of 10 GiB 0.553 [0.530, 0.587] and of 1 GiB 0.553; 10,000 small downloads, a settled sync of 100,000 files and `status` of 3 stayed flat.

## an indexed project state, 2026-11-12

the state is now one json document whose records are cut into blocks of 64 KiB, each block's first path and byte range listed after them, and where the records end written in its first 48 bytes ([state reading](../../architecture/project.md#state-reading)). a read within the named folders, as a ready `path` and a settled `sync <name>` make, reads those 48 bytes, what follows the records and the blocks that can hold paths in the folders: 81,422 of 24,719,656 bytes for `path tiny` beside 100,000 records, in the `read_state` span of a trace, where the release before read all 24,700,438. whole reads parse the document as before. the version went to 2, so a version 1 state counts as empty.

builds: the release builds of efbf3adf and of the change, glibc and musl, one build per arm. each arm had its own copy of every fixture: the base's state in version 1, the change's the same records rewritten into version 2 by a script that rewrote a 3,000-file project's state byte for byte as the change's own writer did, both states with the same modification time. on tmpfs: scale `named-100000`, a 1-file dataset beside 100,000 records of a dataset whose files are not on disk, and `files-100000` with its cache. on ext4: hardlinked trees of scale `files-1000001` with the 1-file dataset added. every batch held the measure lock on cores 12-23, ran a warmup round and then rounds with every arm once in a seeded shuffled order; peak memory is gnu time's `%M`; a ratio is the median of change over base with its 95% paired bootstrap interval from `bench/ab.py`. the change stays if its target intervals end below 0.90 and no other interval starts above 1.02.

| case | libc | n | median, base to change | time ratio [95%] | peak memory, base to change | memory ratio [95%] |
|---|---|---:|---|---|---|---|
| `path tiny` beside 100,000 records | glibc | 30 | 55.8 to 5.1 ms | 0.090 [0.089, 0.091] | 32.2 to 8.9 MiB | 0.279 [0.276, 0.281] |
| | musl | 30 | 34.6 to 5.0 ms | 0.144 [0.138, 0.147] | 36.5 to 14.5 MiB | 0.396 [0.395, 0.398] |
| settled `sync tiny` beside 100,000 records | glibc | 30 | 56.7 to 5.4 ms | 0.094 [0.093, 0.097] | 32.4 to 9.8 MiB | 0.302 [0.301, 0.304] |
| | musl | 30 | 34.8 to 5.2 ms | 0.148 [0.144, 0.153] | 37.1 to 15.1 MiB | 0.406 [0.405, 0.408] |
| `path tiny` beside 1,000,001 records | glibc | 20 | 464.3 to 5.9 ms | 0.013 [0.013, 0.013] | 243.3 to 9.2 MiB | 0.038 [0.037, 0.038] |
| | musl | 20 | 258.6 to 5.4 ms | 0.021 [0.020, 0.022] | 248.5 to 16.5 MiB | 0.066 [0.066, 0.067] |
| settled `sync tiny` beside 1,000,001 records | glibc | 20 | 468.0 to 7.0 ms | 0.015 [0.014, 0.016] | 243.7 to 9.8 MiB | 0.040 [0.040, 0.040] |
| | musl | 20 | 261.6 to 6.5 ms | 0.024 [0.023, 0.026] | 251.1 to 17.1 MiB | 0.068 [0.068, 0.069] |
| `--json status`, 100,000 files | glibc | 20 | 432.4 to 436.0 ms | 1.004 [0.983, 1.050] | 76.8 to 76.7 MiB | 0.999 [0.997, 1.000] |
| | musl | 20 | 376.9 to 378.5 ms | 0.999 [0.961, 1.027] | 94.5 to 94.2 MiB | 0.997 [0.981, 0.999] |
| settled sync, 100,000 files | glibc | 20 | 409.9 to 417.0 ms | 1.010 [0.997, 1.038] | 77.0 to 77.0 MiB | 1.000 [0.999, 1.001] |
| | musl | 20 | 369.9 to 374.5 ms | 0.995 [0.987, 1.013] | 92.9 to 92.9 MiB | 1.001 [0.999, 1.001] |
| a sync that places one deleted file and writes the 100,000-record state | glibc | 25 | 773.3 to 766.7 ms | 0.992 [0.976, 1.002] | 132.9 to 132.9 MiB | 0.999 [0.997, 1.002] |
| | musl | 25 | 730.5 to 736.1 ms | 1.007 [1.002, 1.013] | 153.9 to 154.0 MiB | 1.000 [0.999, 1.001] |
| sync of 40,000 one-file datasets into an empty folder | glibc | 10 | 1.219 to 1.209 s | 0.993 [0.973, 1.007] | 140.5 to 140.2 MiB | 1.000 [0.997, 1.001] |
| | musl | 10 | 988.2 to 994.2 ms | 1.005 [0.998, 1.016] | 200.4 to 201.4 MiB | 1.000 [0.990, 1.011] |
| settled sync of 3 files | glibc | 40 | 4.8 to 4.8 ms | 1.006 [0.997, 1.013] | 9.3 to 9.3 MiB | 1.006 [0.996, 1.014] |
| | musl | 40 | 4.5 to 4.5 ms | 0.990 [0.966, 1.001] | 14.7 to 14.7 MiB | 1.002 [0.999, 1.004] |
| `--json status`, 1,000,001 files | glibc | 15 | 5.943 to 5.780 s | 0.961 [0.953, 1.004] | 690.4 to 690.5 MiB | 1.000 [1.000, 1.000] |
| | musl | 15 | 4.553 to 4.580 s | 1.001 [0.989, 1.011] | 668.2 to 668.3 MiB | 1.000 [1.000, 1.000] |
| settled sync, 1,000,001 files | glibc | 15 | 5.404 to 5.464 s | 0.998 [0.989, 1.022] | 690.7 to 690.8 MiB | 1.000 [1.000, 1.000] |
| | musl | 15 | 4.488 to 4.455 s | 0.998 [0.966, 1.005] | 668.8 to 668.8 MiB | 1.000 [1.000, 1.000] |

the first build of the change started its writer's buffer empty, and its doublings ended a 24.7 MB state's buffer at 38 MiB, where serde_json's start of 128 bytes ends at 32. on the sync that writes the 100,000-record state, heaptrack showed peak heap 101.7 to 108.0 MB, and gnu time 1.1 MiB more on glibc, [1.005, 1.021], and 1.9 MiB more on musl, [1.011, 1.015]. the change now starts at 128 bytes (peak heap 101.7 MB again), and the rows above are its build, except the two 1,000,001-file whole reads, which ran on the first build: they never write the state. other load on the machine flagged most rounds of the 100,000-file status (17 of 20) and of the first run of the writing sync, which ran again unflagged for its row. the one cost left is 0.7% on musl for a sync that writes a large state; glibc's interval for it holds 1.

## a stop during the data folder walk, 2026-11-12

the walk that lists every file under the data folder for a survey looked at no stop, so a first ctrl-c during it waited for the whole listing, which on a network filesystem of a million files can take many seconds. it now looks at the run's stop before each folder it lists, and `Survey::read` ends with `interrupted`. `a_survey_ends_at_a_stop_asked_before_it` lists a data folder with a stop already asked: it failed while the walk ignored the stop, and again with the check taken out.

an atomic load per folder costs nothing measurable: status of 100,000 files in 102 folders, in place on ext4 over nvme, efbf3adf against the clean release build of this batch, 0.996 [0.988, 1.003] on glibc (43 pairs) and 1.003 [0.997, 1.011] on musl (46 pairs), peak memory 79.1 to 79.6 MiB and 95.2 to 95.8 MiB. how long a stop takes on a large slow tree was not timed.

## small leftovers from the perf maps, 2026-11-13

ideas the perf maps left open whose a/b took one build, each kept or dropped by the ledger's rule.

- the data folder walk built each file's path with `format!`, which grows the string about twice a file, some 200,000 growth calls at 100,000 files. making it at its final length, one experiment build switched by `FETCHLOOM_EXP_PRESIZE`, gave `--json status` of 100,000 files on tmpfs 0.986 [0.970, 0.998] and the settled sync 0.991 [0.979, 1.004], 19 and 20 pairs on glibc, peak memory flat. dropped, below the bar.
- a split download writes each chunk of a stream past the file's start as it arrives and hashes it on the way, where the first stream's chunks gather into 1 MiB writes hashed on a worker, and joining a stream's bytes to the start reads them back through a new 1 MiB buffer. a download splits only once one stream of a file over 64 MiB runs below 1 MB/s for 2 s, so the network sets its pace. a 96 MiB file from a local server holding each connection to 0.8 MB/s, with a musl build of df4d2a5b: on one stream 125.8 s; split, 36.8 s for 0.27 s of cpu, 0.7%, which covers hashing every byte and reading back the three quarters the later streams wrote. a cheaper handoff or buffer cannot reach the bar, so neither was built: dropped.
- each SHA-256 written to the state, data.lock or a JSON answer was first made into a new 64-byte string. the cpu map found writing them from a buffer on the stack 11 to 17% faster when rendering small states alone. one experiment build per libc of 7201f052, switched by `FETCHLOOM_EXP_HEX_STACK`, printed the same bytes, and as whole commands: `--json export` of 100,000 files, 200,000 digests, 0.960 [0.955, 0.969] on musl and 0.960 [0.947, 0.969] on glibc; the sync that places one file and writes the 100,000-record [indexed state](#an-indexed-project-state-2026-11-12) 1.002 [0.968, 1.028] and 1.002 [0.970, 1.014]; a sync of 10,000 datasets, `add -i` of 1,000 links and `--json status` of 100,000 files within 1.2%; peak memory flat. dropped, below the bar.
- the cpu map found sonic-rs parsing states about 20% faster than serde_json on its own, but only about 5% of a whole status, and that was before the [indexed state](#an-indexed-project-state-2026-11-12). one experiment build per libc of 7201f052 read the whole state and wrote its records with sonic-rs 0.5.10 under `FETCHLOOM_EXP_SONIC`. its state held the same records in the same order as serde_json's, 24,719,420 bytes either way, and serde_json read it back clean. as whole commands, musl then glibc: the settled sync of 100,000 files 0.947 [0.930, 0.968] and 0.954 [0.942, 0.975]; `--json status` of them 0.965 [0.939, 0.975] and 0.981 [0.942, 0.999]; the sync writing their state 0.956 [0.934, 0.966] and 0.974 [0.963, 0.986]; a sync of 10,000 datasets and a settled sync of 3 files within 1%, the latter's peak memory 1.031 [1.016, 1.058] on glibc. it adds 16 packages to Cargo.lock, rkyv among them, and about 0.3 MB to the binary. other users' load flagged nearly half the musl rounds; these keep them, and the clean rounds give the same picture. dropped, below the bar.

## reusing racy hashes within a survey, 2026-11-12

`status --verify` and `sync --verify` hashed an intact file twice when its recorded time was as new as the state file's time: the racy check read it first, then verification read it again. the change keeps that first result on the in-memory placed record, without another set of paths. it leaves edited-versus-damaged handling and cache-doubt checks alone. the proof is never serialized or accepted from a state file; the record stayed 88 bytes on both sides of the layout check.

one release experiment from d0176dd7 per libc ran both arms through a read-once switch, every arm once per seeded shuffled round. the following `status --verify` cases have 12 clean pairs each after warmups, on ext4 over nvme and an owned FAT32 image. state times were fixed to the files' recorded tick to exercise racy reads without sleeps. every fixture's bytes were independently checked; every command's JSON was checked. the shared machine's load bursts remain in the raw records, excluded by the predeclared rule and repeated. timed windows held the measurement lock on cores 12–23, for less than five minutes each.

| files | build | before s [range] | after s [range] | time ratio [95%] | peak RSS ratio [95%] |
|---|---|---|---|---|---|
| 2 files of 16 MiB, ext4 | glibc | 0.058 [0.056, 0.063] | 0.033 [0.032, 0.036] | 0.563 [0.558, 0.565] | 1.000 [0.985, 1.013] |
| 2 files of 16 MiB, ext4 | musl | 0.055 [0.054, 0.059] | 0.030 [0.029, 0.033] | 0.544 [0.540, 0.553] | 0.987 [0.987, 0.987] |
| 2 files of 16 MiB, FAT32 | glibc | 0.060 [0.058, 0.062] | 0.034 [0.032, 0.035] | 0.560 [0.552, 0.568] | 1.005 [0.998, 1.011] |
| 2 files of 16 MiB, FAT32 | musl | 0.059 [0.057, 0.061] | 0.032 [0.031, 0.037] | 0.543 [0.535, 0.553] | 0.991 [0.990, 0.996] |
| 10,000 files of 4 KiB, ext4 | glibc | 0.077 [0.072, 0.087] | 0.064 [0.063, 0.080] | 0.827 [0.803, 0.840] | 0.981 [0.975, 0.990] |
| 10,000 files of 4 KiB, ext4 | musl | 0.074 [0.073, 0.081] | 0.061 [0.060, 0.066] | 0.821 [0.813, 0.830] | 0.933 [0.929, 0.947] |
| 100,000 files of 32 B, ext4 | glibc | 0.670 [0.653, 0.693] | 0.562 [0.549, 0.606] | 0.837 [0.826, 0.857] | 1.001 [1.000, 1.003] |
| 100,000 files of 32 B, ext4 | musl | 0.652 [0.636, 0.682] | 0.549 [0.532, 0.599] | 0.840 [0.827, 0.882] | 0.953 [0.952, 0.955] |

`sync --locked --verify` of the two 16 MiB files took 0.572 [0.564, 0.577] of its time on glibc and 0.564 [0.554, 0.568] on musl, with peak RSS ratios 1.000 [0.985, 1.011] and 1.000 [1.000, 1.000]. ordinary racy status, non-racy verification, settled status of 10,000 files and settled sync of 100,000 files stayed within noise on both builds; none of their time or memory intervals ended above 1.02. the 100,000-file verification peaked at median 79.0 MiB on glibc both ways and 135.2 to 128.9 MiB on musl.

wall and CPU came from the interleaved harness. peak RSS came from GNU time's child `wait4`, not the python parent's inherited RSS floor; the original harness value is retained beside it. a separate syscall check of the two large files counted 142 to 100 reads on glibc and 137 to 95 on musl. full commands, samples, excluded rounds and build hashes remain with the sweep outside the repository. no native windows or macos performance claim is made.

the fixed-input regression makes a file unavailable after its racy hash, then verifies that it is not reopened, an unchecked changed file is still found, and a racy edit stays an edit. the old code failed on the second open. disabling the new proof reproduced that failure. state round-trip checks reject a forged persisted proof, and the alias regression carries the proof with the record; these checks also passed on FAT32.

the change meets the 10% keep rule without a measured control regression. it is kept, with the switch removed and the state format unchanged.

## memoizing recipe input contents, 2026-11-12

dropped after three bounded versions. the prototype remembered fetched recipe-input content by both the pin object's identity and its member-filter object's identity. read-only passes borrowed those objects; a build froze fetched-input hashes after relocking, without caching code hashes or derived trees. the checks before and after executing a recipe remained live. no recipe-key encoding changed.

v1 used a B-tree memo. the fan-out case improved sharply, but independent 32-pair controls confirmed about 0.5–0.8% extra time when every recipe had a different one-file input. v2 used hash lookup, reserved the single-file canonical-line buffer from its format bounds, and built the frozen-input map without a second memo. its glibc 1,000-distinct-input control still slowed by about 1%, with roughly 260 more minor faults and higher system time. an independent repeat reproduced that cost. v3 reserved the complete recipe pass before its per-step allocations and pre-sized the frozen-input map from the selected iterator's bound. the last control pass still slowed, so no version stayed.

v1 used the earlier 27811da3 build; v2 and v3 include c3438f1c's CPU changes and the kept rename index. each version has one release executable per libc with `FETCHLOOM_EXP_RECIPE_CONTENT` read once, not separately compiled arms. the paired wall, noise and bootstrap method follows [the rename comparison](lock-and-names.md#indexing-pin-membership-and-renames-2026-11-12). v2 used at least 64 clean pairs for one and ten recipes, 32 for protected controls, and 10 for large fan-out gains. native RSS was measured outside the wall lock with GNU time: 32 pairs at small n, five through 1,000 and three above it, with elapsed time retained separately because allocator purging depends on time.

in a fan-out fixture, n recipes share one input of n files; the commands are offline JSON `status` and `build --dry-run`. no-reuse controls give each recipe its own one-file input. the named control previews one recipe among 1,000, and the ordinary 100,000-file status control has no recipes. all v2 output pairs matched. the complete v2 matrix follows; times are medians with 95% intervals, and RSS is median [minimum, maximum] of per-process peaks in MiB.

| libc | workload | n | before ms [95%] | after ms [95%] | paired ratio [95%] | peak RSS MiB, before to after |
|---|---|---:|---|---|---|---|
| glibc | shared-input status | 1 | 4.432 [4.404, 4.457] | 4.458 [4.410, 4.496] | 1.0035 [1.0005, 1.0118] | 11.69 [11.14, 12.25] to 11.73 [10.85, 12.36] |
| musl | shared-input status | 1 | 4.111 [4.085, 4.152] | 4.109 [4.080, 4.150] | 1.0037 [0.9950, 1.0148] | 18.05 [18.05, 18.30] to 18.05 [18.05, 18.05] |
| glibc | shared-input dry run | 1 | 3.858 [3.842, 3.919] | 3.850 [3.825, 3.877] | 0.9957 [0.9828, 1.0084] | 11.60 [11.08, 11.92] to 11.58 [10.87, 12.00] |
| musl | shared-input dry run | 1 | 3.371 [3.305, 3.408] | 3.391 [3.332, 3.458] | 1.0093 [0.9953, 1.0198] | 16.25 [16.25, 16.26] to 16.25 [16.25, 16.26] |
| glibc | shared-input status | 10 | 4.470 [4.441, 4.499] | 4.452 [4.420, 4.499] | 0.9939 [0.9882, 1.0000] | 11.84 [11.05, 12.12] to 11.82 [10.89, 12.16] |
| musl | shared-input status | 10 | 4.392 [4.361, 4.421] | 4.349 [4.326, 4.364] | 0.9926 [0.9798, 0.9965] | 18.30 [18.30, 18.31] to 18.30 [18.30, 18.31] |
| glibc | shared-input dry run | 10 | 4.181 [4.109, 4.273] | 4.126 [4.005, 4.241] | 0.9827 [0.9796, 0.9895] | 11.77 [11.05, 12.31] to 11.78 [11.09, 12.07] |
| musl | shared-input dry run | 10 | 3.607 [3.551, 3.736] | 3.530 [3.480, 3.591] | 0.9776 [0.9683, 0.9905] | 16.50 [16.25, 16.51] to 16.50 [16.25, 16.51] |
| glibc | shared-input status | 1,000 | 447.475 [445.695, 448.513] | 25.185 [25.050, 25.782] | 0.0565 [0.0562, 0.0569] | 16.84 [16.55, 17.33] to 16.95 [16.71, 17.14] |
| musl | shared-input status | 1,000 | 463.236 [462.403, 465.187] | 19.448 [19.311, 19.622] | 0.0419 [0.0417, 0.0422] | 34.27 [34.27, 34.28] to 30.27 [30.27, 30.28] |
| glibc | shared-input dry run | 1,000 | 1163.073 [1155.403, 1172.514] | 739.067 [730.816, 749.855] | 0.6308 [0.6269, 0.6486] | 16.36 [15.79, 16.71] to 16.19 [16.04, 16.43] |
| musl | shared-input dry run | 1,000 | 1159.120 [1155.832, 1161.063] | 709.471 [707.390, 711.157] | 0.6119 [0.6098, 0.6143] | 32.35 [32.35, 32.36] to 32.60 [32.35, 32.60] |
| glibc | shared-input status | 4,000 | 6875.321 [6837.000, 7035.020] | 97.247 [95.045, 101.165] | 0.0140 [0.0139, 0.0144] | 33.42 [33.02, 33.68] to 32.33 [32.03, 33.43] |
| musl | shared-input status | 4,000 | 7186.068 [7175.918, 7197.408] | 66.039 [65.833, 67.124] | 0.0092 [0.0092, 0.0093] | 53.53 [53.52, 53.53] to 70.27 [70.27, 70.28] |
| glibc | status, no recipes | 100,000 | 426.502 [424.685, 430.796] | 427.475 [425.807, 431.293] | 1.0003 [0.9956, 1.0115] | 79.11 [79.02, 79.47] to 79.10 [78.98, 79.20] |
| musl | status, no recipes | 100,000 | 367.862 [365.961, 370.291] | 368.748 [367.061, 370.699] | 1.0024 [0.9971, 1.0107] | 95.80 [85.65, 95.90] to 86.14 [85.65, 95.98] |
| glibc | distinct-input status | 1,000 | 46.029 [45.616, 46.266] | 46.585 [46.367, 47.022] | 1.0111 [1.0015, 1.0199] | 19.53 [19.32, 19.96] to 20.61 [20.48, 20.67] |
| musl | distinct-input status | 1,000 | 35.438 [35.332, 35.568] | 35.516 [35.293, 35.607] | 1.0010 [0.9950, 1.0050] | 42.27 [42.27, 42.28] to 44.28 [44.27, 44.52] |
| glibc | distinct-input status | 4,000 | 190.867 [189.802, 191.724] | 191.076 [189.779, 192.440] | 0.9985 [0.9932, 1.0064] | 47.23 [46.85, 47.38] to 46.98 [46.67, 47.03] |
| musl | distinct-input status | 4,000 | 132.682 [132.340, 133.029] | 132.696 [132.229, 132.942] | 1.0018 [0.9961, 1.0064] | 82.28 [82.03, 82.28] to 82.28 [82.28, 82.28] |
| glibc | one named dry run | 1,000 | 18.454 [18.283, 19.108] | 18.490 [18.265, 18.976] | 1.0036 [0.9924, 1.0106] | 16.11 [15.94, 16.67] to 16.39 [16.02, 16.52] |
| musl | one named dry run | 1,000 | 16.722 [16.662, 16.851] | 16.832 [16.631, 16.923] | 1.0065 [0.9966, 1.0138] | 26.36 [26.35, 26.60] to 26.35 [26.34, 26.60] |

| libc | workload | n | instructions M, before to after | clean pairs | excluded burst pairs |
|---|---|---:|---:|---:|---:|
| glibc | shared-input status | 1 | 4.518 to 4.520 | 64 | 0 |
| musl | shared-input status | 1 | 3.360 to 3.359 | 64 | 0 |
| glibc | shared-input dry run | 1 | 3.361 to 3.357 | 64 | 0 |
| musl | shared-input dry run | 1 | 2.577 to 2.579 | 76 | 4 |
| glibc | shared-input status | 10 | 6.109 to 5.686 | 64 | 0 |
| musl | shared-input status | 10 | 4.690 to 4.313 | 64 | 0 |
| glibc | shared-input dry run | 10 | 5.723 to 5.292 | 78 | 2 |
| musl | shared-input dry run | 10 | 4.562 to 4.190 | 64 | 0 |
| glibc | shared-input status | 1,000 | 4522.578 to 142.146 | 13 | 1 |
| musl | shared-input status | 1,000 | 4221.601 to 120.446 | 10 | 0 |
| glibc | shared-input dry run | 1,000 | 12663.174 to 8307.728 | 10 | 4 |
| musl | shared-input dry run | 1,000 | 11497.636 to 7398.191 | 10 | 8 |
| glibc | shared-input status | 4,000 | 71331.961 to 570.015 | 10 | 0 |
| musl | shared-input status | 4,000 | 66207.897 to 488.600 | 13 | 1 |
| glibc | status, no recipes | 100,000 | 2001.309 to 2001.387 | 32 | 14 |
| musl | status, no recipes | 100,000 | 2476.139 to 2476.324 | 32 | 0 |
| glibc | distinct-input status | 1,000 | 261.836 to 261.132 | 32 | 0 |
| musl | distinct-input status | 1,000 | 224.904 to 224.978 | 32 | 0 |
| glibc | distinct-input status | 4,000 | 1064.742 to 1061.451 | 32 | 0 |
| musl | distinct-input status | 4,000 | 922.276 to 922.487 | 32 | 0 |
| glibc | one named dry run | 1,000 | 80.328 to 80.331 | 35 | 1 |
| musl | one named dry run | 1,000 | 67.910 to 67.911 | 32 | 0 |

v2's default musl 4,000-recipe peak rose from 53.5 to 70.3 MiB. it remains below 150 MB, but is not memory-neutral. the earlier purge-delay diagnostic reduced the v1 before/after peaks to about 39.6 MiB each; that override was not used for a keep decision. the ordinary and small measurements remain visible rather than subtracting inferred retained allocator pages.

the fixed independent v2 confirmation again found the glibc 1,000-input slowdown: ratio 1.0106 [1.0031, 1.0179] over 32 clean pairs. its other no-reuse, named and single-dataset checks did not establish a slowdown. the final v3 control results were:

| libc | control | before ms [95%] | after ms [95%] | paired ratio [95%] | peak RSS MiB, before to after |
|---|---|---|---|---|---|
| glibc | 1,000 distinct inputs | 46.151 [45.753, 46.668] | 46.485 [46.221, 46.941] | 1.0092 [1.0019, 1.0156] | 20.06 [18.94, 20.16] to 20.70 [20.61, 21.13] |
| glibc | 4,000 distinct inputs | 188.578 [187.127, 190.354] | 189.115 [188.569, 189.920] | 1.0003 [0.9959, 1.0063] | 46.81 [46.66, 46.91] to 46.82 [46.12, 47.02] |
| glibc | one named recipe among 1,000 | 18.157 [18.123, 18.216] | 18.207 [18.133, 18.251] | 1.0014 [0.9988, 1.0058] | 16.25 [16.05, 16.36] to 16.23 [16.17, 16.55] |
| musl | 1,000 distinct inputs | 35.250 [35.183, 35.416] | 35.575 [35.426, 35.766] | 1.0086 [1.0050, 1.0113] | 42.28 [42.27, 42.59] to 46.27 [42.27, 46.28] |
| musl | 4,000 distinct inputs | 131.481 [131.191, 131.764] | 131.161 [130.878, 131.657] | 0.9987 [0.9934, 1.0036] | 82.28 [82.27, 82.28] to 82.28 [82.28, 82.28] |
| musl | one named recipe among 1,000 | 16.827 [16.716, 16.946] | 16.950 [16.829, 17.087] | 1.0079 [1.0022, 1.0201] | 26.35 [26.35, 26.60] to 26.59 [26.34, 26.60] |

each v3 row has 32 clean wall pairs. the larger fan-out ladder was not repeated for v3 after these controls rejected it. the stated keep rule rejects a reproducible time regression elsewhere, so the large fan-out gain was not enough.

correctness checks passed: `shared_recipe_inputs_are_read_once` first failed with two calculations instead of one, then passed with the memo and failed again when reuse was bypassed. `reading_again_uses_the_current_pin_and_member_filters`, `content_memo_keeps_pin_and_member_identities_apart`, `frozen_inputs_do_not_cross_pin_or_filter_identity` and `frozen_inputs_leave_code_and_derived_trees_live` covered identity changes and live code/derived values. the revised source also integrated the newly landed strict-sync check. v2 and v3 each passed 19 affected unit tests and all-target clippy. these tests, the switches and prototype code were removed from the product with the rejected change; their patches, binaries, full first-pass and independent-control records remain with the sweep outside the repository. no allocator policy was changed.

## searching selected folder ranges, 2026-11-12

kept. `outermost` sorts borrowed folder paths once, removes duplicate and nested roots, and clones only the roots the survey needs. the state predicate uses binary predecessor lookup over those same roots, with direct checks for zero or one root. paths compare as if they ended in `/`: ordinary byte order would put `a-` between `a` and `a/x` and could hide the right ancestor. the comparison allocates no query string. the change is confined to folder normalization and membership in `sync/disk.rs`; verification scheduling, the state format, candidates and flags are unchanged.

the source base includes 066a6416 and the kept rename change. one stock release executable per libc carried `FETCHLOOM_EXP_FOLDER_RANGES`, read once before the survey. both arms used the same binary and settled, offline fixtures. a plain sync established each fixture, then an independent check verified the state's complete marker and both manifest/lock text hashes. every reported JSON answer matched between arms.

`sync` named all datasets at 1, 10, 1,000 and 40,000. a second ladder named 1, 10 or 1,000 within the same 40,000-dataset project. controls named the sole folder of a 100,000-file dataset or ran plain sync of the wide project. n is the selected dataset count, except the ordinary file control, where it counts files. JSON output was discarded only during timing.

`abh.py` shuffled paired arms on cores 12–23 at nice 0, with a warmup after each acquisition. counter-backed window budgets targeted 250 s; the longest observed lock hold was 225.9 s, below five minutes. whole load-burst pairs stayed in the raw records but were excluded and replaced. small cases have at least 64 clean pairs, controls 32, and large-gain cases 10. the median and paired-log-ratio intervals use `bench/ab.py`'s 20,000-resample 95% percentile bootstrap.

native RSS and instructions ran separately on cores 0–11 at nice 10 without the wall lock. small-n RSS has 32 observations per arm, other cases through 1,000 have five, and larger ones three. RSS cells show median [minimum, maximum] of per-process peaks in MiB, not the Python parent's inherited high-water mark. at 40,000 selected datasets glibc's median peak falls by about 1 MiB; musl spends 0.75 MiB more. the wide-manifest musl workloads already exceed the ordinary 150 MB target, a miss still shown here. the ordinary 100,000-file control stays below that target on both libc builds. no protected time interval establishes a slowdown.

| libc | workload | n | before ms [95%] | after ms [95%] | paired ratio [95%] | peak RSS MiB, before to after |
|---|---|---:|---|---|---|---|
| glibc | all named datasets | 1 | 4.442 [4.361, 4.546] | 4.400 [4.348, 4.531] | 1.0052 [0.9914, 1.0146] | 13.10 [11.86, 13.38] to 13.04 [11.87, 13.50] |
| musl | all named datasets | 1 | 4.149 [4.050, 4.235] | 4.080 [4.038, 4.130] | 0.9783 [0.9625, 1.0091] | 17.14 [17.13, 17.14] to 17.14 [17.13, 17.14] |
| glibc | all named datasets | 10 | 4.370 [4.342, 4.440] | 4.406 [4.320, 4.453] | 0.9946 [0.9835, 1.0103] | 13.23 [12.12, 13.58] to 13.16 [12.05, 13.63] |
| musl | all named datasets | 10 | 4.032 [3.967, 4.077] | 4.035 [3.962, 4.083] | 0.9970 [0.9862, 1.0231] | 17.14 [17.13, 17.14] to 17.14 [17.13, 17.14] |
| glibc | all named datasets | 1,000 | 29.208 [28.860, 29.696] | 25.170 [24.981, 25.740] | 0.8683 [0.8483, 0.8832] | 16.23 [16.16, 16.63] to 16.47 [15.91, 16.55] |
| musl | all named datasets | 1,000 | 31.637 [31.162, 31.874] | 24.002 [23.682, 24.227] | 0.7591 [0.7499, 0.7707] | 25.45 [25.45, 25.46] to 25.45 [25.41, 25.46] |
| glibc | all named datasets | 40,000 | 7354.738 [7332.877, 7604.208] | 969.291 [963.004, 972.418] | 0.1312 [0.1275, 0.1325] | 137.77 [137.66, 137.79] to 136.75 [136.42, 136.88] |
| musl | all named datasets | 40,000 | 11742.476 [11683.997, 11879.797] | 812.972 [804.548, 814.583] | 0.0687 [0.0681, 0.0697] | 159.30 [159.30, 159.30] to 160.05 [160.05, 160.05] |
| glibc | named subset of 40,000 datasets | 1 | 277.834 [276.250, 278.580] | 277.943 [276.465, 278.782] | 0.9995 [0.9980, 1.0033] | 109.40 [108.60, 109.88] to 109.50 [108.60, 109.88] |
| musl | named subset of 40,000 datasets | 1 | 175.960 [175.762, 176.291] | 176.217 [175.945, 176.473] | 1.0004 [0.9989, 1.0020] | 143.46 [143.16, 143.46] to 143.46 [143.21, 143.46] |
| glibc | named subset of 40,000 datasets | 10 | 285.787 [285.456, 286.711] | 284.567 [284.087, 285.715] | 0.9958 [0.9941, 0.9996] | 109.52 [108.43, 109.92] to 109.53 [108.79, 109.98] |
| musl | named subset of 40,000 datasets | 10 | 181.299 [181.104, 181.622] | 180.195 [179.958, 180.607] | 0.9940 [0.9928, 0.9947] | 143.46 [143.16, 145.46] to 143.46 [143.16, 145.45] |
| glibc | named subset of 40,000 datasets | 1,000 | 481.233 [476.623, 484.055] | 301.272 [299.214, 304.834] | 0.6285 [0.6206, 0.6359] | 109.73 [109.65, 109.88] to 109.88 [109.36, 110.07] |
| musl | named subset of 40,000 datasets | 1,000 | 465.832 [465.351, 466.473] | 200.341 [199.682, 201.059] | 0.4303 [0.4288, 0.4314] | 147.47 [147.17, 147.47] to 147.48 [147.47, 147.48] |
| glibc | one folder, files control | 100,000 | 400.094 [397.251, 401.771] | 401.855 [398.962, 405.866] | 1.0025 [0.9966, 1.0121] | 80.98 [80.64, 81.56] to 81.23 [81.18, 81.33] |
| musl | one folder, files control | 100,000 | 372.510 [370.601, 373.263] | 372.619 [371.055, 374.074] | 1.0012 [0.9939, 1.0051] | 95.55 [95.49, 105.56] to 95.57 [95.49, 101.54] |
| glibc | plain sync, no selector | 40,000 | 798.946 [792.103, 804.972] | 797.362 [787.379, 806.083] | 0.9985 [0.9955, 1.0046] | 117.61 [117.53, 117.88] to 117.81 [117.76, 117.81] |
| musl | plain sync, no selector | 40,000 | 649.631 [648.297, 653.055] | 649.089 [647.844, 651.650] | 1.0007 [0.9977, 1.0027] | 156.96 [156.96, 156.96] to 156.96 [156.95, 156.96] |

| libc | workload | n | instructions M, before to after | clean pairs | excluded burst pairs |
|---|---|---:|---:|---:|---:|
| glibc | all named datasets | 1 | 3.404 to 3.404 | 64 | 0 |
| musl | all named datasets | 1 | 2.660 to 2.660 | 64 | 0 |
| glibc | all named datasets | 10 | 4.002 to 3.995 | 64 | 0 |
| musl | all named datasets | 10 | 3.161 to 3.160 | 64 | 0 |
| glibc | all named datasets | 1,000 | 109.356 to 75.193 | 10 | 0 |
| musl | all named datasets | 1,000 | 147.564 to 67.697 | 10 | 0 |
| glibc | all named datasets | 40,000 | 58171.889 to 3157.121 | 10 | 0 |
| musl | all named datasets | 40,000 | 112333.961 to 2900.111 | 10 | 0 |
| glibc | named subset of 40,000 datasets | 1 | 1475.979 to 1475.714 | 64 | 0 |
| musl | named subset of 40,000 datasets | 1 | 1312.024 to 1311.786 | 64 | 0 |
| glibc | named subset of 40,000 datasets | 10 | 1520.715 to 1517.891 | 64 | 0 |
| musl | named subset of 40,000 datasets | 10 | 1366.942 to 1358.448 | 64 | 0 |
| glibc | named subset of 40,000 datasets | 1,000 | 2941.925 to 1608.699 | 10 | 8 |
| musl | named subset of 40,000 datasets | 1,000 | 4131.916 to 1452.009 | 10 | 0 |
| glibc | one folder, files control | 100,000 | 2017.887 to 2025.549 | 32 | 7 |
| musl | one folder, files control | 100,000 | 2493.082 to 2493.294 | 32 | 0 |
| glibc | plain sync, no selector | 40,000 | 2297.916 to 2297.917 | 32 | 0 |
| musl | plain sync, no selector | 40,000 | 2105.640 to 2105.638 | 32 | 0 |

`selected_folders_keep_separator_boundaries_and_shared_paths` and the extended `a_survey_within_folders_lists_and_keeps_only_them` passed on the old algorithm first. they cover nested roots, shared/duplicate paths, `a` versus `a-` and `ab`, unicode, the data root, empty selection, links, missing folders and a file blocking a folder. `folder_order_matches_a_terminal_separator` independently checks the custom comparison against explicit suffixed strings. replacing it with ordinary string order made the new assertions fail; restoring it passed. the existing partial-state test still verifies that selected records, candidates and sync flags round-trip. after switch removal, the full survey characterization exercises the indexed path itself. the old paths and experiment switch are removed; raw fixtures, binaries, counters, paired runs and mutation logs remain with the sweep outside the repository.

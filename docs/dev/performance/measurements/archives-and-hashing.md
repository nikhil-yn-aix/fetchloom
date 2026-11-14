# Archives and hashing

## resources levels against default, 2026-11-13

what each [level](../../../user/guides/speed-and-memory.md) trades, on the eight workloads of the perf audit's level pass, four of them on musl too. lean sets 2 requests at once, 2 to one host, a 128 MiB buffer budget and 2 local workers; max sets 32 and 16 requests and keeps default's workers and memory. every arm ran the same binary with only `FETCHLOOM_RESOURCES` changed. a ratio is level over default, the median of paired rounds with its 95% bootstrap interval.

| workload | libc | pairs | default | lean: time, peak memory | max: time, peak memory |
|---|---|---:|---|---|---|
| `add -i` of 100 links of 4 KiB from a server that waits 50 ms an answer | glibc | 12 | 2.264 s, 16.3 MiB | 2.006 [2.005, 2.011], 0.998 [0.971, 1.007] | 0.281 [0.279, 0.284], 1.077 [1.055, 1.087] |
| `add -i` of 1,000 links of 90 to 110 kB from a local server | glibc | 10 | 343.1 ms, 26.5 MiB | 1.081 [1.027, 1.098], 0.887 [0.869, 0.899] | 1.046 [1.005, 1.073], 1.274 [1.239, 1.294] |
| | musl | 15 | 342.5 ms, 96.4 MiB | 1.085 [1.022, 1.141], 0.679 [0.652, 0.712] | 1.040 [1.017, 1.130], 1.141 [1.099, 1.227] |
| `get -o` of 1 GiB from a local server | glibc | 10 | 771.3 ms, 67.9 MiB | 0.977 [0.961, 0.994], 0.862 [0.792, 0.935] | 1.005 [0.995, 1.033], 1.069 [0.967, 1.160] |
| | musl | 10 | 730.2 ms, 94.7 MiB | 0.997 [0.991, 1.001], 0.991 [0.745, 1.154] | 0.999 [0.993, 1.004], 1.074 [0.991, 1.107] |
| `status --verify` of 8 files of 64 MiB | glibc | 19 | 74.3 ms, 17.7 MiB | 6.608 [5.667, 6.960], 0.673 [0.657, 0.678] | 0.958 [0.870, 0.997], 0.998 [0.990, 1.009] |
| `cache verify` of 100,000 objects | glibc | 15 | 408.3 ms, 32.1 MiB | 1.961 [1.921, 2.014], 0.985 [0.978, 0.992] | 1.010 [0.981, 1.025], 1.006 [0.988, 1.012] |
| sync of a zip of 10,000 files | glibc | 12 | 299.3 ms, 39.6 MiB | 2.095 [2.043, 2.145], 0.934 [0.824, 1.137] | 1.018 [0.993, 1.030], 0.934 [0.847, 1.080] |
| | musl | 15 | 314.0 ms, 128.5 MiB | 2.031 [1.970, 2.051], 0.870 [0.784, 0.894] | 0.992 [0.958, 0.996], 1.000 [0.986, 1.016] |
| settled sync of 100,000 files | glibc | 8 | 409.8 ms, 79.7 MiB | 1.112 [1.078, 1.167], 0.999 [0.994, 1.004] | 1.000 [0.978, 1.007], 1.000 [0.995, 1.004] |
| sync of 40,000 one-file datasets into an empty folder | glibc | 10 | 1.208 s, 145.0 MiB | 1.115 [1.102, 1.137], 0.998 [0.997, 1.001] | 0.999 [0.988, 1.007], 1.001 [0.998, 1.002] |
| | musl | 14 | 1.000 s, 203.6 MiB | 1.144 [1.131, 1.147], 0.901 [0.898, 0.909] | 1.000 [0.987, 1.024], 0.990 [0.981, 1.000] |

so lean pays 1.08 to 1.15 times on most local work and about 2 to 6.6 times where the work spreads over workers: a zip, `cache verify`, the large files' verification and the far server. it saves memory where buffers or workers hold it: big files hashed on fewer workers, 1,000 downloads (0.887 glibc, 0.679 musl), and on musl the zip and the many datasets. max is its target's 0.281 for 7.7% more memory; with 1,000 links from a local server it is 4 to 5% slower for 14 to 27% more, and on local work it changes nothing, since it sets only requests. its 0.958 on the 74 ms `status --verify` has no setting behind it.

glibc and musl release builds of bec45367, interleaved rounds after a warmup under the measure lock on cores 12-23. projects and caches on tmpfs, so flushes cost nothing; links from a local server on tmpfs, the zip and the 1 GiB file from the nvme disk's page cache. the copied 100,000-file and large projects were synced once by this build before the runs, since a state from before the [indexed format](project-state.md#an-indexed-project-state-2026-11-12) reads as empty, and every file would be hashed. rounds with more than 12 other cores busy are left out; the settled sync and `cache verify` lost most rounds to load, and all 40 of each give the same answer: lean 1.095 [1.062, 1.122] and 1.963 [1.927, 1.987], max 1.003 and 1.009.

every level's folders had names of one length. in a first pass they did not, and a project path five letters longer alone cost 3.3 MiB at the peak of the sync of 40,000 datasets, 1.023 [1.022, 1.026] for the same build without a level, enough to fake a 2 to 3% memory saving for both other levels. against the audit's pass on efbf3adf, whose levels shared one folder, every time ratio here is within its interval or at most 5% from it. the memory ratios overlap its intervals but three: max's on 1,000 local links on glibc, 1.318 then and 1.274 now; lean's on `cache verify`, 1.000 and 0.985; and lean's on the large files' verification, 0.621 and 0.673, where both levels' peaks rose by about 2 MiB. so nothing since has moved what the levels trade by more than that.

## pending archive reader budgets, 2026-11-12

with 12 available CPUs and an explicit cap of eight threads, unpacking asked for three archive readers and budgeted one writer for each. when only one archive needed reading, two reader reservations went unused. the change clamps that explicit reader count to the archives still pending after cache reuse, leaving one reader and seven writers for this case. the unset default keeps the same budget as before; no flush, publication or placement policy changes.

one switched release build per libc, from d0176dd7, compared fresh `sync --locked` runs with `FETCHLOOM_THREADS=8`. each arm used the same project and cache paths, a new empty cache, the same loopback HTTP source and an independently checked zip. the larger fixture held 10,000 distinct 4 KiB files; the small control held 32. the table gives 12 clean interleaved pairs per row, after warmups, with paired bootstrap intervals. ext4 was on nvme; xfs, btrfs and FAT32 were mounted images on that storage. only bounded wall-time batches held the shared lock, on cores 12–23. flagged load bursts stay in the raw records, not in these time estimates.

| files and filesystem | build | before s [range] | after s [range] | time ratio [95%] | peak RSS MiB, before to after | peak RSS ratio [95%] |
|---|---|---|---|---|---|---|
| 10,000 files of 4 KiB, xfs | glibc | 2.152 [2.084, 2.332] | 1.493 [1.408, 1.541] | 0.689 [0.672, 0.700] | 36.26 to 37.62 | 1.019 [0.904, 1.211] |
| 10,000 files of 4 KiB, xfs | musl | 2.200 [2.084, 2.285] | 1.557 [1.417, 1.606] | 0.703 [0.682, 0.715] | 127.84 to 127.63 | 0.991 [0.951, 1.057] |
| 10,000 files of 4 KiB, ext4 | glibc | 1.296 [1.252, 1.352] | 0.710 [0.655, 0.741] | 0.545 [0.533, 0.552] | 36.61 to 36.18 | 0.891 [0.852, 1.135] |
| 10,000 files of 4 KiB, ext4 | musl | 1.325 [1.286, 1.511] | 0.717 [0.660, 0.764] | 0.538 [0.501, 0.547] | 119.80 to 127.78 | 1.049 [0.993, 1.109] |
| 10,000 files of 4 KiB, btrfs | glibc | 2.120 [2.031, 2.321] | 1.956 [1.880, 2.071] | 0.926 [0.902, 0.935] | 31.45 to 30.80 | 0.978 [0.918, 1.013] |
| 10,000 files of 4 KiB, btrfs | musl | 2.080 [1.987, 2.281] | 2.011 [1.881, 2.161] | 0.959 [0.936, 0.978] | 121.61 to 109.91 | 0.909 [0.872, 0.951] |
| 32 files of 4 KiB, ext4 | glibc | 0.026 [0.025, 0.028] | 0.022 [0.021, 0.023] | 0.843 [0.797, 0.859] | 13.35 to 13.52 | 1.010 [0.989, 1.027] |
| 32 files of 4 KiB, ext4 | musl | 0.028 [0.026, 0.033] | 0.023 [0.022, 0.028] | 0.829 [0.803, 0.846] | 30.69 to 32.72 | 1.066 [1.064, 1.071] |
| 32 files of 4 KiB, FAT32 | glibc | 0.044 [0.043, 0.047] | 0.038 [0.037, 0.046] | 0.860 [0.835, 0.873] | 13.43 to 13.61 | 1.021 [0.996, 1.032] |
| 32 files of 4 KiB, FAT32 | musl | 0.045 [0.044, 0.046] | 0.039 [0.038, 0.042] | 0.875 [0.866, 0.887] | 30.70 to 32.71 | 1.066 [0.996, 1.066] |

more of the requested threads now do useful work, at a measured memory cost. on musl, the small ext4 case added 2.03 MiB at the median, 6.6% [6.4%, 7.1%]; the 10,000-file ext4 case added 7.99 MiB, though its paired RSS interval was wide. the largest candidate peak in these cap-eight cases was 138.70 MiB (145.4 MB). these costs fit the owner's default memory budget of about 150 MB; they are not described as memory-neutral. peak RSS came from GNU time's child `wait4`, not the python harness's inherited RSS floor. a separate unlocked 12-pair memory counter also found growth on the small musl case, 1.036 [1.003, 1.148]; its wall samples are not qualification evidence.

| control | build | pairs | before s [range] | after s [range] | time ratio [95%] | peak RSS ratio [95%] |
|---|---|---|---|---|---|---|
| 10,000 files, ext4, unset cap | glibc | 12 | 0.707 [0.681, 0.717] | 0.704 [0.680, 0.766] | 0.996 [0.990, 1.022] | 1.150 [0.898, 1.219] |
| 10,000 files, ext4, unset cap | musl | 12 | 0.717 [0.670, 0.762] | 0.712 [0.677, 0.757] | 1.000 [0.988, 1.011] | 0.993 [0.930, 1.041] |
| 10,000 files, xfs, unset cap | glibc | 12 | 1.460 [1.416, 1.517] | 1.479 [1.429, 1.619] | 1.005 [0.991, 1.035] | 1.001 [0.869, 1.051] |
| 10,000 files, xfs, unset cap | musl | 12 | 1.503 [1.448, 1.599] | 1.497 [1.411, 1.748] | 0.991 [0.982, 1.038] | 1.022 [0.962, 1.047] |
| 32 files, xfs, cap 1 | glibc | 12 | 0.034 [0.032, 0.037] | 0.033 [0.033, 0.036] | 0.984 [0.978, 1.024] | 0.995 [0.985, 0.998] |
| 32 files, xfs, cap 1 | musl | 12 | 0.035 [0.032, 0.045] | 0.034 [0.032, 0.036] | 0.987 [0.949, 1.013] | 1.000 [0.995, 1.000] |
| 3 archives of 1,000 files, xfs, cap 8 | glibc | 12 | 0.459 [0.429, 0.466] | 0.458 [0.432, 0.489] | 1.000 [0.982, 1.048] | 0.988 [0.947, 1.003] |
| 3 archives of 1,000 files, xfs, cap 8 | musl | 12 | 0.453 [0.427, 0.477] | 0.461 [0.428, 0.622] | 1.010 [0.970, 1.093] | 0.986 [0.904, 1.047] |
| 1 member of 64 MiB, xfs, cap 8 | glibc | 12 | 0.260 [0.246, 0.429] | 0.265 [0.247, 0.360] | 1.005 [0.958, 1.030] | 0.954 [0.884, 1.133] |
| 1 member of 64 MiB, xfs, cap 8 | musl | 24 | 0.329 [0.234, 0.361] | 0.344 [0.238, 0.367] | 1.016 [0.994, 1.035] | 0.990 [0.975, 1.029] |

the one-thread lean control keeps its inline, zero-writer path and showed no memory increase. this does not introduce resource levels or promise that any larger lean preset has been qualified. default, one-thread and three-archive controls keep the same effective budgets. the musl single-large-member control first gave 1.029 [1.014, 1.058]; an independent 12-pair confirmation, retaining the first pairs, gave the pooled result above. no control crossed the earlier lab's time-regression guard, a lower 95% bound above 1.02. that is not proof of exact equivalence: the large-member control still leaves a small effect unresolved.

the combined settled-sync control, 10,000 files and 12 pairs, gave time ratios 1.007 [0.989, 1.019] on glibc and 0.998 [0.982, 1.013] on musl, with RSS ratios 1.001 and 1.000. startup had 60 pairs per build, ratios 1.004 [0.998, 1.014] and 0.982 [0.976, 0.995], with unchanged median RSS; startup takes neither changed path, so that fluctuation is not a speedup claim. every completed command's result and placed bytes were checked. exact commands, binary and harness hashes, samples, excluded rounds and the one intentionally cancelled window remain with the sweep outside the repository.

the fixed-input budget regression expected one reader and seven writers for requested readers = 3, pending archives = 1 and cap = 8. the old calculation returned three readers and one writer. removing the clamp after the green run reproduced that failure. the cap-budget invariant also covers fewer pending readers.

the change meets the 10% time-gain rule on ext4 and xfs, and the reported memory cost fits the updated budget rule. it is kept with the switch removed. this fixes an explicit-cap underutilization, not the default xfs gap against curl plus unzip.

## overlapping required checksums, 2026-11-12

SHA-256 and MD5 shared one download hash worker. the CPU sweep found their combined throughput about 0.37 GB/s, against 1.49 GB/s for SHA-256 alone. a large download that needs both now gives MD5 a second worker when the quota-aware CPU count and `FETCHLOOM_THREADS` leave a slot. a SHA-256 pin still needs no MD5, and known small files still hash on the writer. no caller waits for an extra CPU slot or fails because the optional thread could not start.

each lane shares the same immutable byte batches. the MD5 handoff holds one waiting batch, not another copy of the body; it can retain about two extra 1 MiB batches across queued and running work. checkpoints wait for both prefixes, and finish and cancellation join both workers before the partial claim is released. range readback returns MD5 to the original worker so it reads the file only once. a new hash worker that exceeds the CPU budget also returns an existing pair to one lane at its next batch. after resource levels landed, lean's derived ceiling was kept on the original single-lane path so it spends no extra batch memory; an explicit `FETCHLOOM_THREADS` still overrides the level. fixed-input tests check that distinction, and no lean speedup is claimed by this default-level table.

the test: a local TLS Dataverse stand-in, with a private test CA trusted for the child process alone, stated independently calculated MD5 or SHA-256 values. release builds of 7dc24bbd plus the change, fat LTO, one codegen unit, abort, frame pointers and line tables, one binary per libc with a read-once switch. the EPYC 7V13 had 24 visible CPUs and shared training load; builds used cores 0–11 at nice 10, and timed commands 12–23 at nice 0. `abh.py` shuffled both arms per round under the measure lock, each batch under five minutes; each began with an empty cache/output on ext4 NVMe and warm source files. Python hashlib checked every resulting file outside the clock.

there were twelve measured rounds before bounded noise retries; a pair with a burst in either arm was left out. GNU time supplied the actual child's RSS, since the harness's parent can set a higher inherited wait4 floor. the table gives clean paired medians and their 95% bootstrap intervals, with before/after median time and median RSS. counts are process pairs, not response chunks.

| workload | libc | pairs | before s | after s | after/before [95%] | median MiB, before to after |
|---|---|---:|---:|---:|---|---|
| one 512 MiB file, MD5 | glibc | 9 | 1.5574 | 1.2086 | 0.776 [0.768, 0.782] | 79.3 to 75.6 |
| one 512 MiB file, MD5 | musl | 11 | 1.5323 | 1.1774 | 0.767 [0.765, 0.771] | 106.3 to 120.3 |
| eight 64 MiB files, MD5 | glibc | 11 | 0.9603 | 0.9236 | 0.982 [0.953, 1.037] | 146.1 to 129.1 |
| eight 64 MiB files, MD5 | musl | 14 | 0.7828 | 0.7087 | 0.934 [0.870, 0.999] | 244.0 to 217.2 |
| 64 files of 4 KiB, MD5 | glibc | 23 | 0.7200 | 0.7210 | 1.003 [0.999, 1.005] | 15.1 to 15.0 |
| 64 files of 4 KiB, MD5 | musl | 14 | 0.7208 | 0.7186 | 0.999 [0.994, 1.003] | 34.8 to 34.9 |
| one 512 MiB SHA-256 pin | glibc | 11 | 0.7578 | 0.7875 | 0.962 [0.923, 1.130] | 25.7 to 24.2 |
| one 512 MiB SHA-256 pin | musl | 13 | 0.7600 | 0.7614 | 1.009 [0.983, 1.063] | 70.9 to 68.6 |
| 512 MiB MD5, thread cap 1 | glibc | 8 | 1.5417 | 1.5382 | 0.998 [0.991, 1.010] | 55.0 to 54.0 |
| 512 MiB MD5, thread cap 1 | musl | 12 | 1.5344 | 1.5301 | 1.001 [0.993, 1.004] | 94.5 to 91.5 |
| 512 MiB MD5, split requested | glibc | 12 | 1.5832 | 1.2118 | 0.768 [0.765, 0.771] | 70.0 to 64.5 |
| 512 MiB MD5, split requested | musl | 12 | 1.5263 | 1.1670 | 0.766 [0.765, 0.770] | 104.5 to 108.5 |

the 512 MiB MD5 case wins by 22–23%, with no established time regression in the controls. the first small-file GNU pilot included the switch only on one arm; a confirmation with it set to 0 and 1 on the two arms was flat, 1.001 [0.997, 1.004]. that switch is removed from the kept code. the slow split scheduler was entered in the split-requested case, but these fast transfers could finish before its next stream; a separate checked 64 MiB/s-capped run made three requests and exercised real range readback. its wall time is not in this table.

memory is not claimed neutral. for the main MD5 case, glibc's largest clean child RSS was 95.7 to 90.0 MiB; musl's median rose by 14.1 MiB, while its largest clean peak fell from 168.6 to 154.3 MiB. both run envelopes already extend past the historical ordinary-workload 150 MB target. the measured worst peak did not grow, but the higher median residency is a cost of this change, not memory neutrality. the many-file case used less memory. cap one adds no second lane.

characterization checks use independently generated hashlib vectors, including a prefixed checkpoint, range readback and final SHA-256/MD5. channel-controlled tests cover bounded shared ownership, cancellation/claim lifetime, optional-thread refusal, worker failure and CPU slots becoming busy. the lane-selection test failed on the original code; deliberately losing the saved MD5 state on thread-start refusal failed the independent vector test, then passed after restoration. no test uses elapsed time to prove blocking.

raw commands, binary hashes, checked payload hashes, round records and noise flags of this checksum experiment are kept on the measuring machine under `/datadisk/fl-perf/cpu2/`, with the scripts that reproduce the comparison and its paired analysis. no bandwidth, interface, global cache or global filesystem flush was changed for the test.

## musl bzip2 short memory moves, 2026-11-12

the CPU sweep found a libc-specific bzip2 gap, not a measured fix. on a8f9dad7, release builds with frame pointers and line tables unpacked one 64 MiB member to fresh tmpfs cache staging. five interleaved clean rounds on the EPYC 7V13 gave glibc 2.211 s [2.192, 2.235] and musl 2.843 s [2.817, 3.296]. native GNU tar 1.35 with bzip2 1.0.8 took 2.217 s [2.215, 2.251] in the musl batch. these bounds are observed ranges, not confidence intervals. independent hashes checked the outputs outside the clock.

the static musl user profile put 31.47% of samples in `memmove`, 51.61% in `libbz2_rs_sys::decompress::decompress`, and 14.28% in the wrapper's decompressor. fetchloom uses bzip2 0.6.1 and libbz2-rs-sys 0.2.5. the short move-to-front rotations are an upstream investigation lead; they do not prove which replacement would help. the native command also omits fetchloom's SHA-256, staged publication and archive guards, so its time is not a promised decoder speedup.

no dependency, decoder or allocator is changed, and no upstream issue was submitted. the raw `archive-large-tar.bz2-{glibc,musl}` rounds and `profile-musl-large-bzip` samples remain with the CPU sweep's records. a future fix needs an independent before/after comparison on both libc builds, not the native gap as its acceptance result.

## overlapping archive decoding and storage, 2026-11-12

the original archive reader decoded, wrote and hashed a large member before sending it to a writer for sealing. the first prototype sent the 1 MiB prefix and then reused three 256 KiB buffers while an existing writer wrote and hashed them. it made a real `get` of a 64 MiB gzip member 0.717 [0.695, 0.741] of the glibc time and 0.774 [0.762, 0.785] on musl, twelve clean pairs each. ZIP was 0.816 [0.779, 0.833] and 0.806 [0.782, 0.815]. these are command timings, not the earlier CPU-only ceiling.

that version was dropped. a boundary control with 32 different 1.0625 MiB members took 0.1614 to 0.1647 s on glibc, ratio 1.021 [1.005, 1.044], over 22 clean pairs including a bounded confirmation. its median child RSS was 17.1 to 17.9 MiB. extra memory was not the failure: the handoff's cost was not repaid for short members. the boundary regression remains in the records rather than being hidden by the large-file win.

these runs used separate switched release binaries for glibc and static musl, fat LTO, one codegen unit, abort, frame pointers and line tables, on the same EPYC 7V13 as the checksum measurements. a local HTTP server supplied ZIP, plain tar, tar.gz, tar.zst, tar.xz, tar.bz2 and 7z fixtures with one 64 MiB member, plus a ZIP of 4,000 files of 4 KiB and the boundary control. `get` used a fresh cache/output on ext4 NVMe, warm source files, ordinary defaults and a thread-cap-one control. hashlib checked every output's size and SHA-256 outside the clock. the first setup batch's checker expected a `data/` directory which `get` correctly stripped; those failed-check rows are retained separately and are not measurements accepted here.

`abh.py` randomized the two arms within each round, four pairs per lock acquisition on cores 12–23 at nice 0. builds used cores 0–11, nice 10 and three jobs. a burst on either arm excluded the whole pair. GNU time measured child peak RSS without the Python parent's inherited floor; paired median ratios have 95% bootstrap intervals. all commands, source patches, binary hashes and round records remain with the CPU sweep's archive handoff evidence.

the kept version admits streaming only for a member declared above 4 MiB. shorter members take the original path. the declared size is only a scheduling hint: no buffer is sized from it, and every actual byte still passes the existing guard. the size admission removed the earlier boundary regression; a separate 32-member fixture just beyond the new boundary, at 4.0625 MiB per file, also gained. after resource levels landed, a zero- or one-writer budget was kept inline as well, so single-core and lean runs allocate no new handoff buffers. this fallback is checked directly with paused writers, not claimed as a timed speedup.

| workload | libc | clean pairs | before s | after s | after/before [95%] | median MiB, before to after |
|---|---|---:|---:|---:|---|---|
| 64 MiB ZIP | glibc | 13 | 0.2991 | 0.2448 | 0.816 [0.780, 0.830] | 19.7 to 19.5 |
| 64 MiB ZIP | musl | 12 | 0.2923 | 0.2400 | 0.817 [0.778, 0.840] | 64.3 to 61.3 |
| 64 MiB gzip | glibc | 8 | 0.2853 | 0.2174 | 0.754 [0.751, 0.778] | 19.9 to 19.2 |
| 64 MiB gzip | musl | 8 | 0.2734 | 0.2078 | 0.759 [0.726, 0.774] | 58.3 to 60.3 |
| 64 MiB zstd | glibc | 14 | 0.2328 | 0.2243 | 0.962 [0.952, 0.978] | 19.7 to 19.3 |
| 64 MiB zstd | musl | 12 | 0.2363 | 0.2242 | 0.949 [0.928, 0.970] | 63.5 to 62.5 |
| 64 MiB xz | glibc | 14 | 0.7797 | 0.7178 | 0.926 [0.917, 0.929] | 24.4 to 25.0 |
| 64 MiB xz | musl | 12 | 0.7817 | 0.7149 | 0.909 [0.897, 0.923] | 72.1 to 72.3 |
| 64 MiB bzip2 | glibc | 16 | 2.2748 | 2.2154 | 0.972 [0.966, 0.975] | 21.7 to 21.2 |
| 64 MiB bzip2 | musl | 12 | 3.0295 | 2.9879 | 0.982 [0.978, 0.992] | 62.8 to 68.8 |
| 64 MiB plain tar | glibc | 13 | 0.2998 | 0.3074 | 0.977 [0.957, 1.015] | 37.2 to 32.8 |
| 64 MiB plain tar | musl | 12 | 0.3140 | 0.3120 | 0.971 [0.955, 0.999] | 59.0 to 58.4 |
| 64 MiB 7z | glibc | 10 | 0.8800 | 0.8184 | 0.930 [0.922, 0.940] | 23.3 to 23.6 |
| 64 MiB 7z | musl | 11 | 0.9403 | 0.8725 | 0.936 [0.929, 0.945] | 70.4 to 72.0 |
| ZIP of 32 members of 1.0625 MiB | glibc | 8 | 0.1492 | 0.1509 | 0.993 [0.961, 1.037] | 17.2 to 17.2 |
| ZIP of 32 members of 1.0625 MiB | musl | 12 | 0.1610 | 0.1641 | 1.006 [0.976, 1.031] | 57.5 to 56.4 |
| ZIP of 32 members of 4.0625 MiB | glibc | 15 | 0.5935 | 0.4979 | 0.856 [0.835, 0.869] | 24.9 to 24.2 |
| ZIP of 32 members of 4.0625 MiB | musl | 8 | 0.5872 | 0.5119 | 0.885 [0.856, 0.905] | 72.5 to 71.5 |
| ZIP of 4,000 files of 4 KiB | glibc | 12 | 0.2286 | 0.2247 | 0.982 [0.957, 0.999] | 23.7 to 23.7 |
| ZIP of 4,000 files of 4 KiB | musl | 12 | 0.2302 | 0.2322 | 1.007 [0.964, 1.052] | 80.8 to 78.7 |
| 64 MiB ZIP, thread cap 1 | glibc | 12 | 0.2925 | 0.2938 | 0.995 [0.964, 1.012] | 19.8 to 19.6 |
| 64 MiB ZIP, thread cap 1 | musl | 10 | 0.2388 | 0.2416 | 1.011 [0.977, 1.026] | 50.3 to 49.2 |

large gzip is 24–25% faster and ZIP about 18% faster end to end on both libc builds. no control establishes a time regression; the wider small-file, old-boundary and single-thread intervals do not establish equivalence either. GNU's largest clean child RSS across the fixtures was 38.2 MiB after the change, musl's 88.6 MiB. small median increases in some cases remain well inside the default budget and are shown in the table. the pipeline keeps at most the initial 1 MiB prefix and three 256 KiB reusable buffers for its handoff, not a whole large member.

the second build kept both arms in the same binary again. busy GNU rounds were retained and replaced by bounded quiet batches, with any wait for lower load outside the measure lock. the table pools clean whole pairs only. the default writer budget and thread-cap-one paths used in these A/Bs are unchanged by the subsequently rebased reader-budget and resource-level changes.

the old reader failed the paused-writer offload test. the first prototype failed the boundary-admission test. independent 8 MiB SHA-256 vectors check successful storage and refused-thread fallback; bounded-buffer reuse, interruption and all archive guard tests pass. deliberately replacing interrupted-channel handling with a writer error changed `Interrupted` to `LocalIo` and failed its assertion. deliberately enabling the handoff for a one-writer budget failed the inline-storage assertion; both were restored. the experiment switch is absent from the kept code.

## byte-aware placed-file verification, 2026-11-12

`status --verify` and `sync --verify` gave fewer than 128 files one hashing thread, however large they were. `Survey::verify` now sums the sizes of only the files still needing a check, then uses the shared ordered runner with byte-aware worker admission. the larger of one worker per 64 files and one per 4 MiB can run, bounded by the actual files, available CPUs, eight workers and the command's thread ceiling. ceilings of one or two keep the old count-based choice, so lean adds no hash buffers to a short list. the IO change's same-survey `racy_intact` skip stays intact, including its damaged-file exception.

one switched release binary per libc was built over [the IO/resource-level base](https://github.com/nikhil-yn-aix/fetchloom/commit/7ba99701) plus the checksum and archive changes: fat LTO, one codegen unit, abort, frame pointers and line tables. the test ran real commands on the EPYC 7V13, cores 12–23 at nice 0, with settled projects and independently checked cache/placed files on ext4 NVMe. every output file was checked again with hashlib outside the clock. each pair's complete JSON report had to match after removing only `work.elapsed_ms`; requests stayed at zero. cold runs flushed and evicted only their own eight files, never a global cache. every cold child recorded exactly 1,048,576 input blocks of 512 bytes, the full 512 MiB payload.

`abh.py` randomized both arms in four-pair blocks, with an initial warmup. early blocks took separate leases. to avoid repeated waits behind long network cases, only unfinished blocks were then grouped under bounded leases: the longest was 122.16 s. no accepted pair was repeated. a burst in either arm excluded the whole pair; rows and flags remain in the records. the table gives median times, paired median after/before ratios with 95% bootstrap intervals, and median child peak RSS from GNU time, not the harness parent's inherited floor.

| workload | libc | clean pairs | before s | after s | after/before [95%] | median MiB, before to after |
|---|---|---:|---:|---:|---|---|
| 8 files of 64 MiB, warm | glibc | 9 | 0.4021 | 0.0655 | 0.163 [0.153, 0.204] | 11.8 to 17.9 |
| 8 files of 64 MiB, warm | musl | 12 | 0.3944 | 0.0593 | 0.151 [0.149, 0.153] | 15.4 to 25.7 |
| 8 files of 64 MiB, cold | glibc | 12 | 0.6609 | 0.1734 | 0.263 [0.261, 0.264] | 11.7 to 17.8 |
| 8 files of 64 MiB, cold | musl | 12 | 0.6589 | 0.1731 | 0.262 [0.261, 0.264] | 15.1 to 25.3 |
| 8 files of 64 MiB, lean | glibc | 12 | 0.3953 | 0.3963 | 1.001 [0.998, 1.005] | 11.6 to 11.6 |
| 8 files of 64 MiB, lean | musl | 12 | 0.3988 | 0.3983 | 1.000 [0.996, 1.002] | 14.9 to 14.9 |
| 8 files of 64 MiB, max | glibc | 11 | 0.3947 | 0.0622 | 0.158 [0.154, 0.180] | 11.6 to 17.8 |
| 8 files of 64 MiB, max | musl | 12 | 0.3991 | 0.0597 | 0.150 [0.148, 0.151] | 14.9 to 25.1 |
| 8 files of 64 MiB, thread cap 1 | glibc | 12 | 0.3950 | 0.3953 | 1.001 [0.999, 1.001] | 11.7 to 11.4 |
| 8 files of 64 MiB, thread cap 1 | musl | 12 | 0.3987 | 0.3983 | 1.001 [0.999, 1.003] | 14.9 to 14.9 |
| 8 files of 64 MiB, thread cap 2 | glibc | 10 | 0.3955 | 0.3943 | 0.998 [0.994, 1.002] | 11.6 to 11.7 |
| 8 files of 64 MiB, thread cap 2 | musl | 12 | 0.3989 | 0.3995 | 1.002 [1.000, 1.003] | 14.9 to 14.9 |
| 8 files of 64 MiB, thread cap 4 | glibc | 12 | 0.3966 | 0.1093 | 0.276 [0.275, 0.276] | 11.7 to 14.8 |
| 8 files of 64 MiB, thread cap 4 | musl | 12 | 0.4000 | 0.1074 | 0.268 [0.267, 0.271] | 14.9 to 19.1 |
| 8 files of 4 MiB | glibc | 7 | 0.0348 | 0.0169 | 0.472 [0.389, 0.509] | 11.5 to 17.5 |
| 8 files of 4 MiB | musl | 12 | 0.0323 | 0.0111 | 0.341 [0.339, 0.353] | 14.9 to 25.1 |
| 8 files of 1 MiB | glibc | 7 | 0.0140 | 0.0110 | 0.837 [0.763, 0.894] | 11.8 to 12.9 |
| 8 files of 1 MiB | musl | 12 | 0.0132 | 0.0101 | 0.763 [0.677, 0.780] | 14.9 to 17.1 |
| 8 files of 4 KiB | glibc | 6 | 0.0058 | 0.0058 | 0.981 [0.876, 1.153] | 10.6 to 10.8 |
| 8 files of 4 KiB | musl | 12 | 0.0059 | 0.0059 | 1.009 [0.950, 1.042] | 14.9 to 14.9 |
| one 64 MiB file | glibc | 12 | 0.0560 | 0.0559 | 1.000 [0.996, 1.008] | 11.2 to 11.3 |
| one 64 MiB file | musl | 12 | 0.0569 | 0.0567 | 0.994 [0.993, 1.007] | 14.9 to 15.0 |
| settled status without verify | glibc | 12 | 0.0069 | 0.0068 | 1.005 [0.978, 1.027] | 10.7 to 10.8 |
| settled status without verify | musl | 12 | 0.0072 | 0.0072 | 0.999 [0.985, 1.034] | 15.0 to 15.0 |
| sync --verify, 8 files of 64 MiB | glibc | 12 | 0.4006 | 0.0626 | 0.155 [0.154, 0.173] | 12.6 to 18.8 |
| sync --verify, 8 files of 64 MiB | musl | 12 | 0.4074 | 0.0612 | 0.150 [0.149, 0.153] | 18.0 to 26.3 |

warm 512 MiB verification is 84–85% faster, cold verification about 74% faster. the largest clean candidate RSS across all cases was 19.1 MiB on glibc and 26.3 MiB on musl, within the default budget. the main warm case spends 6.1 MiB more median RSS on glibc and 10.3 MiB on musl; this is reported, not called memory neutral. lean and low thread ceilings retain the original reader count and show no established memory increase. the control intervals do not establish a time regression, but the wider tiny-command bounds are not proof of equivalence.

an unlocked strace check, outside timing, counted exactly 64 MiB read from each of the eight files on both arms: one reading thread before, eight after, 536,870,912 bytes in each. the gain therefore does not come from skipping hashes. worker-choice tests use fixed CPUs, sizes and ceilings. independent 4 MiB SHA-256 vectors check result ordering and refused-thread fallback; interrupted reads stay errors. the count-only policy failed the new scheduling test. deliberately dropping the IO racy-proof filter failed its existing regression, and allowing extra readers under a small ceiling failed the lean-buffer test; restoration passed. the kept path has no experiment switch.

raw commands, binary hashes, source patch, reports, per-file read counts and every paired run remain in the CPU sweep's records of this verification change. these measurements cover warm/cold ext4 NVMe on linux, not rotational disks, network filesystems, native macos/windows or aarch64 throughput; those remain unmeasured rather than inferred from the CPU gain.

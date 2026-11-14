# Process

## a/b passes on placement and startup, 2026-11

the final benchmark found two cases slower than main: a no-op sync and placing from a full cache. measured alone, the build before the changes below took 1.19 times main's time for the no-op and 1.25 times to place 100,000 files, and profiles traced it to three causes. each change ran as a static musl build against the build before it, with main and every build run once a round in a shuffled order: six rounds placing 100,000 files from a full cache, 15 placing 1,000 one-file datasets and 40 no-op syncs of those datasets. a ratio is the median of new over old with its 95% bootstrap interval, and the rule above applied.

| change | commit | result | outcome |
|---|---|---|---|
| placement work per file cut: each folder checked and grouped once, the root stripped once, results put in order without a sort, a worker woken only when one waits | 5911edb | 100,000 files 0.840 [0.832, 0.862]; 1,000 datasets 0.955 [0.948, 0.962]; no-op flat | kept |
| transparent huge pages left on | 914b739 | 100,000 files 0.884 [0.873, 0.889]; 1,000 datasets 0.971 [0.959, 0.977]; no-op 0.918 [0.910, 0.924]; peak memory 1.17, 2.47 and 1.72 | kept |
| mimalloc for libc's own allocations on musl | d2bc3e7 | no-op 0.833 [0.828, 0.838]; 1,000 datasets 1.007 [0.995, 1.015]; 100,000 files 1.017 [1.002, 1.021] | kept |

with all three, against main: 100,000 files 0.944 [0.934, 0.948], 1,000 datasets 1.003 [0.991, 1.017] and the no-op 0.909 [0.905, 0.917].

## the end of a run, 2026-11-12

a run ended its tokio runtime with `shutdown_background`, so `main` returned while the runtime's threads were still exiting: a median of 9 of them at the end of a sync of 32 links. on windows, returning from `main` ends those threads wherever they are and then runs main's thread-local destructors, which under `-vv` take the same process-wide locks the threads take as they exit, and a ci run on fat32 hung after its last line. `end_runtime` now waits for them, at most `EXIT_WAIT`. one experiment build of a8f9dad chose the end at run time, `FETCHLOOM_EXP_EXIT_<ms>=1` for a bound of that many milliseconds and none for `shutdown_background`, and logged how long the end took. abh.py ran the arms in interleaved rounds with the measure lock held, on cores 12-23, or on two of them, alone or each shared with one or three busy loops. tokio joins the threads only when every one lets go within the bound, so an end shorter than the bound joined them all.

| run | build | wall, `shutdown_background` and with a bound | end of the run, median and slowest | joined |
|---|---|---|---|---:|
| sync of 32 links of 100 KB from an empty cache | glibc | 27.69 ms, 27.93 ms | 0.35 ms, 3.51 ms | 180 of 180 |
| the same | musl | 27.86 ms, 27.58 ms | 0.41 ms, 1.68 ms | 180 of 180 |
| settled sync of the 32 | glibc | 4.78 ms, 4.83 ms | 0.09 ms, 3.30 ms | 300 of 300 |
| the same | musl | 4.92 ms, 4.95 ms | 0.13 ms, 1.29 ms | 300 of 300 |
| sync of 32, 2 cores | glibc | 29.38 ms, 29.24 ms | 0.45 ms, 3.08 ms | 120 of 120 |
| settled, 2 cores | glibc | 4.88 ms, 4.97 ms | 0.10 ms, 2.60 ms | 200 of 200 |
| sync of 32, 2 cores with a busy loop each | glibc | 54.12 ms, 55.13 ms | 1.45 ms, 5.20 ms | 120 of 120 |
| settled, 2 cores with a busy loop each | glibc | 9.66 ms, 11.41 ms | 1.59 ms, 1.98 ms | 200 of 200 |
| sync of 32, 2 cores with three busy loops each | glibc | 116.89 ms, 122.18 ms | 1.74 ms, 8.65 ms | 120 of 120 |
| settled, 2 cores with three busy loops each | glibc | 24.31 ms, 24.73 ms | 6.01 ms, 10.09 ms | 200 of 200 |

walls are medians of 30 or 50 runs an arm; "with a bound" pools the bounds tried, which joined alike: 10, 25, 50, 100, 250 and 1,000 ms on 12 cores, 10 to 100 ms on 2. one end, 10.09 ms in the last row, passed 10 ms, so 25 ms is the smallest bound tried that every end fit in. on a loaded machine the wait shows in the wall, by about the end's own time.

a stopped run pays the whole bound when a blocking task outlives it: `status --verify` of a 4 GiB file on tmpfs, stopped by SIGINT once 512 MiB were read, its hash left running as a stop leaves a scan, eight runs an arm:

| end | `Stopped` to exit, glibc | `Stopped` to exit, musl |
|---|---:|---:|
| `shutdown_background` | 0.93 ms | 1.05 ms |
| 10 ms | 10.98 ms | 11.14 ms |
| 25 ms | 25.99 ms | 26.08 ms |
| 50 ms | 51.08 ms | 51.15 ms |
| 100 ms | 101.12 ms | 101.27 ms |
| 250 ms | 251.11 ms | 251.08 ms |
| 1 s | 1,001.12 ms | 1,001.12 ms |

a second SIGINT sent as `Stopped` showed did not shorten the wait at 100 ms or 1 s, since the run's listener goes with the runtime; a second ctrl-c while the command still runs exits 130 from a runtime thread without the wait.

since [tiny commands](#tiny-commands-2026-11-12), only windows waits: linux and macos end the runtime with `shutdown_background` again, and a stopped run there ends about 1 ms after `Stopped`, as the first row of the table above.

## tiny commands, 2026-11-12

commands that take a few milliseconds were 0.1 to 1.9 ms slower than at a8f9dad7: `path` of a 3-file dataset beside 100,000 state records 1.035 on glibc, a settled sync of 3 files 1.115. two causes were found. the end of a run waited for the runtime's threads to exit, as [the end of a run](#the-end-of-a-run-2026-11-12) added for windows. and every command that builds the network, as a settled sync does before it finds every file in place, looked for every key on the machine: each source's token file, `.netrc`, kaggle's files and fetchloom's index of saved keys, `accounts.json`, which it read once for each account, seven times. a key in the credential store would also have been asked for over d-bus. three changes, one commit:

- linux and macos end the runtime without waiting, since they run main's destructors before ending the other threads; windows keeps the wait.
- `Context::net` hands the network the search for keys, which runs when the first HTTPS request needs a key, so a run that sends none reads no key file. the warnings about a `.netrc` others can open and about a key no header can carry come then too.
- a search for keys reads `accounts.json` once: 7 opens to 1 under strace, on a run that searches.

one experiment release build of each libc, the change with two switches added, chose the old end with `FETCHLOOM_EXP_JOIN=1` and the old search for keys, when the network is built, with `FETCHLOOM_EXP_EAGER=1`. it ran beside the stock release builds of a8f9dad7 (glibc sha256 0ddf803c…, musl 33c84fa9…) and of 89cc7ca6, whose source is the same as efbf3adf's, every arm once a round in a seeded shuffled order after a warmup round, on tmpfs, under the measure lock on cores 12-23. a ratio is the median of paired ratios with its 95% bootstrap interval from `bench/ab.py`; rounds whose other load passed 12 cores were dropped for every arm. the fixtures are the audit's: the scale lab's named-100000 project and a project of 3 imported files of 1 to 8 KiB.

against efbf3adf, five arms a round:

| case | libc | pairs | end without the wait | keys when needed | both, and the index once |
|---|---|---:|---|---|---|
| settled sync of 3 files | glibc | 100 | 0.973 [0.962, 0.982] | 0.978 [0.970, 0.984] | 0.958 [0.952, 0.966] |
| | musl | 100 | 0.966 [0.952, 0.975] | 0.971 [0.962, 0.976] | 0.950 [0.942, 0.958] |
| `path` beside 100,000 | glibc | 33 | 0.995 [0.990, 1.000] | 0.999 [0.990, 1.001] | 0.997 [0.987, 1.003] |
| | musl | 33 | 0.997 [0.989, 1.003] | 0.998 [0.991, 1.005] | 0.993 [0.987, 0.996] |

the `path` batch ran beside other agents' builds, and 27 of its 60 rounds were dropped. a second batch of three arms a round:

| case | libc | pairs | a8f9dad7 to efbf3adf | a8f9dad7 to the change | efbf3adf to the change |
|---|---|---:|---|---|---|
| `path` beside 100,000 | glibc | 167 | 1.026 [1.023, 1.029] | 1.019 [1.017, 1.022] | 0.992 [0.991, 0.995] |
| | musl | 167 | 1.014 [1.010, 1.018] | 1.013 [1.009, 1.016] | 0.997 [0.993, 1.002] |
| settled sync of 3 files | glibc | 150 | 1.038 [1.029, 1.041] | 1.017 [1.012, 1.023] | 0.981 [0.975, 0.987] |
| | musl | 150 | 1.022 [1.017, 1.031] | 0.991 [0.985, 1.001] | 0.969 [0.962, 0.976] |
| `--version` | glibc | 150 | 1.006 [1.001, 1.011] | 1.018 [1.008, 1.024] | 1.009 [1.003, 1.017] |
| | musl | 150 | 0.996 [0.991, 1.004] | 1.002 [0.997, 1.008] | 1.002 [0.996, 1.011] |
| `help` | glibc | 150 | 1.013 [1.006, 1.018] | 1.026 [1.020, 1.031] | 1.011 [1.005, 1.018] |
| | musl | 150 | 1.009 [1.000, 1.016] | 1.010 [1.004, 1.015] | 1.007 [0.997, 1.011] |

then the clean release build of the batch this change came in, with the flush groups and the data folder walk's stop that follow it, against the same two, three arms a round:

| case | libc | pairs | a8f9dad7 to efbf3adf | a8f9dad7 to the batch | efbf3adf to the batch |
|---|---|---:|---|---|---|
| `path` beside 100,000 | glibc | 147 | 1.020 [1.016, 1.023] | 1.025 [1.021, 1.027] | 1.004 [1.000, 1.007] |
| | musl | 147 | 1.022 [1.019, 1.025] | 1.003 [1.001, 1.008] | 0.981 [0.977, 0.984] |
| settled sync of 3 files | glibc | 150 | 1.065 [1.058, 1.073] | 1.014 [1.006, 1.020] | 0.951 [0.946, 0.956] |
| | musl | 150 | 1.066 [1.056, 1.077] | 0.992 [0.979, 1.005] | 0.929 [0.921, 0.937] |
| `--version` | glibc | 100 | 1.028 [1.019, 1.040] | 1.012 [1.002, 1.020] | 0.976 [0.969, 0.987] |
| | musl | 100 | 1.048 [1.028, 1.059] | 1.003 [0.996, 1.013] | 0.960 [0.950, 0.973] |
| `help` | glibc | 98 | 1.043 [1.030, 1.050] | 1.026 [1.018, 1.033] | 0.981 [0.967, 0.990] |
| | musl | 98 | 1.041 [1.031, 1.054] | 1.001 [0.985, 1.016] | 0.961 [0.956, 0.976] |

`--version` and `help` run none of the changed code, yet they moved by 1 to 5% between builds and batches: on commands this short, where the binary's code lands and what else the machine does weigh as much as the work. what is left of `path` on glibc is of that kind: a8f9dad7 and the change run the same number of user instructions within 0.5% (288.6 and 289.9 million, `perf stat -r 10`) in the loop that passes over the state's 24 MB, but the binary is 1.2 MB larger, with 15 more page faults and 1.0 MiB more peak memory, and its user time is 1.2 ms longer. the voluntary context switches of `path` fell from 21 to 15, a8f9dad7's 14. the settled sync's peak memory fell 0.1 to 0.3 MiB on glibc; musl's was 16.1 to 16.5 MiB with or without the change, 2.0 to 2.4 MiB above a8f9dad7, which this change did not look into.

the change is kept: it is faster where it aimed, 0.929 to 0.981 on the settled sync, flat on `path`, slower nowhere it runs, and the settled sync is back within 1.02 of a8f9dad7 on both libcs, as is `path` on musl. `path` on glibc is not, at 1.019 to 1.025, for the reason above, which none of the three changes touches. `the_run_ends_without_waiting_for_threads_still_exiting` failed on the old end, and on the new one with `cfg!(unix)` put in place of `cfg!(windows)`; `a_network_that_sends_nothing_looks_for_no_keys` failed on the old search and with the search forced when the network is built.

## a shorter purge delay for lean on musl, 2026-11-12

the static musl build keeps memory mimalloc has freed for 1,000 ms before it hands it back, which is most of why it peaks above glibc. a half-hour spike asked whether lean could hand it back sooner without unsafe code, which the crate forbids.

- a build-time default: mimalloc 3, which libmimalloc-sys 0.1.49 builds unless asked otherwise, writes the 1,000 ms into its table of options with no `#define` around it, so no compiler flag changes it.
- the crate's features: `v2` builds mimalloc 2, whose delay is 10 ms, but for every level and as another allocator, and the perf audit measured 10 ms costing default 1 to 8% of its time.
- a safe call: the mimalloc crate's `extended` feature adds `version` and `stats_json` only. setting an option takes `libmimalloc_sys::mi_option_set`, an unsafe foreign function, and `std::env::set_var` is unsafe in the 2024 edition.

none of them sets a delay for lean alone, so nothing was built. the delay can only come from the environment the process starts with: a person's `MIMALLOC_PURGE_DELAY`, or fetchloom starting itself again with the variable through `CommandExt::exec`, which is safe but replaces the process on every lean run, a choice beyond a spike. what lean would gain, measured with the variable set before the start, a stock musl release build with the [tiny commands](#tiny-commands-2026-11-12) changes, interleaved rounds under the measure lock on cores 12-23, against lean as it is:

| run | rounds | 10 ms: time, peak memory | 0 ms: time, peak memory |
|---|---:|---|---|
| sync of 40,000 one-file datasets into an empty folder | 15 | 1.068 [1.063, 1.076], 184.8 to 160.0 MiB, 0.867 | 1.125 [1.111, 1.136], 150.5 MiB, 0.815 |
| `add -i` of 1,000 links of 90 to 110 kB | 20 | 1.013 [0.992, 1.058], 71.3 to 50.6 MiB, 0.703 | 1.106 [1.069, 1.147], 40.5 MiB, 0.568 |
| status of 100,000 files on ext4 | 20 | 1.027 [1.009, 1.033], 86.1 to 79.3 MiB, 0.922 | 1.067 [1.055, 1.076], 78.2 MiB, 0.908 |

## profile-guided optimization, 2026-11-13

the cpu map's first pgo corpus sped up reading the state but regressed parsing a 10,000-dataset manifest, 1.079. this trained again with a corpus rebalanced toward manifests and measured what linux ships, the static musl build.

an instrumented build ran 54 commands once each, outside the clock: sync, status, list, export and `lock --check` of 40,000 and of 1,000 datasets; sync, status, `status --verify`, `cache verify`, list and export of 10,000 files; `path` and `sync` of one dataset beside 10,000; `add -i` of 2,000 tiny links; a zip of 2,000 members and a tar.gz, tar.xz and tar.zst of 300; a folder of 2,000 files; a 64 MiB `get -o`; and usage, missing-file and no-project errors. the a/b's cases are other instances of the same work. the merged profile held 9.8 MB for 29,838 functions, and the optimized build found a profile for every function.

| case | pairs | time [95%] | peak memory [95%] |
|---|---:|---|---|
| `lock --check` of 10,000 datasets | 20 | 0.901 [0.895, 0.910] | 0.969 [0.969, 0.971] |
| `--json list` of 10,000 datasets | 20 | 0.910 [0.906, 0.914] | 0.971 [0.970, 0.973] |
| `--json status` of 10,000 datasets | 20 | 0.934 [0.932, 0.938] | 0.967 [0.967, 0.968] |
| sync of 10,000 datasets into an empty folder | 12 | 0.949 [0.946, 0.954] | 0.972 [0.964, 0.974] |
| `--json status` of 100,000 files | 20 | 0.937 [0.928, 0.965] | 0.984 [0.979, 0.987] |
| settled sync of 100,000 files | 20 | 0.954 [0.947, 0.961] | 0.980 [0.980, 0.981] |
| `cache verify` of 100,000 objects | 20 | 1.002 [0.985, 1.009] | 0.991 [0.991, 0.995] |
| `add -i` of 1,000 links from a local server | 13 | 0.945 [0.907, 0.967] | 0.948 [0.913, 1.019] |
| sync of a zip of 10,000 files | 15 | 0.959 [0.928, 0.974] | 0.961 [0.946, 1.010] |
| settled sync of 3 files | 40 | 0.973 [0.959, 0.997] | 0.836 [0.836, 0.848] |

so the rebalanced corpus turned the manifest's regression into the largest gain, and nothing slowed. the binary is 14,879,664 bytes against 17,472,432, 14.8% or 2.5 MiB smaller, about what a tiny command's peak memory fell. but no case clears the 10% bar: `lock --check` comes closest, with an interval reaching 0.910.

the release builds each binary so that rebuilding the commit gives the same bytes, and a profile trained in the release job would break that. a second training of the same corpus, by the same binary on the same inputs, merged to another profile, 99.957% of its edge counts overlapping the first, since threads and randomly seeded maps move the counts; the binary built from it differed from the first in 13,666,826 bytes. a committed profile for each of the six release targets, trained on each and redone as the code moves, would be the only way to keep the release reproducible. that was not built: dropped, below the bar, with that cost on top.

three musl release builds of df4d2a5b from one source tree, control, instrumented and optimized, the flags passed through `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS` as the release workflow passes its own; rustc 1.99.1 with llvm 23.1.1. interleaved rounds after a warmup under the measure lock on cores 12-23, projects and caches on tmpfs, the copied projects synced once by the control build first, so both builds read the indexed state. `lock --check` and `list` lost most rounds to other users' load; their rows keep all 20, and the clean rounds alone give 0.910 for both.

## tiny commands at the release candidate, 2026-11-13

the release recheck ran the audit's headline and tiny cases with the release candidate 7daf4747 against a8f9dad7. every case that moves data was flat or faster on both libcs; a zip of 10,000 files on glibc came out at 1.023 [1.009, 1.045] in a first batch and flat in two more, 1.007 [0.993, 1.014] beside an a/a arm at 1.000 [0.989, 1.016]. three commands of a few milliseconds were slower on glibc in both of their batches. the shipped musl build was not slower on any of them.

the builds: on glibc, a8f9dad7's stock release (sha256 `0ddf803c…`) against a local `cargo build --release --locked` of 7daf4747 (`b174ccbe…`); on musl, a stock release build of a8f9dad7 against the stage run's x86_64-unknown-linux-musl package of 7daf4747 (`855bd7ce…`), which a local build with ci's flags reproduces but for mimalloc's build-time stamp. the a/a arm is a byte-identical copy of the glibc a8f9dad7 at another path, in the same rounds. every arm ran once a round in a seeded shuffled order after a warmup round, under the measure lock on cores 12–23, on tmpfs. a ratio is the median of paired ratios with its 95% bootstrap interval from `bench/ab.py`.

| command | libc | pairs | first batch | second batch | a/a, second batch |
|---|---|---:|---|---|---|
| `help` | glibc | 100, 200 | 1.030 [1.020, 1.037] | 1.036 [1.032, 1.041] | 1.005 [1.000, 1.012] |
| `--version` | glibc | 100, 200 | 1.020 [1.014, 1.029] | 1.016 [1.011, 1.021] | 1.000 [0.995, 1.007] |
| settled sync of 3 files | glibc | 100, 200 | 1.010 [1.004, 1.017] | 1.017 [1.011, 1.022] | 1.005 [1.000, 1.009] |
| `help` | musl | 100 | 1.002 [0.992, 1.018] | | |
| `--version` | musl | 100 | 0.999 [0.988, 1.012] | | |
| settled sync of 3 files | musl | 100 | 0.982 [0.971, 0.997] | | |

the glibc medians were 2.5, 2.9 and 3.9 ms, so the difference is 0.04 to 0.1 ms, and gnu time's own fork and exec inside the clock make the ratios understate it. medians of 20 runs of `perf stat` say where it comes from: user instructions 1.43 to 1.49 million for `--version` (+4.2%), 3.02 to 3.26 million for `help` (+8.0%) and 3.13 to 3.35 million for the sync (+6.9%); page faults 254 to 271, 268 to 294 and 314 to 344. the system calls are the same but for two `ioctl`s, and the dynamic loader does the same work, 13,687 to 14,865 relative relocations in about 0.05 ms either way. the glibc binary grew from 15.5 to 17.1 MB. musl runs the same extra instructions with fewer page faults (155 to 143, 160 to 148, 219 to 196) and is not slower. the extra work fits the larger command line, which `help` builds whole, and the larger binary's pages; a profile with symbols of both commits would name the functions. not chased: linux ships the musl build, and the cost is under 0.1 ms a command.

## mimalloc's huge pages at the release candidate, 2026-11-13

the release recheck found the musl build's peak memory rounded up by huge pages: mimalloc reserves a 1 GiB arena and asks for transparent huge pages on it with `madvise(MADV_HUGEPAGE)`, which ubuntu's `madvise` setting honors, so each 2 MiB stretch a command touches costs 2 MiB, and the peak moves between runs by whole pages (a 100,000-file status 83.6 to 103.6 MiB on one binary). [transparent huge pages left on](#ab-passes-on-placement-and-startup-2026-11) decided this once, with the process's own `prctl`; this a/b asks again of mimalloc's own switches, which leave other programs alone. dropped again: memory falls by a third to two thirds, and the work over many files gets 10 to 23% slower.

three arms in the same rounds, each case one batch after a warmup round, the arms in a seeded shuffled order, under the measure lock on cores 12–23, on tmpfs: the stage run's musl package of 7daf4747 as it ships; the same binary with `MIMALLOC_ALLOW_THP=0`, which skips the `madvise` and turns huge pages off for the process; and a musl build of c7593e78 with mimalloc's `no_thp` feature, made as ci makes it, whose only change is the missing `madvise` (strace shows none). a ratio is off over on, the median of paired ratios with its 95% bootstrap interval from `bench/ab.py`. the owner's training and other builds held some cores throughout, so most rounds carried other load; it falls on all three arms of a round alike.

| workload | pairs | time, variable | time, no_thp build | peak MiB: on, variable, build |
|---|---:|---|---|---|
| `--json status` of 100,000 files | 20 | 1.176 [1.138, 1.202] | 1.173 [1.134, 1.190] | 89.6, 83.9, 81.9 |
| settled sync of 100,000 files | 20 | 1.134 [1.118, 1.154] | 1.116 [1.084, 1.156] | 96.6, 81.2, 79.3 |
| placing 100,001 files from a full cache | 12 | 1.133 [1.119, 1.153] | 1.119 [1.101, 1.147] | 203.6, 169.3, 170.4 |
| sync of 40,000 one-file datasets | 10 | 1.146 [1.136, 1.163] | 1.133 [1.125, 1.154] | 209.0, 183.5, 182.0 |
| settled sync of 1,000 one-file datasets | 60 | 1.154 [1.147, 1.160] | 1.154 [1.144, 1.161] | 31.0, 14.7, 12.7 |
| `lock` merge of 1,000 datasets | 12 | 1.098 [1.080, 1.112] | 1.087 [1.068, 1.117] | 41.5, 25.0, 22.6 |
| `drop` across 1,000 datasets | 12 | 1.230 [1.212, 1.239] | 1.220 [1.208, 1.252] | 44.0, 23.1, 20.8 |
| `cache verify` of 100,000 objects | 20 | 1.041 [1.028, 1.073] | 1.011 [0.987, 1.040] | 50.2, 32.2, 31.1 |
| `status --verify` of 8 × 64 MiB | 20 | 1.017 [0.992, 1.036] | 1.224 [1.031, 1.390] | 26.5, 16.8, 15.0 |
| `add -i` of 1,000 links | 25 | 1.030 [0.990, 1.048] | 1.024 [1.003, 1.093] | 94.7, 35.1, 33.1 |
| sync of a zip of 10,000 files | 20 | 1.037 [0.967, 1.099] | 1.024 [0.989, 1.062] | 126.8, 61.0, 60.5 |
| 1 GiB `get` | 12 | 1.010 [1.007, 1.028] | 1.012 [1.008, 1.018] | 94.9, 62.9, 59.4 |
| `path` of 1 file beside 100,000 records | 60 | 0.984 [0.978, 0.994] | 1.021 [1.016, 1.034] | 16.7, 9.4, 7.1 |
| settled sync of 3 files | 150 | 0.998 [0.990, 1.004] | 1.019 [1.015, 1.027] | 16.6, 9.2, 7.3 |
| `--version` | 150 | 1.000 [0.991, 1.009] | 1.018 [1.011, 1.022] | 10.6, 5.2, 4.4 |
| `help` | 150 | 1.027 [1.019, 1.034] | 1.039 [1.031, 1.049] | 11.4, 5.9, 4.6 |

the cost is page faults: with 4 KiB pages the same memory faults far more often. `perf stat` over 10 runs of the shipped binary, huge pages on and then off by the variable: the no-op over 1,000 datasets 204 to 1,595 faults and 18.1 to 21.6 ms of cpu, the status of 100,000 files 707 to 15,564 faults and 673 to 735 ms. the two switches behave alike here; the build's small extra on the shortest commands is its own binary, another commit's layout. on a system set to `always`, the feature would leave the kernel's huge pages on where the variable turns them off. huge pages stay on. turning them off would not bring the two musl peaks above [the 150 MB budget](../budgets.md) under it either: placing 100,001 files, 203.6 to 169.3 MiB, and the sync of 40,000 datasets, 209.0 to 183.5 MiB.

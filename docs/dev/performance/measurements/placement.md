# Placement

## flushing a few files on a busy disk, 2026-11-09

the readme's first `add`, 18 KiB from hugging face, took 14 s on this machine's os disk, which a training job kept busy: the batch's one `syncfs` waits for everything any program wrote to the filesystem, and one took 16.7 s. the final benchmark measured d2bc3e7, and this change was measured on its own, below. the other commits since change nothing the lab times but a check for a git repository after each sync, a few system calls.

the flush alone, after the same writes on the lab's disks, with the median of seven rounds for one file and nine for 64:

| flush | ext4 | xfs |
|---|---:|---:|
| one 1 GiB file: fsync, then one `syncfs` | 1006.9, 1004.9 ms | 925.8, 927.5 ms |
| 64 files of 64 KiB: fsync each on 8 threads, then one `syncfs` | 7.6, 4.1 ms | 14.4, 5.8 ms |

on the busy os disk, one `syncfs` took a median 424 ms after one small file and up to 14.9 s, where fsync for each of 64 files took 13 to 190 ms.

a sync of four small files from a local server, each build once a round in a shuffled order, 15 rounds: on the busy os disk 684 ms (worst 1,860) before and 185 ms (worst 216) after, 0.264 of the time; on the lab's idle nvme 17 ms either way, 0.973.

| change | commit | result | outcome |
|---|---|---|---|
| the first 64 files of every batch flushed on their own | none kept | ext4 s1 1.078 [1.064, 1.303] in seven rounds; peak memory ext4 s2 1.151 [1.028, 1.266] and s3 1.078 [1.002, 1.146]: those 64 flushes held threads in every large batch | dropped |
| a run of up to 64 downloads, copies or archive members flushed one by one, larger runs as before | 4639987 | ext4 s1 0.985 [0.894, 1.041] and xfs s1 1.004 [0.988, 1.025] in 15 rounds; ext4 s4 1.003 [0.909, 1.199], peak memory 1.033 [0.984, 1.110]; s2, s3, s5 and s6 run the same code as before | kept |

## the cache report on 100,000 placed files, 2026-11-11

`fetchloom cache` reads each registered project's state and looks once at every placed file whose object the cache holds, to count copies. the fixture: two projects that each imported a folder of 50,000 files of 60 to 600 bytes into one cache, so 100,000 objects and 100,000 placed files, all hardlinks, on ext4. release builds of f5de3fe with two temporary switches, one leaving the copies pass out and one running it on `place::run_all`, and of the change itself; `fetchloom --json cache` under `/usr/bin/time`, each variant once before the rounds and then once a round in a rotated order, nine rounds. the machine was shared: ubuntu 24.04 on 24 cores of an AMD EPYC 7V13, with a load average of 43 to 63 from other work.

| report | median | range | peak RSS |
|---|---:|---:|---:|
| the needs walk and the object scan, as before the copies pass | 0.42 s | 0.41 to 0.43 s | 51.8 MiB |
| with the copies pass on one thread | 0.82 s | 0.81 to 0.85 s | 76.2 MiB |
| with the copies pass on `run_all`, at most eight threads | 0.60 s | 0.59 to 0.62 s | 76.2 MiB |

the pass took 0.40 s on one thread and 0.18 s on `run_all`, so the report runs it there: 0.73 of the one-thread time, with the same peak memory, which a project's parsed state adds. caches were warm; a cold disk or a network drive makes each look wait longer.

## pushing into a folder mirror, 2026-11-11

`push` first flushed each file it wrote before reading it back and naming it, and on a busy disk that flush took about 20 ms a file. the change writes all of a push's files, puts them on the disk by the cache's batch rule, then reads each back and names it; both ways flush the folders that gained names at the end. one release build ran both behind a switch, both in every round, the order swapped each round, each run into a new mirror folder after a `syncfs` of the disk and 2 s of rest. the mirror, the cache and the project were on the os disk, ext4, of the shared 24-core machine, which other work kept at a load average of 44 to 62. every run ended with status 0, every file under its name and nothing left in `tmp/`, and in the first two rounds every file was hashed against its name. a ratio is the median, over rounds, of the time together over the time with each file flushed, with its 95% bootstrap interval.

| workload | rounds | each file flushed, median and range | together, median and range | ratio |
|---|---:|---|---|---|
| 10,000 files of 9 to 13 bytes, first push | 6 | 36.73 s, 34.72 to 57.42 | 2.71 s, 2.06 to 5.67 | 0.074 [0.058, 0.121] |
| 100 files of 10 MiB | 6 | 32.74 s, 26.69 to 61.59 | 20.02 s, 18.92 to 21.05 | 0.619 [0.378, 0.717] |
| one file of 1 GiB | 12 | 20.62 s, 17.26 to 42.56 | 19.04 s, 17.81 to 37.05 | 0.988 [0.888, 1.065] |

one file is under the batch rule's 64, so both ways flush it on its own; its row measures reading it back through a new handle after closing the one that wrote it, which cost nothing measurable. system cpu time was 2.91 s against 3.05 s for the small files, and the same for the large ones. built as committed, at a load average of 47, a first push of the 10,000 files took 2.58 to 2.93 s in three runs, and a second push, which finds each file with one look at its name and writes nothing, 0.02 s in nine.

## a second run on a shared cache, 2026-11-12

j6-m4's shape at a smaller size: run A syncs a pinned 1 GiB file and 1,000 files of about 100 KB from a server on this machine paced to 40 MB/s, into an empty cache; 3 s later run B syncs the 1,000 files in another project on the same cache. before, A named its finished downloads 256 at a time or when every one of its downloads ended, so B waited on A's last group until A's large file was done. one release build of 28c136b with the change behind a switch ran each variant in every round, the order rotated each round; three rounds each, the bound's own rows two. nvme is ext4 on the machine's nvme drive; root disk is ext4 on its shared cloud disk, where a `dd` left 3 GB of another program's unwritten data just before A started.

| disk | group waiting at most | flush | B, median and range | A, median |
|---|---|---|---|---:|
| nvme | no limit (before) | filesystem | 26.44 s, 26.43 to 26.44 | 29.22 s |
| nvme | 1 s | filesystem | 12.87 s, 11.86 to 12.88 | 29.15 s |
| nvme | 1 s | each file of the group | 12.11 s, 11.87 to 20.15 | 29.19 s |
| nvme | 0.5 s | filesystem | 12.12 s, 12.12 to 12.12 | 29.16 s |
| nvme | 2 s | filesystem | 13.37 s, 13.37 to 13.37 | 29.20 s |
| root disk, 3 GB unwritten | no limit (before) | filesystem | 56.99 s, 56.63 to 72.41 | 59.86 s |
| root disk, 3 GB unwritten | 1 s | filesystem | 50.41 s, 49.97 to 51.68 | 59.19 s |
| root disk, 3 GB unwritten | 1 s | each file of the group | 54.31 s, 51.02 to 55.09 | 59.08 s |

B can take a file only once A has downloaded it, so its wait is A's time for the 1,000 files plus the bound. on the root disk both runs end with a flush of the whole filesystem, which waits for the other program's 3 GB whichever way a waiting group flushes; flushing the filesystem early wrote part of it sooner, and B ended 3.9 s before flushing each file would have let it. the same build, one run alone on nvme:

| workload | no limit | 1 s, filesystem | 1 s, each file |
|---|---:|---:|---:|
| 10,000 files of about 100 KB, unpaced | 3.90 s, 3.80 to 4.50 | 3.96 s, 3.83 to 3.99 | 4.29 s, 4.00 to 4.68 |
| 300 files paced to 4 MB/s | 7.46 s, 7.46 to 7.49 | 7.42 s, 7.28 to 7.48 | 7.45 s, 7.42 to 8.73 |

the change keeps a 1 s bound and flushes a waiting group as a full one is flushed, with one `syncfs`: B's wait fell by half on nvme and by a tenth on the busy root disk, A and the runs alone did not change beyond their noise, and a group flushed file by file was not reliably faster for B on nvme, one of its runs taking 20.15 s, and was slower on the busy disk and with many files.

## flush groups under a low open-file limit, 2026-11-12

a run of more than 64 downloads names its finished downloads in groups after one flush, and each download waiting for its group keeps its claim's file open. groups were 256 whatever the process could open, so under a hard limit of 256 open files `add -i` of 1,000 links failed with exit 50, "could not write to the cache: too many open files", on a8f9dad7 and efbf3adf alike, glibc and musl. a soft limit alone is raised at start, so only a hard one this low fails. a group is now a quarter of `Machine::open_files`, at most 256.

one run each of `add -i` of 1,000 links of 90 to 110 kB from a local server, glibc, on tmpfs, under the measure lock; open files sampled from `/proc/<pid>/fd`, flushes counted with `strace -f -c`:

| build | open-file limit | exit | most files open | `syncfs` | `fsync` |
|---|---:|---:|---:|---:|---:|
| efbf3adf | 1,048,576 | 0 | 320 | 4 | 18 |
| the change | 1,048,576 | 0 | 325 | 4 | 18 |
| the change | 1,024 | 0 | 329 | 4 | 18 |
| the change | 256 | 0 | 117 | 16 | 18 |
| a8f9dad7, efbf3adf, glibc and musl | 256 | 50 | | | |

a limit of 1,024 or more keeps groups of 256, so ordinary runs do what they did: efbf3adf against the clean release build of this batch, 1,000 links in interleaved rounds, 1.000 [0.992, 1.021] on glibc (30 pairs) and 0.987 [0.973, 1.006] on musl (28 pairs), peak memory 1.008 and 1.025 [0.971, 1.049]. under a low limit a run flushes four times as often, which costs a busy disk more; tmpfs could not time that. `downloads_wait_in_groups_a_low_open_file_limit_can_hold` holds 1,000 finished downloads in a batch under a soft limit of 256: with groups of 256 it failed with "too many open files", and again with the quarter taken out.

## scans inside per-file loops, 2026-11-12

a code review found seven loops that search a list once for each file or target. each is quadratic only when its trigger is large, so these ran each trigger at the checklist's scales, 40,000 datasets or 10,000 files among 100,000, with projects made for the purpose. every one of these runs took seconds, except `add -i`, whose whole datasets are searched once each, and that change was dropped.

| trigger | command | before | after | ratio [95%] | pairs |
|---|---|---:|---:|---|---:|
| 40,000 datasets set to `place = false` after they were placed | `status` | 4.42 s | 1.38 s | 0.315 [0.308, 0.317] | 6 |
| 40,000 folders where the datasets' files belong | `status` | 14.36 s | 1.54 s | 0.108 [0.107, 0.109] | 5 |
| 40,000 such folders, each holding a file of someone's | `status` | 23.83 s | 1.75 s | 0.073 [0.072, 0.078] | 4 |
| the same | `sync --dry-run` | 18.70 s | 1.55 s | 0.084 [0.079, 0.085] | 4 |
| 10,000 of 100,000 hardlinked files edited through another link, their times kept | `sync --verify --dry-run` | 2.50 s | 1.02 s | 0.403 [0.372, 0.426] | 8 |
| 100,000 files the cache lacks, 10,000 of them edited, so the data folder holds other bytes | `push` | 12.45 s | 8.67 s | 0.699 [0.691, 0.704] | 5 |
| 40,000 files where the datasets' folders belong | `sync --force` | 12.13 s | 5.40 s | 0.445 [0.436, 0.461] | 4 |
| 40,000 links of 64 to 256 B from a local server | `add -i` | 11.59 s | 11.56 s | 1.013 [0.985, 1.017] | 3 |

the folders blocking a status were three searches at once, so one experiment build chose each old search at run time, `FETCHLOOM_EXP_SCAN4`, `5` and `8`: with only the listing of blockers searching, 5.04 s; with only the search of the listing for what a folder holds, 7.26 s; with only the search for what removing frees, 10.45 s on the folders that hold a file and 10.22 s for `sync --dry-run`. so each costs seconds of its own.

glibc release builds of 50fef54d and of the change, the push and the forced sync on fresh copies each run, the others read only; projects on ext4 over nvme copied from the scale lab's datasets-40000 and files-100000, the edits made with python. rounds were interleaved after a warmup, under the measure lock on cores 12-23, few because each old run took seconds; the add's third round had a load burst. peak memory fell or held on every row but the dry run's, 140.8 to 143.7 MiB, the sets a run with 40,000 blockers builds. ordinary runs build none of them: status of 100,000 files on ext4 1.006 [0.993, 1.017] (30 pairs, under load), a sync placing 40,000 datasets 0.990 [0.963, 1.000], peak memory 0.980 and 0.989.

each change keeps every decision, so the tests pin the decisions and fail when the code is broken on purpose: `a_folder_in_the_way_is_freed_only_when_removing_files_empties_it` with the check for what else a folder holds taken out; `a_changed_copy_skips_names_locked_files_take` with the set ignored; `a_file_set_aside_is_found_by_its_path` with late entries unindexed; `status_names_a_file_in_a_folders_way_once` with every blocker listed; the existing `whatever_stands_in_a_files_way_is_found_from_the_scan_alone` with every folder said to hold nothing; three status tests with the unplaced set inverted; and two of `copied`'s push tests with the refused set inverted.

## the syscalls a new object costs, 2026-11-12

the perf-io2 branch left a half-built change that asks the file system less while storing and placing a file. a file's size, and a reflink's mode, come from the check its open already made; a new object reads its details once when it is made, for both its start and its birth, and seals with that birth instead of asking again; and on linux the cache opens a new object to read as well as write, so a clone is hashed through that file, rewound, instead of opened again by its path. `strace -c -f` of an import of 10,000 files on xfs counted 413,887 syscalls before and 373,860 after: 30,001 fewer `statx`, 10,001 fewer `openat` and as many fewer `close`, 10,001 more `lseek`. that is 4 of the 41 syscalls a file costs, but 1.8% of the cpu and none of the time.

| run | pairs | time [95%] | cpu [95%] | peak memory [95%] |
|---|---:|---|---|---|
| import of 10,000 files of 1 to 8 KiB on xfs | 19 | 1.021 [0.980, 1.036] | 0.982 [0.979, 0.986] | 1.001 [0.993, 1.013] |
| the same on ext4 | 20 | 0.995 [0.985, 1.011] | 0.993 [0.989, 0.996] | 1.003 [0.994, 1.010] |
| a sync placing them again by reflink on xfs | 20 | 1.004 [0.991, 1.017] | 0.999 [0.994, 1.004] | 0.995 [0.986, 1.008] |
| `add -i` of 1,000 links of 90 to 110 kB on tmpfs | 20 | 0.997 [0.975, 1.022] | 1.009 [0.986, 1.018] | 1.007 [0.990, 1.018] |

one glibc release build of the [scans](#scans-inside-per-file-loops-2026-11-12) change with this one behind a run-time switch, `FETCHLOOM_EXP_CHECKED_IO`, against itself without it; rounds interleaved after a warmup under the measure lock on cores 12-23, on ext4 over nvme and on xfs loop-mounted there. one xfs import round had a load burst and is left out. nothing nears the 10% bar, so the change and its two tests were dropped; the patch stays outside the repository.

## flush groups on a slow device, 2026-11-13

a run of up to 64 files is flushed one file at a time, since one `syncfs` also waits for everything other programs left unwritten on the filesystem ([busy disk](#flushing-a-few-files-on-a-busy-disk-2026-11-09)). on a slow device that holds nothing else, one `syncfs` would be far cheaper. this asked whether a rule can tell the two apart.

one experiment build chose the run size below which files flush alone at run time, `FETCHLOOM_EXP_FLUSH_ALONE`: 64 as now, 0 for one `syncfs` always, or `quiet`, one `syncfs` only when /proc/meminfo's `Dirty` and `Writeback` add up to less than 16 MiB as the run starts. with one `syncfs`, an import of 32 files made 18 fsyncs and one `syncfs` instead of 50 fsyncs, and so did 32 links; a zip of 32 made 21 and two instead of 54. a sync placing 32 files made 3 fsyncs either way, since its placements are links to objects already flushed, so it was not timed.

| run, project and cache on the device | rounds | one `syncfs` [95%] | `quiet` [95%] |
|---|---:|---|---|
| import of 32 files of 4 KiB, slow device | 15 | 0.331 [0.307, 0.333], 784 to 212 ms | 0.993 [0.979, 1.005] |
| `add` of a zip of 32 members of 4 KiB, slow device | 15 | 0.820 [0.813, 0.820] | 0.999 [0.996, 1.005] |
| `add -i` of 32 links of 90 to 110 kB, slow device | 15 | 0.947 [0.944, 0.957] | 1.001 [0.984, 1.004] |
| the import, busy os disk | 20 | 0.647 [0.547, 0.759] | 1.024 [0.985, 1.064] |
| the zip, busy os disk | 20 | 1.168 [1.025, 1.239] | 1.047 [0.915, 1.094] |
| the links, busy os disk | 20 | 2.097 [1.848, 2.577], 232 to 515 ms | 0.977 [0.869, 1.056] |

the slow device is a 2 GiB ext4 image of mine on dm-delay, 2 ms a read, 2 ms a write and 8 ms a flush, behind a loop device with direct I/O on the nvme disk, with nothing else on it. the os disk is the network disk the earlier busy case came from, which the kernel reports as rotational, with other users writing to it. glibc, interleaved rounds after a warmup under the measure lock on cores 12-23, the sources and the link server on tmpfs. other users' load flagged a quarter of the rounds; these ratios keep them, and the clean rounds alone give the same picture.

so one `syncfs` saves up to two thirds where nothing else waits to be written, and on the busy disk it made the links twice as slow. the `quiet` rule never chose it on the slow device, where none of its 15 imports came near 210 ms: the two runs whose record survives saw 111 and 125 MiB of other users' unwritten data, and sampled every 5 s over a minute that ranged from 5 to 1,294 MiB. a higher bar would choose one `syncfs` on the busy disk too, since nothing unprivileged says which filesystem holds the data: the per-device writeback counts are in debugfs, and ext4's count of delayed blocks covers ext4 alone. dropped; runs of up to 64 files still flush alone.

## hardlinks before reflinks on xfs, 2026-11-13

on xfs a reflink of a small file costs far more than a hardlink. read-only placements on windows already try a hardlink first; one experiment build, `FETCHLOOM_EXP_HARDLINK_FIRST`, did the same on linux, for read-only placements outside synced folders as on windows. every placed file then had two names, its own and the cache's object, where with reflinks each had one.

| run on xfs, project and cache there | pairs | time [95%] | cpu [95%] | peak memory [95%] |
|---|---:|---|---|---|
| sync of a zip of 10,000 files of 4 KiB into a new cache | 15 | 0.776 [0.751, 0.782], 1.549 to 1.198 s | 0.590 [0.582, 0.599] | 0.981 [0.951, 1.068] |
| sync placing 10,000 files of 1 to 8 KiB the cache holds | 15 | 0.358 [0.333, 0.469], 461 to 166 ms | 0.259 [0.255, 0.266] | 1.000 [0.979, 1.002] |

a musl release build of df4d2a5b with the switch, interleaved rounds after a warmup under the measure lock on cores 12-23, xfs loop-mounted on the nvme disk and shared with other work, which flagged most rounds as loaded; the clean rounds alone give 0.767 [0.751, 0.781] in 9 pairs and 0.484 [0.409, 0.539] in 6.

the speed is the isolation. a reflink shares blocks until either side writes, so an edit to a placed file, after a chmod or by a program that ignores the read-only bit, leaves the cache and every other project as they were. a hardlink is the cache's object, so the same edit changes the object and every project's link to it at once, and fetchloom can only find it on the next run, put the pinned bytes back and keep the changed copy beside them. ext4, which has no reflinks, already works this way, and xfs would join it. the [placement order](../../decisions/README.md) puts reflinks first for that isolation, so a speed rule cannot trade it away: not adopted, a placement decision to make with these numbers.

## short-copy EOF dispatch, 2026-11-12

the operating-system sweep of a8f9dad7 found that `copy_stoppable` calls `io::copy` again after a short completed 1 MiB block has already reached EOF. an optimized standalone Rust reproduction of that loop copied 10,000 distinct 4 KiB files, one binary with a runtime switch, each arm once per seeded shuffled round after a warmup. every output was checked outside the clock. these are primitive measurements, not timings of a changed fetchloom command.

| filesystem | rounds | current loop, median and range | short-block stop, median and range | new/old [95%] |
|---|---:|---|---|---|
| ext4 over nvme | 10 | 0.454 s, 0.449 to 0.464 s | 0.429 s, 0.421 to 0.438 s | 0.941 [0.932, 0.949] |

strace counted one fewer `copy_file_range`, one fewer `sendfile` and two fewer `statx` calls per file, 40,000 calls across this fixture. tmpfs medians suggested about 11% less time, but the first batch and its repeat both contained unrelated-load bursts, so neither supplied a clean acceptance result. the memory fields of those early runs also inherited the harness parent's RSS floor and are not memory-regression evidence.

the ext4 gain is below the 10% keep rule, and there is no checked whole-command win. the change is dropped; the copy loop and its 1 MiB cancellation boundary stay unchanged. raw commands, counts and both noisy batches remain with the sweep outside the repository.

## an import of 100,000 files, 2026-11-12

`add <folder>` kept the list of files its walk found, each with its absolute path in the folder, until the end of the command, though the line telling what it copied was its last use: the list stayed alive while the import was pinned and its files placed, which is where the peak of a large import falls. the scale map's replay of 100,000 files found 12.4 MB of those paths and 7.3 MB of the list's spare room still live at the peak. the list is now freed once that line is written. the import tests hold the lines, warnings and pins it gives.

the fixtures are the scale map's files-n folders, hardlinked into a folder of their own, run as `--json --offline --yes add <folder> --as imported` into an empty project with an empty cache of its own on ext4 over nvme, both made again before each run and deleted after it, outside the clock. one release build chose the old or new way by `FETCHLOOM_EXP_IMPORT_DROP`, measured as in [a merge of many datasets](lock-and-names.md#a-merge-of-many-datasets-2026-11-12): 30 rounds at 1 and 10 files, 30 and 20 at 10,000 on glibc and musl, 16 at 100,000. peak memory is the run's maximum RSS, from wait4.

| files | build | wall, old to new | new/old | peak memory, old to new | new/old |
|---:|---|---|---|---|---|
| 1 and 10 | glibc | 8 and 13 ms, the same | 1.002 [0.989, 1.018], 1.010 [0.975, 1.024] | 14.0 MiB, the same | 1.000 |
| 1 and 10 | musl | 13 and 12 ms, the same | 0.979 [0.974, 1.012], 0.999 [0.990, 1.014] | 18.6 MiB, the same | 1.000 |
| 10,000 | glibc | 0.427 to 0.428 s | 1.002 [0.990, 1.013] | 31.9 to 30.4 MiB | 0.955 [0.948, 0.961] |
| 10,000 | musl | 0.433 to 0.437 s | 1.007 [0.992, 1.026] | 118.5 to 120.7 MiB | 0.974 [0.953, 1.017] |
| 100,000 | glibc | 4.61 to 4.64 s | 0.986 [0.979, 1.011] | 180.9 to 160.7 MiB | 0.890 [0.887, 0.890] |
| 100,000 | musl | 4.30 to 4.34 s | 1.001 [0.988, 1.020] | 216.3 to 194.6 MiB | 0.908 [0.898, 0.927] |

kept: 11% less peak memory for 100,000 files on glibc and 9% on musl, below the replay's 17% of heap, with no time interval away from 1. the import's time is mostly copying the files into the cache.

## conditional mirror staging shards, 2026-11-12

an earlier unconditional staging experiment saved time at 100,000 files but lost at 50. this experiment counted the mirror entries still needing a copy, not all the project's files. at the chosen threshold it prepared 16 checked subfolders below `tmp/`, selected one from each item's hash, and kept the existing flat temporary-name path below it. publication, read-back hashing, flushing and the worker limits stayed unchanged. stale cleanup also visited just the reserved shard folders, without following a listed link or touching unrelated folders.

the threshold pilot used 32-byte distinct files already checked into one ext4 cache, a fresh mirror for each arm on ext4 or a mounted xfs image over nvme, and whole `push --offline` commands. one switched release binary per libc, based on 7ba9970135eb050dcb2db1edb63b7ba867f07b93, ran each arm once per seeded shuffled round. this table compares always-on sharding with the flat path to locate the crossover; there are six clean pairs unless noted. 500 entries was the first sampled size with a gain on both filesystems and libc builds, so the subsequent conditional candidate left pushes below 500 unchanged.

| files | filesystem | glibc time ratio [95%] | musl time ratio [95%] |
|---:|---|---|---|
| 50 | ext4 | 1.001 [0.961, 1.042] | 1.049 [1.013, 1.057] |
| 50 | xfs | 1.010 [0.963, 1.148] | 1.017 [0.957, 1.039] |
| 500 | ext4 | 0.852 [0.824, 0.930] | 0.872 [0.829, 0.924] |
| 500 | xfs | 0.831 [0.816, 0.839] | 0.836 [0.808, 0.845] |
| 5,000 | ext4 | 0.737 [0.730, 0.752] | 0.748 [0.743, 0.759] |
| 5,000 | xfs | 0.836 [0.802, 0.875] | 0.794 [0.764, 0.816] |
| 50,000 | ext4 | 0.730 [0.725, 0.751] | 0.726 [0.723, 0.734] |
| 50,000 | xfs | 0.781 [0.771, 0.806] | 0.789 [0.774, 0.806] |
| 100,000 | ext4 | 0.731 [0.715, 0.739] | 0.720 [0.706, 0.730] |
| 100,000 | xfs | 0.797 [0.792, 0.852]; 4 pairs | not measured before the rebase |

before further qualification, the experiment was rebased onto c3438f1c to include the new CPU work. the old pilot was retained, not pooled with the new builds. each row below is a separate same-binary musl comparison with 12 clean pairs and a read-once switch; each candidate shards only at 500 pending entries or more.

| rebased 100,000-file ext4 case | before s [range] | after s [range] | time ratio [95%] | peak RSS MiB, before to after | peak RSS ratio [95%] |
|---|---|---|---|---|---|
| conditional shards, full temporary paths | 6.548 [6.380, 7.578] | 4.702 [4.604, 4.885] | 0.717 [0.707, 0.731] | 164.65 to 181.11 | 1.090 [1.070, 1.123] |
| conditional shards, compact temporary names | 6.464 [5.803, 8.476] | 4.689 [4.572, 5.741] | 0.732 [0.719, 0.774] | 163.43 to 171.13 | 1.042 [1.024, 1.050] |

the first conditional candidate failed the large-push memory check. its path capacities alone explained less than 1 MiB of extra retained storage, not the observed RSS rise. one bounded alternative retained only a shard index and nonce for each unfinished file, rebuilding a full path when needed. its small-push path kept the existing full path and introduced no retained path allocation there. it reduced the memory cost, but did not remove it: peak RSS still rose by 4.2% [2.4%, 5.0%], about 7.7 MiB at the medians, with both sides already above the ordinary default memory budget. neither candidate is kept under the no-regression rule. the remaining conditional matrix and the compact candidate's glibc qualification were not run after that failure.

peak RSS came from GNU time's child `wait4`, with the original harness value retained separately. the interleaved harness ran under the shared lock on cores 12–23; bounded groups used at most a 270 s timeout plus five seconds to kill a stalled group, never five minutes of ownership. load-burst pairs stayed in the raw records, excluded from these estimates. every completed push's JSON, file count, independent payload bytes, readonly files and absence of unfinished files were checked. exact commands, ranges, build hashes, raw pairs and all three source patches remain with the filesystem sweep outside the repository.

two fixed-input tests failed first: the old path did not spread two pending files into distinct parents, and its sweep left a stale sharded part behind. deliberately disabling routing and shard traversal reproduced both failures. the restored mirror module passed 25 tests; the rebased CLI push module passed 20. the tests exercised read-back corruption and stop in both layouts, two interleaved pushes, unrelated and fresh leftovers, and a planted shard link. actual mixed flat/sharded CLI pushes of 5,000 files also produced one checked mirror. deliberately resolving a compact name through the wrong shard failed the live-file assertion. the prototypes and their added tests were removed; only this record is retained.

## pipelining cache verification, 2026-11-12

dropped. a bounded name walk and checked-open metadata saved one metadata lookup per object and much of the eager scan's memory, but slower small and medium caches failed the keep rule. no pipeline, switch or changed progress behavior remains in the product.

both arms ran `fetchloom --json --yes cache verify` in one release executable per libc, with `FETCHLOOM_EXP_CACHE_VERIFY_PIPE` read once. the baseline scanned names and metadata before hashing. the candidate discovered names while workers opened each object without following a link, reused the metadata checked on that handle, and hashed it. only hashing workers held object descriptors. the exact progress total was set once discovery and pending metadata checks finished; unreadable objects retained the old counts, errors and remaining progress share.

the first version shared the walk and results under one mutex. 44 comparisons covered ext4 and xfs caches of 0, 1, 50, 1,000, 10,000 and 100,000 distinct 32-byte objects, two 16 MiB objects, an ext4 cache of 10,000 distinct 4 KiB objects, and one-thread controls. each comparison has 12 clean interleaved pairs, both libc builds from source `ca82974c` over `066a6416`. a flagged pair was retained and excluded as a whole. the mandated harness ran at nice 0 on cores 12–23 under the shared lock, in turns below five minutes. the intervals below use 20,000 paired log-ratio bootstrap resamples. RSS is GNU time's child `wait4` peak; the harness's parent-inherited peak is retained separately.

| first version, 100,000 objects | old s | new s | ratio [95%] | median RSS MiB |
|---|---:|---:|---|---|
| ext4, glibc | 0.40381 | 0.34912 | 0.861 [0.843, 0.868] | 32.46 to 10.20 |
| ext4, musl | 0.45951 | 0.36464 | 0.796 [0.786, 0.816] | 40.55 to 18.52 |
| xfs, glibc | 0.38652 | 0.35614 | 0.932 [0.897, 0.939] | 41.79 to 10.06 |
| xfs, musl | 0.46715 | 0.36129 | 0.775 [0.767, 0.778] | 48.55 to 18.52 |

that gain did not generalize. at 1,000 objects the first version's ratios were 1.383/1.295 on ext4 and 1.421/1.271 on xfs, glibc/musl. a 50-object musl cache slowed about 8%. even the faster 100,000-object runs used more cpu: ext4 glibc cpu time was 1.586 [1.560, 1.620] of baseline. one-thread 100,000-object ratios of about 0.73–0.77 were not a reason to ship a default-path regression.

one bounded retry prefetched at most 64 names on the caller, kept worker-local results, and shared only name discovery and enumeration errors. atomics reported the exact total after both enumeration and outstanding metadata completed. empty caches needed no hashing workers. this was built anew on `88f914e3` over `03b26b75`; no rounds from different versions were pooled. seven previously regressing glibc controls each received 12 clean pairs, with none excluded:

| retry control | old s | new s | ratio [95%] |
|---|---:|---:|---|
| ext4-1000 | 0.01055 | 0.01416 | 1.344 [1.306, 1.376] |
| xfs-1000 | 0.01026 | 0.01364 | 1.323 [1.296, 1.370] |
| ext4-10000 | 0.04677 | 0.05463 | 1.167 [1.106, 1.201] |
| xfs-10000 | 0.04416 | 0.04461 | 1.000 [0.978, 1.032] |
| ext4-4k | 0.04862 | 0.04926 | 1.002 [0.986, 1.035] |
| ext4-50 | 0.00549 | 0.00542 | 0.985 [0.967, 0.998] |
| xfs-50 | 0.00539 | 0.00534 | 0.983 [0.971, 1.001] |

`ext4-4k` holds 10,000 4 KiB objects; the other suffixes are 32-byte object counts. the retry still slowed 1,000-object caches by 32–34%, and ext4 at 10,000 objects by about 17%. it stopped there: no retry musl build or remaining large-cache qualification was run after this failure. a syscall-only retry comparison at 1,000 objects counted `statx` 2,039 to 1,039, reads unchanged at 2,055, and futex calls 29 to 1,650. those counters expose coordination work; their instrumented elapsed times are not performance evidence.

functional CLI checks compared all errors, totals, changed marks and bytes for a damaged object, a link, a directory and an unreadable fan: eight comparisons in the first version and four glibc comparisons in the retry. the streaming regression failed against the eager scan. deliberately collecting the full name walk and disabling the one-shot total guard then failed both `verification_reads_before_finishing_the_name_walk` and `discovered_total_waits_for_metadata_and_is_reported_once`. after restoration, 91 upkeep, partial and link tests and all-target clippy passed.

reproduction records of this cache-verification experiment stay under `/datadisk/fl-perf/io/impl2/`: its immutable builds, both source patches and base identities, the a/b results of both versions, the corresponding analysis JSON, syscall counts, contract outputs and red/restored logs. the scripts that run, compare and analyze it select either version with `--experiment v1` or `v2`; the unchanged sweep harness's identity is recorded with each batch.

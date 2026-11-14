# Lock and names

## adding a list of links, 2026-11-12

`add -i` of a list of links, each to one file of 90,000 to 110,000 bytes on a server on this machine, each run into an empty project and an empty cache on tmpfs. the old code wrote data.toml's text again and read it back for each link, to add the link's dataset and name the next one; the change writes and reads it once for the whole list. release builds of c9da2d6 (before) and of the change (after) took turns, the order swapped each round, on the shared 24-core machine at a load average of 9 to 14.

| links | rounds | before, median and range | after, median and range | after/before |
|---:|---:|---|---|---:|
| 500 | 5 | 0.94 s, 0.92 to 1.10 | 0.24 s, 0.23 to 0.25 | 0.247 |
| 2,000 | 5 | 13.29 s, 13.23 to 14.89 | 1.37 s, 0.79 to 1.43 | 0.096 |
| 10,000 | 2 | 415.3 s, 410.3 to 420.3 | 4.64 s, 4.58 to 4.70 | 0.011 |

before's time was cpu, growing with the square of the list: 12.8 s of user time for 2,000 links, 414 s for 10,000. a trace of after's 10,000 links shows 3.1 s of downloads, 0.64 s relocking and 0.61 s placing, and at a terminal no gap of more than 0.85 s without a new line or bar. tmpfs leaves out the disk's flushes, which the change does not touch.

relocking then still looked up each new dataset in lists of all the others, four times over: in `resolve_all`, `rename::carry`, `draft` and `plan::from_cache_adding`. those became set and map lookups. the same setup with lists past 10,000 links, which repeat a link with `?c=1` and on, as the server ignores the query; release builds of b3b182a (before) and of the change, three rounds each:

| links | before, median and range | after, median and range | after/before |
|---:|---|---|---:|
| 10,000 | 4.76 s, 4.74 to 4.81 | 3.96 s, 3.90 to 4.07 | 0.823 |
| 40,000 | 31.71 s, 29.97 to 32.49 | 21.58 s, 21.56 to 21.95 | 0.680 |

a trace of after's 40,000 links shows relocking at 0.35 s, from 11.3 s before; placing them still takes 7.5 s, against 0.6 s for 10,000.

placing then still looked for each dataset's folder through every folder named before it, so its time grew with the square of the datasets; a profile of 40,000 links put a quarter of all cpu cycles in comparing folder names there. since 937fa9b, `add -i` leaves the datasets it adds out of those lines but for `-v`, so the search now runs when `sync` places many datasets, as in a fresh copy of such a project. it now finds each folder with one map lookup and still names the folders in the order they first come. release builds of 74b0d10 (before) and of the change (after), three rounds each, the order swapped each round, at a load average of 6 to 8. sync is `sync` into an empty folder holding only the project's data.toml and data.lock, with every file in the cache; add is the `add -i` above. the whole run's time and peak RSS come from `/usr/bin/time`, placing's from the `place` span of a traced run of its own:

| command | links | measure | before, median and range | after, median and range | after/before |
|---|---:|---|---|---|---:|
| sync | 10,000 | whole run | 0.91 s, 0.91 to 0.93 | 0.70 s, 0.67 to 0.70 | 0.769 |
| sync | 10,000 | placing | 0.40 s, 0.39 to 0.40 | 0.16 s, 0.16 to 0.17 | 0.400 |
| sync | 10,000 | peak RSS | 45.9 MiB, 45.5 to 46.1 | 46.2 MiB, 46.2 to 46.4 | 1.008 |
| sync | 40,000 | whole run | 9.68 s, 9.42 to 9.75 | 6.33 s, 6.18 to 6.61 | 0.654 |
| sync | 40,000 | placing | 4.04 s, 3.60 to 4.37 | 0.68 s, 0.66 to 0.70 | 0.168 |
| sync | 40,000 | peak RSS | 145.6 MiB, 145.4 to 146.3 | 145.9 MiB, 145.7 to 146.2 | 1.002 |
| add | 10,000 | whole run | 4.13 s, 3.65 to 4.68 | 4.35 s, 3.67 to 4.44 | 1.053 |
| add | 40,000 | whole run | 14.71 s, 14.69 to 15.14 | 15.02 s, 14.94 to 15.26 | 1.021 |

placing in sync now grows about in step with the datasets, and the time saved is cpu time: user time fell from 9.23 s to 5.84 s for 40,000. add's placing took the same both ways, 0.16 to 0.17 s for 10,000 links and 0.68 to 0.73 s for 40,000, since its datasets skip the search. its 40,000-link run came out 2% slower after, with after going second in two of the three rounds; six more rounds with the order balanced gave 15.34 s before and 15.05 s after. peak RSS rose 0.4 MiB for 10,000 datasets and stayed within its spread for 40,000. a sync of 2,000 datasets, three of them sharing a folder, printed the same lines byte for byte from both builds.

## commands that name a dataset, 2026-11-12

`path <name>` cost what the whole project costs. in a settled project of three imported datasets of 3, 1,000 and 10,000 files, `path` of the 3-file one took 47 ms on the glibc build, and a trace put 6.7 ms in reading data.lock, 7.5 ms in planning, 12.2 ms in reading the state and 14.9 ms in listing the data folder, each for every dataset. beside one dataset of 100,000 files, whose list data.lock keeps in data.lock.d, it took 0.47 s. the changes below make `path` and a settled `sync <name>` read only the pins of the named datasets, of what their recipes read and, for `path`, of datasets whose folders meet theirs, which a clash with their files could involve; only the state's records inside their folders; and only a listing of those folders. `list <name>` and `export <name>` read only the named pins.

both projects were git repositories on ext4 over nvme, settled before the runs. each change ran behind a switch in one release build, each arm adding one change to those before it, every arm once a round in a seeded shuffled order, 12 to 30 measured rounds a case; then the release builds of b3b182a and of the changes. a ratio is the median of new over old with its 95% paired bootstrap interval from `bench/ab.py`. a change stayed if an interval where it aimed ended below 0.97 and no case's interval for time or peak memory started above 1.02; batches that ran under a burst of other load ran again. the static musl build comes first, glibc after the semicolon.

| change | against the arm before it | outcome |
|---|---|---|
| list only the named dataset's folder, and what stands on the way down to it | `path` of 3 files among 11,000 0.654 [0.649, 0.664]; 0.672 [0.668, 0.684]. beside 100,000 files 0.648; 0.636 | kept |
| read from the state only the records inside that folder, passing over the others unbuilt | 0.629 [0.624, 0.636]; 0.740 [0.730, 0.749]. beside 100,000 files 0.570; 0.696, peak memory 0.811; 0.799 | kept |
| read only the pins path needs, and no other list in data.lock.d | 0.647 [0.644, 0.659]; 0.629 [0.622, 0.634]. beside 100,000 files 0.197; 0.240, peak memory 0.417; 0.435 | kept |
| pass over the lines of the other pins' files in data.lock unread | 0.626 [0.619, 0.637]; 0.713 [0.689, 0.722], peak memory 0.666; 1.000. flat where data.lock lists no other files itself | kept |
| answer a settled `sync <name>` from the named datasets alone | `sync` of 3 files among 11,000 0.567 [0.563, 0.572]; 0.595 [0.587, 0.602]. beside 100,000 files 0.370; 0.351 | kept |
| read the number of processors once a run, not the cgroup's cpu limit for each batch of work | 0.997 [0.985, 1.005]; 1.002 [0.988, 1.017], and no other case moved by 3%: a reading is some 30 system calls | dropped: no case moved past the 3% bar |

all of them, b3b182a against the changes, median times:

| case | musl | glibc |
|---|---|---|
| `path` of 3 files among 11,000 | 45.7 to 7.8 ms, 0.168 [0.166, 0.174], peak memory 0.382 | 48.8 to 11.3 ms, 0.231 [0.228, 0.236], peak memory 0.808 |
| `path` of 3 files beside 100,000 | 415 to 29 ms, 0.071 [0.070, 0.071], peak memory 0.305 | 479 to 50 ms, 0.107 [0.105, 0.109], peak memory 0.349 |
| `sync` of 3 files among 11,000 | 43.1 to 9.5 ms, 0.221 [0.215, 0.224], peak memory 0.392 | 45.8 to 12.9 ms, 0.283 [0.277, 0.287], peak memory 0.866 |
| `sync` of 3 files beside 100,000 | 392 to 30 ms, 0.077 [0.074, 0.078], peak memory 0.394 | 423 to 52 ms, 0.124 [0.118, 0.127], peak memory 0.415 |
| `list` and `export` of 3 files beside 100,000 | 125 to 3.5 ms and 62 to 3.4 ms | 162 to 3.7 ms and 77 to 3.8 ms |
| `path` and `sync` of the 10,000 files among 11,000 | 0.917 [0.900, 0.921] and 0.924 [0.915, 0.942] | 0.926 [0.909, 0.939] and 0.932 [0.918, 0.942] |
| `path` of the 100,000 files | 0.972 [0.959, 0.989] | 0.973 [0.952, 1.028] |
| `sync` and `status` of all, in either project, and `--version` | 0.967 to 0.999 | 0.990 to 1.004 |

no peak memory interval started above 1.001. beside 100,000 files, `path` of 3 still reads all of the state's 24 MB of json to pass over its records: 25 ms on musl and 45 ms on glibc, nearly all of what is left. going further needs a state that keeps each dataset's records apart.

## a json document in pieces, 2026-11-13

the scale map found `--json export` of 100,000 files holding far more memory than plain `export`. its document, 34.2 MB here, was encoded into one buffer that doubled as it grew, a copy at each doubling, and the musl build's allocator keeps freed memory for its [purge delay](process.md#a-shorter-purge-delay-for-lean-on-musl-2026-11-12), longer than an export runs: on musl `--json export` peaked at 178.2 MiB against 111.9 for plain `export`, on glibc 105.8 against 87.6. the document now goes into pieces of at most 1 MiB, the first 8 KiB, so nothing is copied or freed as it grows. it is still whole before its first byte goes out, since stdout cannot take back half a document when encoding fails.

| run | libc | pairs | time [95%] | peak memory [95%] |
|---|---|---:|---|---|
| `--json export` of 100,000 files | musl | 20 | 0.967 [0.961, 0.969] | 0.686 [0.685, 0.687], 178.2 to 122.2 MiB |
| | glibc | 20 | 0.974 [0.968, 0.978] | 0.924 [0.922, 0.926], 105.8 to 97.8 MiB |
| `--json list` of 100,000 files | musl | 15 | 0.988 [0.986, 0.990] | 0.905 [0.905, 0.908] |
| | glibc | 15 | 0.980 [0.974, 0.992] | 0.943 [0.938, 0.947] |
| `--json status` of 10,000 datasets | musl | 15 | 0.999 [0.989, 1.005] | 1.000 [1.000, 1.000] |
| | glibc | 15 | 1.001 [0.994, 1.010] | 1.001 [0.988, 1.004] |
| `--json status` of a project of 3 files | musl | 30 | 1.004 [0.990, 1.016] | 1.000 [1.000, 1.000] |
| | glibc | 30 | 1.003 [0.986, 1.016] | 1.004 [0.996, 1.013] |
| plain `export` of 100,000 files, a control | musl | 15 | 1.004 [0.998, 1.008] | 1.000 [1.000, 1.002] |

one experiment build per libc of df4d2a5b chose the encoding at run time, `FETCHLOOM_EXP_JSON_CHUNKS`; interleaved rounds after a warmup under the measure lock on cores 12-23, the projects on tmpfs. the 56 MiB musl saves is about what the freed copies come to: a buffer doubles only when it is full, so the 34.2 MB document left copies of 32, 16, 8 MiB and less behind, nearly 64 MiB in all. a first version, whose writer looped over the pieces on every write, cost time, 1.064 for the export and 1.084 for the list on musl; writing straight into the current piece, with a new piece only on a cold path, made it faster than the one buffer. the final build printed the same bytes as before and peaked at 122.7 MiB, as the experiment did.

`a_document_in_pieces_keeps_its_bytes_and_no_piece_outgrows_its_first_size` writes 3,000 runs of 1 to 4,000 bytes and one of 3 MiB, then checks the bytes and that no piece grew. a writer that ignored a piece's room failed it with a 16 MiB piece, and one that dropped the last piece failed it and `a_document_from_one_buffer_is_that_buffer`.

## a merge of many datasets, 2026-11-12

`lock` resolving a merge wrote data.toml's merged text one dataset at a time: each dataset taken from the other side read the merged text and the other side's text whole, copied the dataset in or took it out, and wrote the text again, so its time grew with the square of the datasets. it now reads each text once and makes every edit on one document, giving each new table the place reading the text again would give it. a layout it cannot follow alike, as dotted keys, a `[datasets]` header, datasets written inline, tables written out of the order their keys hold, or a copied dataset with tables of its own, takes the old way; a test compares the two over 3,000 generated layouts, and another counts that a merge of 200 datasets reads data.toml as often as one of 2.

the fixtures are the scale map's: data.toml and data.lock in git's conflict lines, each side adding n datasets of one file to an empty base, run as `--json --offline lock --dry-run`. one release build of 7dc24bb chose the old or new way by `FETCHLOOM_EXP_MERGE_BATCH`; abh.py ran the two in interleaved rounds, seeded shuffled order, with the measure lock held, on cores 12-23 of the shared 24-core machine, 30 rounds at 1 and 10 datasets and 10 at 1,000 and 2,000. a ratio is the median of new over old with its 95% paired bootstrap interval from `bench/ab.py`.

| datasets | glibc, old to new | new/old | musl, old to new | new/old |
|---:|---|---|---|---|
| 1 | 6.1 to 6.0 ms | 0.995 [0.979, 1.001] | 6.0 to 6.1 ms | 1.005 [0.996, 1.016] |
| 10 | 7.1 to 6.7 ms | 0.956 [0.938, 0.972] | 6.9 to 6.5 ms | 0.952 [0.944, 0.961] |
| 1,000 | 4.02 to 0.101 s | 0.0250 [0.0247, 0.0254] | 3.74 to 0.094 s | 0.0251 [0.0247, 0.0255] |
| 2,000 | 17.99 to 0.326 s | 0.0179 [0.0176, 0.0183] | 15.02 to 0.286 s | 0.0191 [0.0188, 0.0193] |

peak memory stayed the same at 1 and 10 datasets, fell to 0.947 and 0.925 at 1,000, and was 0.969 on glibc and 1.019 [1.014, 1.021] on musl at 2,000, 59.3 to 60.2 MiB. the merged text and everything `--json` printed matched byte for byte at every size. kept.

what is left still grows with the square of the datasets, at a much smaller cost: each new table looks for the dataset it goes before and shifts the places after it by walking every table once, which is most of the 0.33 s at 2,000, and choosing which side's layout to keep takes each side's datasets out one at a time. going further needs an index of names and places kept beside the document's tables.

## a drop across many datasets, 2026-11-12

`drop` of files in many datasets wrote data.toml again for each dataset it changed, reading and writing the whole text once for each exclusion, each copied members table and each members table it took out, and before that counted each dataset's planned files by going through every planned file of the project. both grew with the square of the datasets. it now collects the changes, makes them on one reading of data.toml, and counts the planned files once. a layout reading data.toml again would change the order of, as dotted keys mixed with tables, takes the old way; a test compares the two over 5,000 generated layouts and changes, errors included, another counts that a drop across 60 datasets reads data.toml as often as one across 3, and two more hold the text a drop across four datasets writes and the refusal of the first dataset in name order left with no file.

the fixtures are the scale map's: n datasets of two files each, `keep.bin` and `remove.bin`, run as `--json --offline drop --dry-run` of every `remove.bin`. one release build of 7dc24bb chose the old or new way by `FETCHLOOM_EXP_DROP_BATCH`, measured as in [a merge of many datasets](#a-merge-of-many-datasets-2026-11-12): 30 rounds at 1 and 10 datasets, 10 at 1,000 and 2,000.

| datasets | glibc, old to new | new/old | musl, old to new | new/old |
|---:|---|---|---|---|
| 1 | 3.8 to 3.8 ms | 0.996 [0.985, 1.020] | 3.2 to 3.2 ms | 1.006 [0.987, 1.035] |
| 10 | 4.1 to 3.9 ms | 0.936 [0.926, 0.951] | 3.7 to 3.6 ms | 0.962 [0.933, 0.973] |
| 1,000 | 2.30 to 0.036 s | 0.0157 [0.0155, 0.0159] | 2.06 to 0.031 s | 0.0149 [0.0147, 0.0150] |
| 2,000 | 8.83 to 0.070 s | 0.0080 [0.0077, 0.0083] | 8.31 to 0.056 s | 0.0067 [0.0067, 0.0068] |

peak memory stayed the same at 1 and 10 datasets and within 1% at 1,000 and 2,000, the most 1.008 on musl at 2,000, 0.4 MiB, for the changes held until data.toml is written. what `--json` printed matched byte for byte at every size. kept.

## listing only what is shown, 2026-11-12

`list` built every view of every dataset whatever it showed: each file's path in its folder, sorted, and a list of sizes only summed, for a table that needs only counts; each pinned file with its link, which only `--json` prints; and every dataset's row for `list <name>`, which then kept one, still formatting its files under `--json`, which prints none of them. it now builds the view asked for: counts for the table and `--json`, paths for `list <name>` at a terminal, pinned files for `--json`, and the named dataset alone. a test holds what each of the four views shows for six kinds of dataset, among them an archive this machine recorded with a members table and one it never unpacked.

the fixtures are the scale map's datasets-n, n datasets of one file, and files-n, one dataset of n files in data.lock.d, with their caches, run as `list` and `list <name>`, with and without `--json`. one release build of 7dc24bb chose the old or new way by `FETCHLOOM_EXP_LIST_VIEW`, measured as in [a merge of many datasets](#a-merge-of-many-datasets-2026-11-12): 30 rounds at 1 and 10, 15 at 40,000 datasets and 100,000 files.

| case | glibc, old to new | new/old, peak memory | musl, old to new | new/old, peak memory |
|---|---|---|---|---|
| `--json list`, 40,000 datasets | 501 to 486 ms | 0.976 [0.964, 0.990], 0.952 | 322 to 313 ms | 0.982 [0.933, 0.988], 0.975 |
| `--json list <name>`, 40,000 datasets | 320 to 301 ms | 0.939 [0.923, 0.961], 1.001 | 183 to 168 ms | 0.901 [0.889, 0.926], 1.000 |
| `list`, 40,000 datasets | 482 to 457 ms | 0.950 [0.932, 0.962], 0.913 | 285 to 276 ms | 0.971 [0.960, 0.989], 0.963 |
| `list <name>`, 40,000 datasets | 317 to 296 ms | 0.935 [0.920, 0.939], 1.001 | 182 to 168 ms | 0.924 [0.912, 0.931], 1.000 |
| `--json list`, 100,000 files | 196 to 189 ms | 0.953 [0.948, 0.962], 0.915 | 161 to 154 ms | 0.948 [0.933, 0.975], 0.931 |
| `--json list <name>`, 100,000 files | 238 to 192 ms | 0.815 [0.808, 0.824], 0.746 | 182 to 150 ms | 0.823 [0.819, 0.841], 0.931 |
| `list`, 100,000 files | 169 to 151 ms | 0.899 [0.884, 0.906], 0.923 | 126 to 115 ms | 0.917 [0.909, 0.921], 0.899 |
| `list <name>`, 100,000 files | 203 to 188 ms | 0.930 [0.907, 0.946], 0.851 | 153 to 149 ms | 0.973 [0.966, 0.989], 0.997 |

at 1 and 10, the 32 cases on both builds came out between 0.99 and 1.05 at the median of 30 rounds. three intervals started above 1: `list <name>` of one dataset on glibc, `--json list` of 10 datasets and `list <name>` of 10 files on musl. 100 more rounds of each gave 1.003 [0.997, 1.012], 1.006 [0.996, 1.014] and 1.017 [0.998, 1.035]. peak memory at 1 and 10 stayed the same, or fell to 0.984 for the table on musl. every view printed the same bytes as before at every size. kept: what is left of a list's time is mostly reading data.toml and data.lock.

## indexing pin membership and renames, 2026-11-12

kept. pin membership now searches the already name-sorted pin vectors. rename matching groups each source's spec and `extract`, or each recipe's input names, run words, code paths and variables, once. a signature must belong to exactly one old and one eligible new name. moved pins are moved rather than cloned; named runs still copy out-of-scope pins. no persisted state changes.

one release executable per libc, built from 27811da3 with `FETCHLOOM_EXP_PIN_INDEX` read once, compared the old and new paths. the flag and old paths were then removed. rustc 1.99.1, glibc 2.39 and the default musl allocator, ext4 nvme, offline JSON commands with warm data: `status` and settled `lock` at 1, 10, 1,000 and 40,000 datasets, unique rename forecasts at 1, 10, 600 and 1,000, and a 100,000-file ordinary status control. every row uses the same binary and input on both sides. output equality was checked separately.

`abh.py` shuffled each pair on cores 12–23 at nice 0 under the measurement lock, released between bounded groups. load-burst pairs were retained but excluded as whole pairs, then replaced. the initial tiny rename batches were repeated: a 0.5 s pre-batch load probe without another warmup had made their first arms slower. the reported tiny runs warm up after each acquisition. times are medians, with 95% percentile bootstrap intervals; ratios bootstrap paired log ratios through `bench/ab.py` with 20,000 resamples. these intervals do not establish exact equality or correct for checking several workloads.

RSS is a separate GNU-time child measurement on cores 0–11 at nice 10, without the wall lock: median [minimum, maximum] of per-process peaks, not the Python-parent floor in `abh.py`'s wait4 field. there are five native observations per arm through 1,000 entries and three above it. small-n RSS stays within its observed spread. the 1,000-rename musl stress case spends 7.1 MiB more, reaching 36.0 MiB; this is a reported memory cost, not a memory-neutral claim. the ordinary 100,000-file control remains below the 150 MB budget. the 40,000-dataset musl status and lock cases already exceeded that ordinary-workload target; their additional peaks are under 1 MiB. MB is decimal and MiB is binary.

a later independent native-RSS check used 32 pairs for each one- and ten-dataset status, lock and rename case. no interval established an RSS increase: every glibc interval included 1, and each musl interval either included 1 or favored lower RSS. the one-dataset musl rename used about 2 MiB less. those observations are retained separately; the table below still shows the original native sample, rather than silently replacing it.

an allocator diagnostic changed only `MIMALLOC_PURGE_DELAY` to 0: the 1,000-rename musl peaks became 20.4/20.1 MiB, versus 28.9/36.0 MiB at the shipped 1,000 ms delay. that supports a retention explanation, but neither an allocator change nor the diagnostic's lower memory is part of this keep decision. all tables below use the default allocator policy.

| libc | workload | n | before ms [95%] | after ms [95%] | paired ratio [95%] | peak RSS MiB, before to after |
|---|---|---:|---|---|---|---|
| glibc | status, datasets | 1 | 3.763 [3.721, 3.803] | 3.768 [3.737, 3.833] | 1.0032 [0.9929, 1.0113] | 10.36 [10.09, 10.56] to 10.37 [10.12, 10.76] |
| musl | status, datasets | 1 | 3.847 [3.814, 3.924] | 3.864 [3.792, 3.927] | 0.9944 [0.9832, 1.0181] | 14.95 [14.95, 14.95] to 14.95 [14.94, 14.95] |
| glibc | lock, datasets | 1 | 4.350 [4.271, 4.418] | 4.427 [4.345, 4.567] | 1.0064 [0.9963, 1.0244] | 11.70 [11.25, 11.80] to 11.76 [11.60, 11.79] |
| musl | lock, datasets | 1 | 3.908 [3.825, 4.014] | 3.991 [3.936, 4.073] | 1.0090 [0.9879, 1.0436] | 14.93 [14.93, 14.93] to 14.93 [14.80, 14.93] |
| glibc | status, datasets | 10 | 4.381 [4.245, 4.492] | 4.378 [4.268, 4.423] | 0.9925 [0.9740, 1.0237] | 10.51 [10.43, 10.67] to 10.64 [9.68, 10.76] |
| musl | status, datasets | 10 | 4.039 [4.010, 4.110] | 4.016 [3.965, 4.094] | 0.9891 [0.9792, 1.0023] | 14.95 [14.95, 14.95] to 14.94 [14.87, 14.95] |
| glibc | lock, datasets | 10 | 4.451 [4.385, 4.532] | 4.544 [4.442, 4.733] | 1.0171 [0.9971, 1.0345] | 12.00 [11.83, 12.07] to 11.82 [11.57, 11.99] |
| musl | lock, datasets | 10 | 3.684 [3.661, 3.748] | 3.690 [3.647, 3.816] | 0.9962 [0.9870, 1.0093] | 14.93 [14.80, 14.93] to 14.93 [14.80, 14.93] |
| glibc | status, datasets | 1,000 | 27.710 [27.489, 27.813] | 25.647 [25.160, 26.444] | 0.9269 [0.9159, 0.9487] | 13.30 [13.12, 13.34] to 13.37 [12.90, 13.73] |
| musl | status, datasets | 1,000 | 27.027 [26.215, 28.016] | 25.398 [24.375, 25.745] | 0.9246 [0.9153, 0.9478] | 25.38 [25.20, 25.39] to 25.38 [25.31, 25.39] |
| glibc | lock, datasets | 1,000 | 14.598 [14.281, 14.840] | 12.376 [12.090, 12.635] | 0.8422 [0.8328, 0.8608] | 14.10 [13.93, 14.41] to 14.22 [14.12, 14.47] |
| musl | lock, datasets | 1,000 | 13.995 [13.565, 14.193] | 11.816 [11.491, 12.207] | 0.8353 [0.8269, 0.8655] | 31.11 [31.11, 31.11] to 31.11 [30.93, 31.11] |
| glibc | status, datasets | 40,000 | 7593.403 [7290.038, 7822.390] | 1104.601 [1084.080, 1124.374] | 0.1455 [0.1404, 0.1505] | 116.12 [115.20, 116.25] to 116.16 [116.14, 116.18] |
| musl | status, datasets | 40,000 | 3457.476 [3449.533, 3466.505] | 875.585 [864.818, 883.389] | 0.2536 [0.2485, 0.2554] | 154.71 [154.53, 154.77] to 155.40 [155.22, 155.41] |
| glibc | lock, datasets | 40,000 | 7408.789 [7305.159, 7529.643] | 439.115 [436.297, 448.144] | 0.0599 [0.0588, 0.0608] | 116.55 [116.30, 116.68] to 116.44 [115.86, 116.45] |
| musl | lock, datasets | 40,000 | 2863.722 [2856.372, 2866.920] | 285.986 [284.816, 287.958] | 0.0999 [0.0994, 0.1006] | 186.08 [186.08, 186.25] to 186.95 [186.95, 187.13] |
| glibc | unique renames | 1 | 4.602 [4.550, 4.680] | 4.630 [4.557, 4.775] | 1.0027 [0.9918, 1.0311] | 10.96 [10.66, 11.39] to 11.14 [11.12, 11.43] |
| musl | unique renames | 1 | 4.434 [4.392, 4.503] | 4.285 [4.212, 4.338] | 0.9685 [0.9584, 0.9787] | 17.60 [17.48, 17.61] to 15.57 [15.57, 15.57] |
| glibc | unique renames | 10 | 4.881 [4.802, 5.010] | 4.616 [4.548, 4.701] | 0.9496 [0.9222, 0.9684] | 11.08 [10.57, 11.51] to 11.10 [10.87, 11.23] |
| musl | unique renames | 10 | 4.413 [4.382, 4.458] | 4.324 [4.284, 4.347] | 0.9773 [0.9633, 0.9898] | 17.61 [17.35, 17.61] to 17.60 [17.48, 17.61] |
| glibc | unique renames | 600 | 1335.849 [1280.093, 1342.686] | 15.836 [15.450, 15.935] | 0.0119 [0.0116, 0.0123] | 12.97 [12.92, 13.04] to 12.85 [12.64, 12.92] |
| musl | unique renames | 600 | 1649.760 [1646.830, 1651.957] | 14.560 [14.471, 14.932] | 0.0088 [0.0088, 0.0091] | 24.88 [24.88, 24.89] to 26.02 [26.01, 26.02] |
| glibc | unique renames | 1,000 | 5472.140 [5400.593, 5495.236] | 23.760 [22.958, 24.773] | 0.0044 [0.0042, 0.0045] | 14.20 [13.71, 14.52] to 14.38 [14.11, 14.55] |
| musl | unique renames | 1,000 | 6838.609 [6829.122, 6847.039] | 21.511 [21.101, 21.947] | 0.0031 [0.0031, 0.0032] | 28.89 [28.88, 28.89] to 36.01 [35.82, 36.02] |
| glibc | status, files control | 100,000 | 447.400 [429.189, 463.357] | 429.063 [423.937, 448.631] | 0.9597 [0.9312, 1.0064] | 78.49 [78.38, 78.58] to 78.38 [78.00, 78.63] |
| musl | status, files control | 100,000 | 385.965 [369.276, 392.810] | 381.148 [370.645, 390.603] | 1.0021 [0.9912, 1.0105] | 97.39 [85.35, 99.14] to 95.44 [85.21, 100.99] |

| libc | workload | n | instructions M, before to after | clean pairs | excluded burst pairs |
|---|---|---:|---:|---:|---:|
| glibc | status, datasets | 1 | 3.173 to 3.173 | 64 | 0 |
| musl | status, datasets | 1 | 2.526 to 2.527 | 64 | 0 |
| glibc | lock, datasets | 1 | 3.152 to 3.155 | 64 | 0 |
| musl | lock, datasets | 1 | 2.483 to 2.486 | 64 | 0 |
| glibc | status, datasets | 10 | 3.997 to 3.999 | 64 | 0 |
| musl | status, datasets | 10 | 3.244 to 3.239 | 64 | 0 |
| glibc | lock, datasets | 10 | 3.579 to 3.579 | 60 | 4 |
| musl | lock, datasets | 10 | 2.875 to 2.875 | 64 | 0 |
| glibc | status, datasets | 1,000 | 115.099 to 98.731 | 10 | 0 |
| musl | status, datasets | 1,000 | 110.030 to 88.277 | 12 | 0 |
| glibc | lock, datasets | 1,000 | 68.819 to 52.409 | 10 | 0 |
| musl | lock, datasets | 1,000 | 71.600 to 49.852 | 12 | 0 |
| glibc | status, datasets | 40,000 | 30972.136 to 4036.744 | 10 | 2 |
| musl | status, datasets | 40,000 | 29174.134 to 3655.827 | 10 | 0 |
| glibc | lock, datasets | 40,000 | 29038.889 to 2126.822 | 10 | 0 |
| musl | lock, datasets | 40,000 | 27560.992 to 2042.746 | 11 | 17 |
| glibc | unique renames | 1 | 4.239 to 4.236 | 61 | 3 |
| musl | unique renames | 1 | 3.195 to 3.187 | 64 | 0 |
| glibc | unique renames | 10 | 6.141 to 5.362 | 64 | 0 |
| musl | unique renames | 10 | 4.886 to 4.191 | 64 | 0 |
| glibc | unique renames | 600 | 10758.294 to 84.639 | 10 | 5 |
| musl | unique renames | 600 | 15987.157 to 77.303 | 12 | 0 |
| glibc | unique renames | 1,000 | 43479.296 to 138.707 | 10 | 2 |
| musl | unique renames | 1,000 | 68013.643 to 127.642 | 12 | 0 |
| glibc | status, files control | 100,000 | 2000.476 to 2000.509 | 10 | 5 |
| musl | status, files control | 100,000 | 2474.830 to 2475.101 | 12 | 0 |

characterizations passed before the rewrite. the new cases are `fetched_renames_keep_bytes_and_report_new_name_order`, `named_rename_copies_the_pin_and_excludes_other_new_names`, `fetched_matches_require_unique_specs_and_the_same_extraction`, `given_and_upgraded_names_are_not_rename_candidates`, `recipe_renames_match_the_ordered_recipe_not_its_old_hashes` and `recipe_renames_refuse_ambiguous_old_or_new_shapes`. disabling the duplicate-new guard made the ambiguity cases fail; restoring it passed. the existing real-CLI fetched and recipe rename cases also passed. fixtures, experiment binaries, raw rounds, native RSS and mutation evidence remain with the sweep outside the repository.

## indexing export selection, 2026-11-12

dropped under the default-policy memory gate. one sorted, deduplicated vector of borrowed names removed repeated linear membership checks from named export's lock reader, inventory and left-out reporting. validation still preceded sorting, preserving the first unknown-name error; output order, duplicate arguments and stale-pin omission stayed unchanged.

the build, paired analysis, noise filter and independent native-RSS method are those of [the rename comparison](#indexing-pin-membership-and-renames-2026-11-12). `FETCHLOOM_EXP_EXPORT_INDEX` selected the two paths in one executable per libc. `export` named every dataset at 1, 10, 1,000 and 40,000; an unnamed export of one 100,000-file dataset was the control. all commands used offline JSON output. RSS below is median [minimum, maximum] of per-process peaks in MiB, not the harness's parent floor.

wall time improved substantially at 40,000 names and the small cases showed no established slowdown, but native musl RSS rose from 137.6–144.4 MiB to 184.2–184.4 MiB. that is much more than the modest memory allowance, so the change did not stay. the unnamed 100,000-file export already misses the 150 MB target and is unchanged; it does not excuse a new increase elsewhere. no production code or allocator setting was retained for this proposal.

| libc | workload | n | before ms [95%] | after ms [95%] | paired ratio [95%] | peak RSS MiB, before to after |
|---|---|---:|---|---|---|---|
| glibc | all named exports | 1 | 3.830 [3.753, 3.925] | 3.762 [3.685, 3.931] | 0.9916 [0.9728, 1.0106] | 10.99 [10.85, 11.14] to 10.78 [10.18, 10.98] |
| musl | all named exports | 1 | 3.596 [3.567, 3.619] | 3.579 [3.560, 3.621] | 0.9935 [0.9859, 1.0026] | 15.56 [15.56, 15.57] to 15.56 [15.55, 15.57] |
| glibc | all named exports | 10 | 3.964 [3.825, 4.026] | 3.802 [3.776, 3.950] | 0.9824 [0.9586, 1.0136] | 10.68 [10.22, 10.99] to 10.73 [10.35, 11.14] |
| musl | all named exports | 10 | 4.129 [4.049, 4.251] | 4.162 [4.022, 4.229] | 0.9935 [0.9858, 1.0203] | 15.57 [15.56, 15.57] to 15.56 [15.56, 15.61] |
| glibc | all named exports | 1,000 | 27.275 [26.833, 27.860] | 16.389 [16.146, 17.001] | 0.6045 [0.5944, 0.6177] | 14.16 [13.61, 14.23] to 14.14 [13.43, 14.16] |
| musl | all named exports | 1,000 | 26.128 [25.391, 26.518] | 14.756 [14.343, 15.112] | 0.5659 [0.5560, 0.5823] | 35.82 [35.77, 35.82] to 35.82 [35.82, 35.83] |
| glibc | all named exports | 40,000 | 17657.924 [17186.672, 17752.020] | 636.157 [625.705, 643.062] | 0.0367 [0.0356, 0.0374] | 144.78 [144.77, 144.92] to 146.40 [145.85, 146.91] |
| musl | all named exports | 40,000 | 14356.194 [14332.797, 14361.045] | 394.845 [394.072, 397.699] | 0.0275 [0.0275, 0.0277] | 143.02 [137.62, 144.43] to 184.27 [184.15, 184.35] |
| glibc | unnamed export, files control | 100,000 | 334.756 [327.599, 337.032] | 334.278 [331.241, 335.977] | 1.0023 [0.9901, 1.0134] | 105.09 [104.79, 105.33] to 104.91 [104.89, 105.16] |
| musl | unnamed export, files control | 100,000 | 264.906 [263.350, 268.155] | 265.317 [263.419, 266.273] | 1.0006 [0.9926, 1.0022] | 177.88 [177.88, 177.89] to 177.88 [177.88, 177.88] |

| libc | workload | n | instructions M, before to after | clean pairs | excluded burst pairs |
|---|---|---:|---:|---:|---:|
| glibc | all named exports | 1 | 3.126 to 3.127 | 32 | 34 |
| musl | all named exports | 1 | 2.493 to 2.490 | 64 | 0 |
| glibc | all named exports | 10 | 3.755 to 3.757 | 32 | 54 |
| musl | all named exports | 10 | 3.047 to 3.051 | 64 | 0 |
| glibc | all named exports | 1,000 | 158.972 to 76.988 | 10 | 0 |
| musl | all named exports | 1,000 | 176.873 to 68.485 | 12 | 0 |
| glibc | all named exports | 40,000 | 137582.837 to 3183.745 | 10 | 0 |
| musl | all named exports | 40,000 | 130462.786 to 2889.036 | 12 | 0 |
| glibc | unnamed export, files control | 100,000 | 2522.494 to 2523.609 | 10 | 12 |
| musl | unnamed export, files control | 100,000 | 2314.885 to 2314.957 | 12 | 0 |

the borrowed vector itself occupies about 0.61 MiB at 40,000 names. the shipped mimalloc 3.3.2 reports a 1,000 ms purge delay. a separate causal probe changed only that option, once per arm, and obtained:

| purge policy | old peak MiB | indexed peak MiB |
|---|---:|---:|
| default, 1,000 ms | 144.5 | 184.2 |
| 0 ms, diagnostic only | 133.5 | 134.2 |

this supports delayed reclamation as the main source of the RSS gap. the override is not part of the gate: the default peak is real, and a global allocator change would need its own all-command checks. no inferred retained pages were subtracted from the cost.

the CLI characterization `selection_keeps_error_order_deduplicates_and_omits_stale_pins` passed on the old and experimental paths. deliberately sorting before validation made its error-order assertion fail. the test and prototype remain in the saved experiment patch, not as unused product code or a new test for unchanged behavior. all five workload pairs produced identical JSON on both libc builds. raw rounds, binaries, counters and the separate purge probe remain with the sweep outside the repository.

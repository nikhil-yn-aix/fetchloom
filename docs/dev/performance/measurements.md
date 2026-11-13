# measurements

what the linux lab measured for this release, the a/b passes behind its performance decisions, and the historical observations before them.

## ledger

every change measured with the current harness, one row each: what it gained where it aimed, what it cost, and whether it stayed. a ratio is new over old, so under 1 is faster or smaller; the section a row links to holds its fixture, rounds, intervals and keep rule. changes measured before 2026-11-09, without retained run records, are in [observations](#observations-and-rejected-experiments).

| area | change | result | outcome | details |
|---|---|---|---|---|
| network | request slots released during retry and quota sleeps | mixed 2/2 case: glibc 0.619, musl 0.622; healthy/default controls within noise | kept | [sleeping requests](#network-slots-while-a-request-waits-2026-11-12) |
| network | one recovery probe after an unknown-quota 429 | 429 counts stay at 13–14 per run, below the required 20% reduction | dropped, no request-count win | [quota probe](#probing-after-an-unknown-quota-2026-11-12) |
| network | successful metadata reused within one run | 100 selections at 50 ms: glibc 0.166, musl 0.163; 100 requests to 5; +0.18 MiB on a one-dataset run, musl | kept | [metadata memo](#a-per-run-metadata-memo-2026-11-12) |
| network | max's requests at once doubled from the default's while each doubling brought data in 25% faster | 100 links from a server that waits 50 ms: 1.709 [1.223, 2.386] against max's fixed 32 and 16; 1,000 links from a local server: peak memory 0.847 | dropped, far case slower | [max's requests at once](#maxs-requests-at-once-on-near-and-far-servers-2026-11-12) |
| network | max's requests at once narrowed to the default's once the first round showed each connection fast | 50 ms server flat (glibc 0.997, musl 1.000); local server peak memory glibc 0.897 [0.879, 0.913], musl 0.951 [0.924, 0.975] | dropped, memory below the 10% bar | [max's requests at once](#maxs-requests-at-once-on-near-and-far-servers-2026-11-12) |
| network | several connections per host for file bodies | far s2 0.288, poor s2 0.424; lan and home flat | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | hosts interleaved in the download queue | two-host fixture 0.764 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | download jobs built at admission | peak memory 228 to 205 MB at 100,000 pins | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | download permits released before publication | ext4 s3 0.925 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | no closing resume record for small downloads | xfs s3 0.757, ext4 s3 0.840 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | unfinished downloads spread over 16 folders | xfs s5 0.712, ext4 s3 0.902 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | a large download written to the disk while it arrives | `get -o` of 10 GiB 0.531; small downloads flat | kept | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| network | an unchanged bucket object reused by its listed ETag and size | `lock --upgrade` of a prefix: 7 to 2 requests, 12.2 to 2.0 MB | kept | [bucket prefix](#upgrading-a-bucket-prefix-2026-11-12) |
| network | fast large files split into ranges from a link-rate probe | live hugging face 5.6 to 3.7 s, but lan 1 GiB 1.075 and 1.5 to 1.6 times the cpu | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | shorter download queues | xfs s1 1.108 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| network | a split download's later streams written in batches, and its read-back buffer kept between ranges | 96 MiB split from a server holding each connection to 0.8 MB/s: 36.8 s for 0.27 s of cpu, 0.7%, hashing every byte and reading three quarters back | dropped: the network sets a split download's pace | [small leftovers](#small-leftovers-from-the-perf-maps-2026-11-13) |
| network | lean against default: 2 requests at once, 2 to one host | 100 links from a server that waits 50 ms: time 2.006, peak memory 0.998; `add -i` of 1,000 local links 1.081 and 0.887 glibc, 1.085 and 0.679 musl; 1 GiB `get` 0.977 and 0.862 glibc, 0.997 and 0.991 musl | lean's trade, no change | [levels](#resources-levels-against-default-2026-11-13) |
| network | max against default: 32 requests at once, 16 to one host | 100 links at 50 ms: time 0.281, peak memory 1.077; `add -i` of 1,000 local links 1.046 and 1.274 glibc, 1.040 and 1.141 musl; 1 GiB `get` 1.005 and 1.069 glibc, 0.999 and 1.074 musl | max's trade, no change | [levels](#resources-levels-against-default-2026-11-13) |
| network | a cold sync asks the cache about each pinned file once, on the workers, where it asked four times in turn | 10,000 small files 0.877 [0.868, 0.905], 100,000 0.876 [0.841, 0.883], beside a/a arms at 1.005 and 0.990; on ext4 0.947 [0.934, 0.971]; `add`, no-op and zip flat; peak memory flat | kept | [pinnacle pass](#a-cold-syncs-questions-to-the-cache) |
| network | a download's request slot freed once its body is read, and its partial record read once | cold sync of 10,000 small files 0.892 [0.878, 0.901], over the ask-once change 0.872 [0.819, 0.913]; `add` 0.919; ext4 1.002; 512 MiB `get` flat | dropped, below the bar | [pinnacle pass](#a-downloads-slot) |
| network | no URL record for a link `add` learns from a listing, or the record written in place | `add` of 100,000 small files on ext4 writes 1,083 MB, 409.6 MB of it these records, a 4 KiB block each; without them 0.622 of the bytes and time 0.849, but `lock --upgrade` and another project's `add` then download every unchanged object again; written in place, the same bytes | not changed: the records keep the ETag data.lock lacks | [pinnacle pass](#what-add-writes) |
| network | URL records as lines appended to 16 shard files, each with an index of 12-byte entries a run reads once per shard and groups by key, where each record was a file of its own | `add` of 100,000 links on ext4 writes 0.749 of the bytes, 1,083 to 811 MB, in 0.845 [0.790, 0.861] of the time beside an a/a arm at 1.034; 10,000 links 0.746 and 0.810; beside a store of a million records, `add` of 10,000 links 0.839, of 100,000 0.826, `lock --upgrade` of them 0.739 and 0.729, a `get` of one link 0.747; 1.59 million records take 32 files and 0.60 GB, where they took 1.59 million files and 6.70 GB; gc over a million 3.5 to 5 times faster; peak memory up to 1.095, 9 to 10 bytes a record in each shard a run reads | kept | [pinnacle pass](#url-records-in-shards) |
| network | the same store with 256 shards, each run checking and indexing every line of each shard it reads | cold `add` 0.811 and 0.837, 0.92 of the bytes; beside a million records `lock --upgrade` of 10,000 links 4.302 [4.227, 4.411], peak memory 1.59 | dropped, slower beside a large store | [pinnacle pass](#url-records-in-shards) |
| network | 16 shards whose index of 16-byte entries a run sorts on its first read | cold `add` 0.819 and 0.825, 0.75 of the bytes; beside a million records `lock --upgrade` of 10,000 links 1.095 [1.088, 1.101] beside an a/a arm at 1.006, peak memory 1.14 | dropped for the grouped index | [pinnacle pass](#url-records-in-shards) |
| search | a 3 s grace for the sites still searching once one has answered a search of every site, 10 s when `--source` names them | 12 pairs of new search words: wall 0.307 [0.306, 0.420], 10.08 to 3.08 s at the median; peak memory 1.002 [0.972, 1.033]; dandi left out in 7 of 12, openneuro in 1, ENA in 11 against 8 | kept, a site later than 3 s left out with its warning | [search grace](#a-shorter-search-grace-2026-11-13) |
| hashing | a hashing read's buffer sized to the file | importing 100,000 files 0.812; `cache verify` of 100,000 objects 0.784 | kept | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| hashing | hashing read buffers started at a page | `cache verify` of 8 GiB on glibc 0.697; flat on musl | kept | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| hashing | SHA-256 and a required MD5 on separate workers | 512 MiB 0.776 glibc, 0.767 musl; musl median RSS 106.3 to 120.3 MiB, maximum 168.6 to 154.3 MiB | kept | [paired checksums](#overlapping-required-checksums-2026-11-12) |
| hashing | placed-file verification scheduled by bytes as well as file count | 8 files of 64 MiB warm 0.163 glibc, 0.151 musl; cold 0.263 and 0.262; median RSS +6.1 and +10.3 MiB, lean unchanged | kept | [verification](#byte-aware-placed-file-verification-2026-11-12) |
| hashing | lean against default: 2 local workers | `status --verify` of 8 files of 64 MiB: time 6.608, peak memory 0.673; `cache verify` of 100,000 objects 1.961 and 0.985, glibc | lean's trade, no change | [levels](#resources-levels-against-default-2026-11-13) |
| hashing | max against default: default's workers and memory | `status --verify` of 8 files of 64 MiB 0.958 and 0.998; `cache verify` of 100,000 objects 1.010 and 1.006, glibc | max's trade, no change | [levels](#resources-levels-against-default-2026-11-13) |
| flushing | runs of up to 64 files flushed one by one, larger batches with one `syncfs` | a sync of 4 small files on a busy disk 684 to 185 ms; the lab flat | kept | [busy disk](#flushing-a-few-files-on-a-busy-disk-2026-11-09) |
| flushing | a waiting group of finished downloads named after at most 1 s | a second project on a shared cache 26.4 to 12.9 s | kept | [shared cache](#a-second-run-on-a-shared-cache-2026-11-12) |
| flushing | one flush per file instead of one `syncfs` a batch | ext4 s3 1.567 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| flushing | flush groups of finished downloads sized to a quarter of the open-file limit, at most 256 | `add -i` of 1,000 links under a hard limit of 256 open files: exit 50 to 0, at most 117 files open; at the default limit groups of 256 as before, 1.000 glibc, 0.987 musl | kept, for correctness | [low open-file limit](#flush-groups-under-a-low-open-file-limit-2026-11-12) |
| flushing | the first 64 files of every batch flushed on their own | ext4 s1 1.078, peak memory 1.151 | dropped | [busy disk](#flushing-a-few-files-on-a-busy-disk-2026-11-09) |
| flushing | a run of up to 64 files flushed with one `syncfs` while the system holds under 16 MiB of unwritten data | always one `syncfs` on an idle slow device: import of 32 files 0.331, zip of 32 0.820, 32 links 0.947; on the busy os disk 0.647, 1.168 and 2.097; the 16 MiB rule never found this machine quiet, 0.977 to 1.047 | dropped: no signal says which filesystem holds the unwritten data | [slow device](#flush-groups-on-a-slow-device-2026-11-13) |
| archives | budget an explicit thread cap only for pending readers | xfs time 0.689 glibc / 0.703 musl; small musl +2.03 MiB, within the default budget | kept, reported memory cost | [reader budgets](#pending-archive-reader-budgets-2026-11-12) |
| archives | archive staging spread over 16 folders | ext4 s4 0.798, xfs s4 0.830 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| archives | no-replace rename for new objects | ext4 s4 1.090 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| archives | xfs staging through `O_TMPFILE` | xfs s4 0.955 [0.914, 1.026] | dropped, no win | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| archives | a new object published on btrfs by one rename | 0.970 | dropped, no win | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| archives | short memory moves in the musl bzip2 decoder | 64 MiB 2.843 s musl, 2.211 s glibc; 31.47% of musl user samples in `memmove` | upstream note, no change | [bzip2](#musl-bzip2-short-memory-moves-2026-11-12) |
| archives | stream every member above 1 MiB to an existing writer | large gzip 0.717 glibc, 0.774 musl; 1.0625 MiB-member control 1.021 [1.005, 1.044] on glibc | dropped | [archive handoffs](#overlapping-archive-decoding-and-storage-2026-11-12) |
| archives | stream declared large members with spare writer budget | large gzip 0.754 glibc, 0.759 musl; 4.0625 MiB-member control 0.856 and 0.885; highest musl RSS 88.6 MiB | kept | [archive handoffs](#overlapping-archive-decoding-and-storage-2026-11-12) |
| archives | lean against default: 2 local workers | sync of a zip of 10,000 files: time 2.095 glibc, 2.031 musl; peak memory 0.934 glibc, 0.870 musl | lean's trade, no change | [levels](#resources-levels-against-default-2026-11-13) |
| archives | max against default: default's workers and memory | the same zip: time 1.018 glibc, 0.992 musl; peak memory 0.934 glibc, 1.000 musl | max's trade, no change | [levels](#resources-levels-against-default-2026-11-13) |
| archives | a sync's drop of unpacked archives looks only at the archives it may drop, each member once across projects, on the workers, and reads only the kept rebuilds of each project's state | sync of a zip of 10,000 files that 20 projects share 2.09 to 0.18 s, 0.086 [0.085, 0.086]; one project's zip 0.802, tar.gz 0.822; the cache report beside 100,000 files 0.834; the zip's peak memory 4 to 9% more | kept | [pinnacle pass](#dropping-unpacked-archives) |
| placement | placement workers limited by parent folder, one on exfat | wall flat; cpu 0.79 to 0.81, on exfat 85% lower | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| placement | a hardlink's details taken from its object | xfs s4 0.933 | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| placement | placement work per file cut | 100,000 files 0.840 | kept | [placement](#ab-passes-on-placement-and-startup-2026-11) |
| placement | each dataset's folder found with one lookup | sync of 40,000 datasets 9.68 to 6.33 s | kept | [list of links](#adding-a-list-of-links-2026-11-12) |
| placement | an import's list of walked files freed before its files are placed | import of 100,000 files peak memory 0.890 on glibc, 0.908 on musl; wall flat | kept | [import](#an-import-of-100000-files-2026-11-12) |
| placement | small copies instead of reflinks on xfs | xfs s4 1.245 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| placement | read-only placements on xfs hardlinked before a reflink is tried | musl: sync of a zip of 10,000 files into a new cache 0.776 [0.751, 0.782], placing 10,000 cached files 0.358 [0.333, 0.469]; memory flat | not adopted: the gain is the isolation reflinks give, a placement decision | [xfs hardlinks](#hardlinks-before-reflinks-on-xfs-2026-11-13) |
| placement | parent folder handles kept between batches | s5 1.268 and 1.297 | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| placement | stop the copy loop after a short completed block | ext4 primitive 0.941; tmpfs noisy; no whole-command win established | dropped, below the bar | [short copies](#short-copy-eof-dispatch-2026-11-12) |
| placement | lean against default: 2 local workers | settled sync of 100,000 files: time 1.112, peak memory 0.999, glibc; sync of 40,000 one-file datasets 1.115 and 0.998 glibc, 1.144 and 0.901 musl | lean's trade, no change | [levels](#resources-levels-against-default-2026-11-13) |
| placement | max against default: default's workers and memory | settled sync of 100,000 files 1.000 and 1.000, glibc; sync of 40,000 datasets 0.999 and 1.001 glibc, 1.000 and 0.990 musl | max's trade, no change | [levels](#resources-levels-against-default-2026-11-13) |
| project state | indexed selected-folder ranges | 40,000 named datasets: ratios 0.1312 glibc and 0.0687 musl; musl stress peak +0.75 MiB | kept | [folder ranges](#searching-selected-folder-ranges-2026-11-12) |
| project state | the data folder walk looks at the run's stop before each folder | a stop asked before the walk ends it at once; status of 100,000 files on ext4 0.996 glibc, 1.003 musl, peak memory +0.5 and +0.6 MiB | kept, for stopping | [stop during the walk](#a-stop-during-the-data-folder-walk-2026-11-12) |
| project state | the state indexed by blocks of records, so a command that names datasets reads only theirs | `path` of 1 file beside 100,000 records 0.090 glibc, 0.144 musl; beside 1,000,000 0.013 and 0.021; peak memory 0.04 to 0.40; whole reads flat, a sync that writes the state +0.7% on musl | kept | [state index](#an-indexed-project-state-2026-11-12) |
| project state | reuse an intact racy hash within one survey | `status --verify` of 32 MiB: musl 0.544, glibc 0.563; controls and memory within noise | kept | [racy hashes](#reusing-racy-hashes-within-a-survey-2026-11-12) |
| project state | project scan in bounded parallel batches | status at 100,000 files 0.600 to 0.442 s | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| project state | one pool of scan workers for the whole walk | settled sync of 100,000 files 0.902, but of 10,000 1.052 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| project state | each scanned file's path made at its final length rather than grown by `format!` | `--json status` of 100,000 files 0.986 [0.970, 0.998], settled sync 0.991 [0.979, 1.004]; peak memory flat | dropped, below the bar | [small leftovers](#small-leftovers-from-the-perf-maps-2026-11-13) |
| project state | a digest written from a buffer on the stack, not a new string each time | `--json export` of 100,000 files 0.960 [0.955, 0.969] musl, 0.960 glibc; the sync writing a 100,000-record state 1.002 on both | dropped, below the bar | [small leftovers](#small-leftovers-from-the-perf-maps-2026-11-13) |
| project state | the state read and written by sonic-rs instead of serde_json | settled sync of 100,000 files 0.947 [0.930, 0.968] musl, 0.954 glibc; status 0.965 and 0.981; the sync writing the state 0.956 and 0.974; 16 more packages in Cargo.lock, binary 0.3 MB larger | dropped, below the bar | [small leftovers](#small-leftovers-from-the-perf-maps-2026-11-13) |
| project state | the state's map built in one pass | settled sync of 100,000 files 0.784, but peak memory 1.148 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| project state | the same map built in merged chunks | 0.85, but on musl peak memory 1.05 to 1.06 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| project state | project state written as a stream | no measured win | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| lock and names | a whole dataset a command hands over, and a draft, found through sets while drafting | `add -i` of 40,000 small links 1.013 [0.985, 1.017] | dropped, no win | [scans](#scans-inside-per-file-loops-2026-11-12) |
| project state | an unplaced dataset's files told through a set of the unplaced | status with 40,000 datasets set to `place = false`: 4.42 to 1.38 s, 0.315 | kept | [scans](#scans-inside-per-file-loops-2026-11-12) |
| project state | each blocker listed once through a set, not a search of those listed | status with 40,000 folders where files belong: 5.04 to 1.54 s, with the scans below already gone | kept | [scans](#scans-inside-per-file-loops-2026-11-12) |
| project state | a whole read of the state goes through its index a block at a time into one list, which builds the map at once, not the whole document held and parsed into a map one record at a time | settled sync of 100,000 files 0.839 [0.817, 0.849], peak memory 0.834; status 0.838, peak 0.904; `status --verify` 0.882; 10,000 files 0.899 and 0.894, a no-op's peak 2 MiB more | kept | [pinnacle pass](#reading-the-whole-state) |
| project state | a state of one block, at most 64 KiB, read in one read again, not through its index | the block-wise read had cost a settled sync of 3 files 8 system calls, 1.009 on both libcs: 3 seeks and 2 reads of its 966-byte state, plus a futex, a signal mask and an munmap that vary between runs of either build. one read now, 3 calls fewer than before the pass: 300 interleaved pairs against 5e18256c, musl 0.996 [0.988, 1.003] beside 6ace5b2d at 0.993 and an a/a arm at 1.002; glibc 0.976 [0.972, 0.980], 6ace5b2d 0.996, a/a 1.002; status and settled sync of 100,000 files flat | kept | [a small state](#reading-a-small-state) |
| project state | the whole state read on a thread of its own beside the data folder walk, and status's data.lock beside both | over the block-wise read below: settled sync of 100,000 files 0.671 against 0.839, status 0.669, 0.501 with data.lock; but 10,000 files' peak memory 1.55 for the no-op and 1.45 for status, the reading thread's own huge pages | dropped, memory | [pinnacle pass](#the-state-beside-the-folder-walk) |
| placement | whether a folder in a file's way holds anything, from the folders the scan found something in | the same status: 7.26 to 1.54 s, with the other scans gone | kept | [scans](#scans-inside-per-file-loops-2026-11-12) |
| placement | what removing a run's unneeded files frees, worked out once for every blocker | 40,000 folders holding a file where files belong: status 10.45 to 1.75 s, `sync --dry-run` 10.22 to 1.55 s | kept | [scans](#scans-inside-per-file-loops-2026-11-12) |
| placement | a changed copy's name checked against a set of the target paths | `sync --verify --dry-run` of 10,000 files edited through other links beside 100,000: 0.403 | kept | [scans](#scans-inside-per-file-loops-2026-11-12) |
| placement | the files `--force` set aside found through an index | `sync --force` through 40,000 files where folders belong: 0.445 | kept | [scans](#scans-inside-per-file-loops-2026-11-12) |
| push | the files the data folder held other bytes of found through a set | a push taking 100,000 files, 10,000 of them edited: 0.699 | kept | [scans](#scans-inside-per-file-loops-2026-11-12) |
| lock and names | indexed pin membership and unique rename signatures | status at 40,000 0.146/0.254; 1,000 renames 0.0044/0.0031, musl +7.1 MiB | kept | [rename indexes](#indexing-pin-membership-and-renames-2026-11-12) |
| lock and names | indexed export selection | 40,000 names wall 0.0367/0.0275, but musl peak 137.6–144.4 to 184.2–184.4 MiB | dropped, memory | [export selection](#indexing-export-selection-2026-11-12) |
| lock and names | fast lock reader for commented locks | list at 100,000 files 0.213 to 0.100 s; peak memory 334 to 78 MB | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| lock and names | `add -i` writes data.toml once for the whole list | 10,000 links 415 to 4.6 s | kept | [list of links](#adding-a-list-of-links-2026-11-12) |
| lock and names | relocking looks datasets up in sets and maps | 40,000 links 31.7 to 21.6 s | kept | [list of links](#adding-a-list-of-links-2026-11-12) |
| lock and names | `path` and `sync <name>` read only the named folder, state records, pins and lock lines (five changes) | `path` of 3 files beside 100,000 415 to 29 ms, peak memory 0.305; `sync` 392 to 30 ms | kept | [named commands](#commands-that-name-a-dataset-2026-11-12) |
| lock and names | the number of processors read once a run | 0.997 | dropped, no win | [named commands](#commands-that-name-a-dataset-2026-11-12) |
| lock and names | waiting `path` calls finish once the project is ready | 32 waiters 3.24 to 0.25 s | kept | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| lock and names | a merge's data.toml edits made on one reading of each side | merge of 2,000 datasets 18.0 to 0.33 s, of 1,000 4.0 to 0.10 s; 1 and 10 flat | kept | [merge](#a-merge-of-many-datasets-2026-11-12) |
| lock and names | a drop across datasets written in one edit, its planned files counted once | drop of a file from each of 2,000 datasets 8.8 to 0.07 s; 1 and 10 flat | kept | [drop](#a-drop-across-many-datasets-2026-11-12) |
| lock and names | `list` builds only the view it shows | `--json list <name>` of 100,000 files 0.815, peak memory 0.746; `list` of them 0.899; 1 and 10 flat | kept | [list](#listing-only-what-is-shown-2026-11-12) |
| lock and names | a `--json` document encoded into pieces of at most 1 MiB, not one buffer that doubles | `--json export` of 100,000 files: peak memory 178.2 to 122.2 MiB on musl, 0.686 [0.685, 0.687], time 0.967; glibc 0.924, time 0.974; `--json list` 0.905 musl, 0.943 glibc; small documents flat | kept | [json pieces](#a-json-document-in-pieces-2026-11-13) |
| cache | bounded name discovery and checked-open metadata during verification | 100,000 objects up to 22% faster, but retry at 1,000 objects 1.343 ext4 / 1.323 xfs | dropped, control time | [verification pipeline](#pipelining-cache-verification-2026-11-12) |
| cache | the cache report's copies pass on parallel workers | 100,000 placed files 0.82 to 0.60 s, same peak memory | kept | [cache report](#the-cache-report-on-100000-placed-files-2026-11-11) |
| cache | a file's details taken from its open's check, a new object's birth read once, a clone hashed through the file that wrote it | import of 10,000 files of 1 to 8 KiB on xfs: 4 of 41 syscalls a file fewer, cpu 0.982 [0.979, 0.986], time 1.021 [0.980, 1.036]; ext4, reflink placement and links flat | dropped, no win | [syscalls a new object costs](#the-syscalls-a-new-object-costs-2026-11-12) |
| cache | the cache's 256 fan folders read on up to 8 workers, each object's path made after, on one thread | `cache verify` of 100,000 objects 0.371 [0.367, 0.379], of 10,000 0.471; the cache report beside 100,000 files 0.644; peak memory 0.93 to 0.97 | kept | [pinnacle pass](#reading-the-caches-folders-on-workers) |
| cache | more readers than the 8 hashing workers when checking files, for a deeper disk queue | cold ext4: `cache verify` of 100,000 objects 0.631 with 32, `status --verify` 0.534; warm 0.955 to 1.023; peak memory 1.17 to 1.71 | dropped, memory | [pinnacle pass](#more-readers-for-a-cold-disk) |
| push | files written, flushed by the batch rule, then read back | first push of 10,000 small files 36.7 to 2.7 s | kept | [folder mirror](#pushing-into-a-folder-mirror-2026-11-11) |
| push | staging shards only for large pushes, with compact-name retry | musl 100,000 files: time 0.732, peak RSS 1.042 [1.024, 1.050] | dropped, memory regression | [conditional shards](#conditional-mirror-staging-shards-2026-11-12) |
| push | a push's unfinished files spread over 16 folders | 100,000 files 0.729, but 50 files 1.046 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| push | push's files made in `tmp/` and renamed out of it by one thread each, behind a lock, or by fewer workers, to stop the kernel spinning on those folders' locks | cpu 0.42 to 0.97, wall 1.07 to 2.65 | dropped, wall time | [pinnacle pass](#pushs-spinning) |
| build | memoized immutable recipe-input contents, three versions | 4,000-recipe status ratios 0.0140 glibc and 0.0092 musl, but no-reuse controls slowed about 1% | dropped, control time | [recipe inputs](#memoizing-recipe-input-contents-2026-11-12) |
| build | a step's exit wakes the build's watch | a 10,000-file recipe 0.889, but on musl peak memory 1.061 | dropped | [nine jobs](#ab-passes-from-a-profile-of-nine-jobs-2026-11-12) |
| process | transparent huge pages turned off | peak memory 0.33 to 0.73, but a no-op sync and placing slower. left on: 100,000 files 0.884 and the no-op 0.918, for peak memory 1.17 to 2.47 | dropped, huge pages stay on | [lab](#ab-passes-on-the-linux-lab-2026-11), [placement](#ab-passes-on-placement-and-startup-2026-11) |
| process | mimalloc's huge pages turned off on musl, by `MIMALLOC_ALLOW_THP=0` or the crate's `no_thp` feature | peak memory 0.35 to 0.89, as `add -i` of 1,000 links 94.7 to 35.1 MiB, but work over many files 1.10 to 1.23 slower: status of 100,000 files 1.176, placing 100,001 from the cache 1.133, a no-op over 1,000 datasets 1.154, `drop` across 1,000 1.230 | dropped again, huge pages stay on | [huge pages at the release](#mimallocs-huge-pages-at-the-release-candidate-2026-11-13) |
| process | mimalloc for libc's own allocations on musl | no-op sync 0.833 | kept | [placement](#ab-passes-on-placement-and-startup-2026-11) |
| process | a mimalloc purge delay of 10 ms for lean on musl | lean with `MIMALLOC_PURGE_DELAY=10` set before start: peak memory 0.703 to 0.922, time 1.013 to 1.068 | not built: nothing sets it for lean alone without unsafe code | [purge delay](#a-shorter-purge-delay-for-lean-on-musl-2026-11-12) |
| process | `panic = unwind` for a clean thread error | no faster anywhere; binary 14.5% larger | dropped | [lab](#ab-passes-on-the-linux-lab-2026-11) |
| process | profile-guided optimization, its training rebalanced toward manifests | musl, 10,000 datasets: `lock --check` 0.901 [0.895, 0.910], `list` 0.910, status 0.934; 100,000 files: status 0.937, settled sync 0.954; nothing slower, binary 14.8% smaller; two trainings of one corpus gave binaries 13.7 million bytes apart | dropped: below the bar, and a reproducible release would need a committed profile for each target | [pgo](#profile-guided-optimization-2026-11-13) |
| process | the runtime's threads joined, at most 25 ms, before exit | fixes a rare windows hang under `-vv`; wall within noise; a stopped run waits up to the bound | kept on windows, for correctness | [end of a run](#the-end-of-a-run-2026-11-12) |
| process | linux and macos runs end without waiting for the runtime's threads; keys looked for at the first request that needs one, their index read once | settled sync of 3 files 0.929 to 0.981 glibc and musl, back within 1.02 of a8f9dad7; `path` beside 100,000 0.981 to 1.004, still 1.019 to 1.025 of a8f9dad7 on glibc | kept | [tiny commands](#tiny-commands-2026-11-12) |
| process | none: the release candidate 7daf4747 against a8f9dad7 on commands of a few milliseconds | glibc `help` 1.030 and 1.036 [1.032, 1.041] in a second batch, `--version` 1.020 and 1.016 [1.011, 1.021], a settled sync of 3 files 1.010 and 1.017 [1.011, 1.022], 0.04 to 0.1 ms; a copy of a8f9dad7 against itself 1.000 to 1.005; musl flat | recorded, not chased: glibc only, 4 to 8% more startup instructions in a binary 1.5 MB larger | [tiny commands at the release](#tiny-commands-at-the-release-candidate-2026-11-13) |
| process | none: the pinnacle pass's four kept changes together, its last commit against 8580a81e | cold sync of 10,000 small files 0.881, of 100,000 0.906; zip 0.804, tar.gz 0.830, a zip 20 projects share 0.086; no-op of 100,000 files 0.823, status 0.839, `status --verify` 0.895; `cache verify` 0.377, cold on ext4 0.793; cache report 0.399; `add`, `get`, `path`, push and a few large files within 0.97 to 1.02; peak memory 0.84 to 1.09 | recorded | [the whole branch](#the-whole-branch) |

## final benchmark on the linux lab, 2026-11-09

the [lab](benchmark-design.md) on an azure Standard_NC24ads_A100_v4 (EPYC 7V13, 24 vCPU, nvme, loop-mounted ext4 and xfs), which another user's training job shared. "final" is the static musl build of d2bc3e7, as linux ships it; "main" is the static musl build of d168842, from before these passes. a lan case ran a warmup round and then seven measured ones, every tool once a round in a new random order; on 2026-11-08 the home cases ran three and the lossy s2 cases five, and lossy s1 ran fifteen. a time is the median of the measured rounds, and final/main the median of the paired ratios with its 95% bootstrap interval. the commands, round orders and every run's record stay with the lab's results, outside the repository.

curl, wget and aria2c, each within 17% of fetchloom on some lan case, ran in the same rounds. dvc, datalad and pooch had not changed and were at least 1.25 times slower than fetchloom on every lan case on 2026-11-08, so those runs stand, marked nov 8; main ran in both sessions, and its medians moved by 5.3% at most. the rows marked nov 8 in the first column are from that session too, with e8cff51: what changed since touches the cache, placement and process start, milliseconds against seconds of network time. that build kept transparent huge pages off, so their memory is left out. lossy s1 ran again, every run starting without tcp state from the one before ([scenarios](benchmark-design.md#scenarios)); its final/best interval resamples both tools' runs.

| case | main s | final s | final/main [95%] | best other tool | its s | final/best | peak MiB final, main |
|---|---:|---:|---|---|---:|---|---|
| ext4 lan s1, 1 GiB | 2.444 | 2.304 | 0.939 [0.906, 0.952] | curl | 2.791 | 0.826 | 71, 27 |
| ext4 lan s2, 1,000 files | 0.444 | 0.415 | 0.923 [0.898, 0.962] | curl | 0.484 | 0.858 | 75, 63 |
| ext4 lan s3, 10,000 files | 2.427 | 1.584 | 0.658 [0.639, 0.669] | curl | 1.743 | 0.909 | 138, 117 |
| ext4 lan s4, zip of 10,000 | 0.937 | 0.758 | 0.814 [0.803, 0.821] | curl and unzip | 0.835 | 0.908 | 119, 103 |
| ext4 lan s5, second project | 0.076 | 0.077 | 1.017 [1.011, 1.047] | pooch (nov 8) | 0.280 | 0.275 | 39, 33 |
| ext4 lan s6, no-op | 0.029 | 0.028 | 0.955 [0.944, 0.972] | wget | 0.116 | 0.239 | 23, 23 |
| xfs lan s1, 1 GiB | 2.423 | 2.339 | 0.951 [0.946, 0.965] | curl | 2.641 | 0.886 | 75, 27 |
| xfs lan s2, 1,000 files | 0.532 | 0.415 | 0.781 [0.759, 0.789] | curl | 0.495 | 0.839 | 77, 61 |
| xfs lan s3, 10,000 files | 3.002 | 1.787 | 0.601 [0.593, 0.609] | curl | 1.800 | 0.993 | 152, 115 |
| xfs lan s4, zip of 10,000 | 1.760 | 1.478 | 0.843 [0.831, 0.852] | curl and unzip | 0.849 | 1.741 | 123, 94 |
| xfs lan s5, second project | 0.095 | 0.073 | 0.772 [0.759, 0.798] | pooch (nov 8) | 0.271 | 0.270 | 37, 31 |
| xfs lan s6, no-op | 0.025 | 0.024 | 0.950 [0.931, 1.166] | wget | 0.116 | 0.204 | 23, 23 |
| ext4 home s1, 256 MiB (nov 8) | 23.014 | 23.020 | 0.998 [0.997, 1.000] | wget | 22.928 | 1.004 |  |
| ext4 home s2, 1,000 files (nov 8) | 8.949 | 8.957 | 1.001 [0.995, 1.001] | aria2c | 8.880 | 1.009 |  |
| ext4 home s7, resume (nov 8) | 13.238 | 13.367 | 1.010 [1.010, 1.011] | curl | 13.390 | 0.998 |  |
| ext4 far s1, 4 MiB |  | 17.795 |  | curl | 13.180 | 1.350 [0.694, 2.292] | 28 |
| ext4 far s2, 50 files (nov 8) |  | 5.779 |  | aria2c | 5.648 | 1.023 |  |
| ext4 poor s1, 1 MiB |  | 14.932 |  | wget | 16.994 | 0.879 [0.525, 1.212] | 21 |
| ext4 poor s2, 20 files (nov 8) |  | 8.100 |  | aria2c | 7.437 | 1.089 |  |

fetchloom verifies every downloaded byte, fills a reusable content-addressed cache and places from it; curl writes straight to its target and checks with `sha256sum` afterwards. fetchloom is ahead of curl on every ext4 lan case and level with it on xfs s3. it is behind on xfs s4, a zip of 10,000 members, where xfs's reflink bookkeeping and the lock on each folder's namespace dominate unpacking (see [placing](../architecture/placing.md)). on lossy single files no tool is reliably faster: every tool's 15 runs spread from about 3 to 30 s, by where losses fall in the transfer, and fetchloom's median is the slowest of the four on far s1 and the fastest on poor s1, both well inside their intervals. against main, only ext4 s5 is slower, by 1 ms, within the 2% the a/b rule allows.

peak memory is higher than main's on downloads. file bodies now come over their own HTTP/1.1 connections, up to four a host, which cut far s2 to 0.288 of its time. without HTTP/2's flow control, a lan download arrives faster than SHA-256 hashes it, and the hash queue, bounded at 16 MiB, fills. sent a 256 MiB file over HTTP/1.1 at full speed, main peaks as high: in two runs each, 48 and 60 MiB with transparent huge pages off and 68 and 85 MiB with them on, against 42 and 47, and 70 and 72, for final. huge pages stay on, since turning them off slowed placement and short commands ([placement and startup passes](#ab-passes-on-placement-and-startup-2026-11)).

### scale

one dataset of 100,000 or 1,000,000 files of 2,560 bytes, 1,000 to a folder, from the lab server on the lan profile. rounds alternate final and main: three at 100,000 files and two at 1,000,000. cold fills an empty cache; warm places from a full cache into a new project; settled is a sync with nothing to do; status, path and path-member read the project, path printing the dataset's folder and path-member one file's.

| case | main s | final s | final/main | peak MiB final, main |
|---|---:|---:|---:|---|
| 100,000 files, cold | 18.59 | 9.27 | 0.498 | 208, 223 |
| 100,000 files, warm | 1.02 | 0.94 | 0.928 | 201, 197 |
| 100,000 files, settled | 0.54 | 0.40 | 0.738 | 97, 97 |
| 100,000 files, status | 0.60 | 0.43 | 0.725 | 120, 120 |
| 100,000 files, path | 0.67 | 0.41 | 0.613 | 120, 97 |
| 100,000 files, path-member | 0.76 | 0.40 | 0.532 | 118, 119 |
| 1,000,000 files, cold | 215.77 | 116.47 | 0.540 | 1251, 1254 |
| 1,000,000 files, warm | 11.85 | 10.66 | 0.900 | 1166, 1081 |
| 1,000,000 files, settled | 6.41 | 4.55 | 0.710 | 754, 771 |
| 1,000,000 files, status | 7.29 | 5.45 | 0.747 | 850, 850 |
| 1,000,000 files, path | 7.35 | 5.78 | 0.786 | 850, 907 |
| 1,000,000 files, path-member | 8.65 | 4.89 | 0.566 | 854, 909 |

## a/b passes on the linux lab, 2026-11

each change below ran behind a switch in one build, against the same build with the switch off, in paired rounds shuffled with the [lab](benchmark-design.md)'s other tools, five to twelve rounds a case. a ratio is the median of new over old, with its 95% bootstrap interval where one was kept; under 1 is faster or smaller. a change stayed only if it won where it aimed and made no lan case slower (an interval's lower end above 1.02); it then became the only code and its switch went. a change that lost went with its code.

| change | commit | result | outcome |
|---|---|---|---|
| several connections per host for file bodies | 92bc25d | far s2 0.288, poor s2 0.424; lan and home flat | kept |
| no closing resume record for small downloads | 0462f98 | ext4 s3 0.840, xfs s3 0.757, xfs s2 0.831 | kept |
| archive staging spread over 16 folders | b4fd36a | ext4 s4 0.798, xfs s4 0.830; cpu 0.66 | kept |
| download permits released before publication | 8fce749 | ext4 s3 0.925, s2 0.942 | kept |
| placement workers limited by parent folder, one on exfat | ff9e441 | wall flat; cpu 0.79 to 0.81; exfat cpu 85% lower | kept |
| project scan in bounded parallel batches | 12cdbf6 | status at 100,000 files 0.600 s to 0.442 s | kept |
| fast lock reader for commented locks | 0f4ecab | list at 100,000 files 0.213 s to 0.100 s; peak memory 334 MB to 78 MB | kept |
| waiting `path` calls finish once the project is ready | 7393540 | 32 waiters 3.24 s to 0.25 s | kept |
| download jobs built at admission | cfabec6 | peak memory 228 MB to 205 MB at 100,000 pins | kept |
| hosts interleaved in the download queue | 53957ca | two-host fixture 0.764 | kept |
| transparent huge pages off for the process | 24cd618 | peak memory 0.33 to 0.73 on every lan case, as ext4 s2 0.327 [0.311, 0.331]; wall flat there, but slower for a no-op sync and for placing from the cache, [measured alone](#ab-passes-on-placement-and-startup-2026-11) | dropped |
| unfinished downloads spread over 16 folders | faed8f4 | ext4 s3 0.902 [0.809, 0.962], xfs s5 0.712 [0.499, 0.814] | kept |
| a hardlink's details taken from its object | 79cc47c | xfs s4 0.933 [0.876, 0.954] | kept |
| one flush per file instead of one `syncfs` a batch | e803ca7 | ext4 s3 1.567, s2 1.183 | dropped |
| no-replace rename for new objects | 3dca829 | ext4 s4 1.090 | dropped |
| parent folder handles kept between batches | 8a93d8e | s5 1.268 and 1.297 | dropped |
| small copies instead of reflinks on xfs | 052c4e9 | xfs s4 1.245 | dropped |
| project state written as a stream | 0f4fddc | no measured win | dropped |
| fast large files split into ranges, from a link-rate probe | c095724 | live hugging face, 548 MB: 3.7 s against 5.6 s; but lan 1 GiB 1.075, home s1 1.035 [1.032, 1.037] and s7 1.025, with 1.5 to 1.6 times the cpu | dropped; splitting waits for a slow stream |
| shorter download queues | none kept | xfs s1 1.108 [1.060, 1.181]; peak memory 0.866 on xfs s1 alone | dropped |
| xfs staging through `O_TMPFILE` | none kept | xfs s4 0.955 [0.914, 1.026]; cpu 0.827 | dropped, no wall win |
| `panic = unwind` for a clean thread error | none kept | no faster anywhere; abort up to 4% faster on s2; binary 14.5% larger | abort kept, with a startup thread probe (7e6ab3b) |

## a/b passes on placement and startup, 2026-11

the final benchmark found two cases slower than main: a no-op sync and placing from a full cache. measured alone, the build before the changes below took 1.19 times main's time for the no-op and 1.25 times to place 100,000 files, and profiles traced it to three causes. each change ran as a static musl build against the build before it, with main and every build run once a round in a shuffled order: six rounds placing 100,000 files from a full cache, 15 placing 1,000 one-file datasets and 40 no-op syncs of those datasets. a ratio is the median of new over old with its 95% bootstrap interval, and the rule above applied.

| change | commit | result | outcome |
|---|---|---|---|
| placement work per file cut: each folder checked and grouped once, the root stripped once, results put in order without a sort, a worker woken only when one waits | 5911edb | 100,000 files 0.840 [0.832, 0.862]; 1,000 datasets 0.955 [0.948, 0.962]; no-op flat | kept |
| transparent huge pages left on | 914b739 | 100,000 files 0.884 [0.873, 0.889]; 1,000 datasets 0.971 [0.959, 0.977]; no-op 0.918 [0.910, 0.924]; peak memory 1.17, 2.47 and 1.72 | kept |
| mimalloc for libc's own allocations on musl | d2bc3e7 | no-op 0.833 [0.828, 0.838]; 1,000 datasets 1.007 [0.995, 1.015]; 100,000 files 1.017 [1.002, 1.021] | kept |

with all three, against main: 100,000 files 0.944 [0.934, 0.948], 1,000 datasets 1.003 [0.991, 1.017] and the no-op 0.909 [0.905, 0.917].

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

## live sites against their own tools, 2026-11-09

each service's public dataset from the [benchmark design](benchmark-design.md#tools), fetched on the shared machine of the final benchmark by fetchloom and by the service's own tool: five rounds, both tools in a new random order each round, every run with empty caches. `bench/live.py` ran them. fetchloom is the static musl build of f49f38e; the others are zenodo_get 3.1.0, `hf download` from huggingface_hub 0.35.3 with hf_xet 1.8.0 followed by `sha256sum -c`, kagglehub 0.3.13 and datahugger 0.13. a run counts only when every file it fetched matches the service's published SHA-256. a live site's speed changes from minute to minute, so, as the design says, each tool gets its median and range rather than a ratio.

| service | dataset | files · size | fetchloom, median and range | own tool | its median and range | CPU s, fetchloom and own tool |
|---|---|---|---|---|---|---|
| zenodo | 8164711 | 1 · 102.0 MiB | 42.3 s, 38.0 to 43.9 | zenodo_get | 56.0 s, 34.4 to 101.7 | 0.84, 1.20 |
| hugging face | stanfordnlp/imdb, plain_text parquet | 3 · 79.6 MiB | 5.3 s, 4.1 to 6.7 | `hf download` | 18.6 s, 15.5 to 19.7 | 0.35, 0.97 |
| kaggle | zynicide/wine-reviews, version 4 | 1 · 50.5 MiB | 1.95 s, 1.81 to 2.63 | kagglehub | 7.2 s, 3.3 to 13.2 | 0.22, 0.65 |
| figshare | 10.6084/m9.figshare.5616403 | 3 · 7.1 MiB | 1.51 s, 1.50 to 1.53 | datahugger | 7.2 s, 6.0 to 9.0 | 0.07, 0.71 |
| dataverse | doi:10.7910/DVN/TJCLKP | 3 · 295 KiB | 1.03 s, 0.99 to 1.34 | datahugger | failed in 5 of 5 | 0.03, none |

on hugging face, kaggle and figshare, fetchloom's slowest run was faster than the other tool's fastest. zenodo sends one file at its own pace, and its spread covers the difference: zenodo_get's fastest run beat fetchloom's fastest, and its slowest took more than twice as long. datahugger fetched dataverse's ingested `.tab` copy of a table instead of the published `.tsv`, which its own check reported as a checksum mismatch, so none of its runs counted; fetchloom fetched the published files. peak memory was 27 to 55 MiB for fetchloom and 36 to 127 MiB for the others.

## pushing into a folder mirror, 2026-11-11

`push` first flushed each file it wrote before reading it back and naming it, and on a busy disk that flush took about 20 ms a file. the change writes all of a push's files, puts them on the disk by the cache's batch rule, then reads each back and names it; both ways flush the folders that gained names at the end. one release build ran both behind a switch, both in every round, the order swapped each round, each run into a new mirror folder after a `syncfs` of the disk and 2 s of rest. the mirror, the cache and the project were on the os disk, ext4, of the shared 24-core machine, which other work kept at a load average of 44 to 62. every run ended with status 0, every file under its name and nothing left in `tmp/`, and in the first two rounds every file was hashed against its name. a ratio is the median, over rounds, of the time together over the time with each file flushed, with its 95% bootstrap interval.

| workload | rounds | each file flushed, median and range | together, median and range | ratio |
|---|---:|---|---|---|
| 10,000 files of 9 to 13 bytes, first push | 6 | 36.73 s, 34.72 to 57.42 | 2.71 s, 2.06 to 5.67 | 0.074 [0.058, 0.121] |
| 100 files of 10 MiB | 6 | 32.74 s, 26.69 to 61.59 | 20.02 s, 18.92 to 21.05 | 0.619 [0.378, 0.717] |
| one file of 1 GiB | 12 | 20.62 s, 17.26 to 42.56 | 19.04 s, 17.81 to 37.05 | 0.988 [0.888, 1.065] |

one file is under the batch rule's 64, so both ways flush it on its own; its row measures reading it back through a new handle after closing the one that wrote it, which cost nothing measurable. system cpu time was 2.91 s against 3.05 s for the small files, and the same for the large ones. built as committed, at a load average of 47, a first push of the 10,000 files took 2.58 to 2.93 s in three runs, and a second push, which finds each file with one look at its name and writes nothing, 0.02 s in nine.

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

## upgrading a bucket prefix, 2026-11-12

`lock --upgrade` of a dataset pinning five tar shards of 2,037,760 bytes from an s3-compatible stand-in on this machine, after a sixth appeared there. the stand-in states each object's size and an ETag in its listing and ignores conditional requests, as wf-audio's did; such a service's ETag is no MD5. the counts are the upgrade's alone, from `--json` work counters; one run each, since they do not vary.

| build | stand-in | requests | bytes downloaded | objects written |
|---|---|---:|---:|---:|
| c9da2d6 | ignores conditional requests | 7 | 12,228,135 | 126 |
| reuse by listed ETag and size | ignores conditional requests | 2 | 2,039,335 | 121 |
| c9da2d6 | answers a matching `If-None-Match` with 304 | 7 | 2,039,335 | 121 |
| reuse by listed ETag and size | answers a matching `If-None-Match` with 304 | 2 | 2,039,335 | 121 |

the two requests are the listing and the new shard. the stand-in that answers 304 sent no bytes for the five old shards before the change either, but took a request for each.

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

## max's requests at once on near and far servers, 2026-11-12

max runs 32 requests at once, 16 to one host. against a server that waits 50 ms before each answer that wins 3.6 times over the default level; against one nearby it only holds more downloads in memory at once: `add -i` of 1,000 links of 90 to 110 kB from a local server peaked at 31.2 MiB at max and 24.3 at the default level on glibc, 109.0 and 92.9 on musl. two ways to spend those connections only where they win were built and measured, each in one experiment build of 5fe2be57 where `FETCHLOOM_EXP_FIXED_MAX=1` restored the fixed 32 and 16 for the base arm, with the default level as a third arm. cases: `add -i` of 100 links of 4 KiB from a python server that sleeps 50 ms before each answer, and of the 1,000 links from a local python server, both on tmpfs; the servers ran on cores 0-11, the client under the measure lock on cores 12-23, interleaved seeded rounds after a warmup, with ratios against the fixed arm and 95% paired bootstrap intervals from `bench/ab.py`.

| change | case | libc | n | time ratio [95%] | peak memory ratio [95%] |
|---|---|---|---:|---|---|
| start at 4 to one host and 8 in all, double while a round of finished requests brings data in 25% faster than the round before, go back a step when one does not | 50 ms server | glibc | 12 | 1.709 [1.223, 2.386], 637 ms to 1.157 s | 0.962 [0.943, 0.991] |
| | local server | glibc | 15 | 0.933 [0.758, 1.065] | 0.847 [0.810, 0.876], 31.4 to 26.4 MiB |
| start at 16 and 32, narrow to 4 and 8 once the first round of finished requests brings each connection 8 MB a second or more | 50 ms server | glibc | 12 | 0.997 [0.992, 1.007] | 1.007 [0.984, 1.032] |
| | | musl | 10 | 1.000 [0.983, 1.005] | 0.972 [0.944, 1.000] |
| | local server | glibc | 20 | 0.972 [0.911, 1.017] | 0.897 [0.879, 0.913], 31.2 to 28.1 MiB |
| | | musl | 20 | 1.015 [0.997, 1.034] | 0.951 [0.924, 0.975], 109.0 to 102.1 MiB |

the default level against fixed max, in the same rounds: the 50 ms server 3.565 [3.525, 3.575] on glibc and 3.545 [3.501, 3.565] on musl; the local server's peak memory 0.765 [0.749, 0.778] and 0.848 [0.835, 0.869].

growing from the default's numbers loses max's win where it is the point: a debug log of one run showed the first doubling, measured on a round that still held requests begun before it, bringing data in only 1.34 times faster, and other runs went back a step there, which the wide interval shows. starting at max and narrowing keeps the far case, but the first round has already run 16 downloads at once, so the local case keeps about half of max's extra memory, 10% on glibc and 5% on musl, neither interval clearing a 10% bar for the code it adds. both were dropped; max stays a fixed 32 and 16, which [resources](../../user/resources.md#which-to-pick) already tells people to pick for far servers. a level that knew a server was near before its first round could do better; nothing read today says so.

## an indexed project state, 2026-11-12

the state is now one json document whose records are cut into blocks of 64 KiB, each block's first path and byte range listed after them, and where the records end written in its first 48 bytes ([state reading](../architecture/project.md#state-reading)). a read within the named folders, as a ready `path` and a settled `sync <name>` make, reads those 48 bytes, what follows the records and the blocks that can hold paths in the folders: 81,422 of 24,719,656 bytes for `path tiny` beside 100,000 records, in the `read_state` span of a trace, where the release before read all 24,700,438. whole reads parse the document as before. the version went to 2, so a version 1 state counts as empty.

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

## a stop during the data folder walk, 2026-11-12

the walk that lists every file under the data folder for a survey looked at no stop, so a first ctrl-c during it waited for the whole listing, which on a network filesystem of a million files can take many seconds. it now looks at the run's stop before each folder it lists, and `Survey::read` ends with `interrupted`. `a_survey_ends_at_a_stop_asked_before_it` lists a data folder with a stop already asked: it failed while the walk ignored the stop, and again with the check taken out.

an atomic load per folder costs nothing measurable: status of 100,000 files in 102 folders, in place on ext4 over nvme, efbf3adf against the clean release build of this batch, 0.996 [0.988, 1.003] on glibc (43 pairs) and 1.003 [0.997, 1.011] on musl (46 pairs), peak memory 79.1 to 79.6 MiB and 95.2 to 95.8 MiB. how long a stop takes on a large slow tree was not timed.

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

## the syscalls a new object costs, 2026-11-12

the perf-io2 branch left a half-built change that asks the file system less while storing and placing a file. a file's size, and a reflink's mode, come from the check its open already made; a new object reads its details once when it is made, for both its start and its birth, and seals with that birth instead of asking again; and on linux the cache opens a new object to read as well as write, so a clone is hashed through that file, rewound, instead of opened again by its path. `strace -c -f` of an import of 10,000 files on xfs counted 413,887 syscalls before and 373,860 after: 30,001 fewer `statx`, 10,001 fewer `openat` and as many fewer `close`, 10,001 more `lseek`. that is 4 of the 41 syscalls a file costs, but 1.8% of the cpu and none of the time.

| run | pairs | time [95%] | cpu [95%] | peak memory [95%] |
|---|---:|---|---|---|
| import of 10,000 files of 1 to 8 KiB on xfs | 19 | 1.021 [0.980, 1.036] | 0.982 [0.979, 0.986] | 1.001 [0.993, 1.013] |
| the same on ext4 | 20 | 0.995 [0.985, 1.011] | 0.993 [0.989, 0.996] | 1.003 [0.994, 1.010] |
| a sync placing them again by reflink on xfs | 20 | 1.004 [0.991, 1.017] | 0.999 [0.994, 1.004] | 0.995 [0.986, 1.008] |
| `add -i` of 1,000 links of 90 to 110 kB on tmpfs | 20 | 0.997 [0.975, 1.022] | 1.009 [0.986, 1.018] | 1.007 [0.990, 1.018] |

one glibc release build of the [scans](#scans-inside-per-file-loops-2026-11-12) change with this one behind a run-time switch, `FETCHLOOM_EXP_CHECKED_IO`, against itself without it; rounds interleaved after a warmup under the measure lock on cores 12-23, on ext4 over nvme and on xfs loop-mounted there. one xfs import round had a load burst and is left out. nothing nears the 10% bar, so the change and its two tests were dropped; the patch stays outside the repository.

## resources levels against default, 2026-11-13

what each [level](../../user/resources.md) trades, on the eight workloads of the perf audit's level pass, four of them on musl too. lean sets 2 requests at once, 2 to one host, a 128 MiB buffer budget and 2 local workers; max sets 32 and 16 requests and keeps default's workers and memory. every arm ran the same binary with only `FETCHLOOM_RESOURCES` changed. a ratio is level over default, the median of paired rounds with its 95% bootstrap interval.

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

glibc and musl release builds of bec45367, interleaved rounds after a warmup under the measure lock on cores 12-23. projects and caches on tmpfs, so flushes cost nothing; links from a local server on tmpfs, the zip and the 1 GiB file from the nvme disk's page cache. the copied 100,000-file and large projects were synced once by this build before the runs, since a state from before the [indexed format](#an-indexed-project-state-2026-11-12) reads as empty, and every file would be hashed. rounds with more than 12 other cores busy are left out; the settled sync and `cache verify` lost most rounds to load, and all 40 of each give the same answer: lean 1.095 [1.062, 1.122] and 1.963 [1.927, 1.987], max 1.003 and 1.009.

every level's folders had names of one length. in a first pass they did not, and a project path five letters longer alone cost 3.3 MiB at the peak of the sync of 40,000 datasets, 1.023 [1.022, 1.026] for the same build without a level, enough to fake a 2 to 3% memory saving for both other levels. against the audit's pass on efbf3adf, whose levels shared one folder, every time ratio here is within its interval or at most 5% from it. the memory ratios overlap its intervals but three: max's on 1,000 local links on glibc, 1.318 then and 1.274 now; lean's on `cache verify`, 1.000 and 0.985; and lean's on the large files' verification, 0.621 and 0.673, where both levels' peaks rose by about 2 MiB. so nothing since has moved what the levels trade by more than that.

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

## hardlinks before reflinks on xfs, 2026-11-13

on xfs a reflink of a small file costs far more than a hardlink. read-only placements on windows already try a hardlink first; one experiment build, `FETCHLOOM_EXP_HARDLINK_FIRST`, did the same on linux, for read-only placements outside synced folders as on windows. every placed file then had two names, its own and the cache's object, where with reflinks each had one.

| run on xfs, project and cache there | pairs | time [95%] | cpu [95%] | peak memory [95%] |
|---|---:|---|---|---|
| sync of a zip of 10,000 files of 4 KiB into a new cache | 15 | 0.776 [0.751, 0.782], 1.549 to 1.198 s | 0.590 [0.582, 0.599] | 0.981 [0.951, 1.068] |
| sync placing 10,000 files of 1 to 8 KiB the cache holds | 15 | 0.358 [0.333, 0.469], 461 to 166 ms | 0.259 [0.255, 0.266] | 1.000 [0.979, 1.002] |

a musl release build of df4d2a5b with the switch, interleaved rounds after a warmup under the measure lock on cores 12-23, xfs loop-mounted on the nvme disk and shared with other work, which flagged most rounds as loaded; the clean rounds alone give 0.767 [0.751, 0.781] in 9 pairs and 0.484 [0.409, 0.539] in 6.

the speed is the isolation. a reflink shares blocks until either side writes, so an edit to a placed file, after a chmod or by a program that ignores the read-only bit, leaves the cache and every other project as they were. a hardlink is the cache's object, so the same edit changes the object and every project's link to it at once, and fetchloom can only find it on the next run, put the pinned bytes back and keep the changed copy beside them. ext4, which has no reflinks, already works this way, and xfs would join it. the [placement order](../decisions.md) puts reflinks first for that isolation, so a speed rule cannot trade it away: not adopted, a placement decision to make with these numbers.

## a json document in pieces, 2026-11-13

the scale map found `--json export` of 100,000 files holding far more memory than plain `export`. its document, 34.2 MB here, was encoded into one buffer that doubled as it grew, a copy at each doubling, and the musl build's allocator keeps freed memory for its [purge delay](#a-shorter-purge-delay-for-lean-on-musl-2026-11-12), longer than an export runs: on musl `--json export` peaked at 178.2 MiB against 111.9 for plain `export`, on glibc 105.8 against 87.6. the document now goes into pieces of at most 1 MiB, the first 8 KiB, so nothing is copied or freed as it grows. it is still whole before its first byte goes out, since stdout cannot take back half a document when encoding fails.

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

## small leftovers from the perf maps, 2026-11-13

ideas the perf maps left open whose a/b took one build, each kept or dropped by the ledger's rule.

- the data folder walk built each file's path with `format!`, which grows the string about twice a file, some 200,000 growth calls at 100,000 files. making it at its final length, one experiment build switched by `FETCHLOOM_EXP_PRESIZE`, gave `--json status` of 100,000 files on tmpfs 0.986 [0.970, 0.998] and the settled sync 0.991 [0.979, 1.004], 19 and 20 pairs on glibc, peak memory flat. dropped, below the bar.
- a split download writes each chunk of a stream past the file's start as it arrives and hashes it on the way, where the first stream's chunks gather into 1 MiB writes hashed on a worker, and joining a stream's bytes to the start reads them back through a new 1 MiB buffer. a download splits only once one stream of a file over 64 MiB runs below 1 MB/s for 2 s, so the network sets its pace. a 96 MiB file from a local server holding each connection to 0.8 MB/s, with a musl build of df4d2a5b: on one stream 125.8 s; split, 36.8 s for 0.27 s of cpu, 0.7%, which covers hashing every byte and reading back the three quarters the later streams wrote. a cheaper handoff or buffer cannot reach the bar, so neither was built: dropped.
- each SHA-256 written to the state, data.lock or a JSON answer was first made into a new 64-byte string. the cpu map found writing them from a buffer on the stack 11 to 17% faster when rendering small states alone. one experiment build per libc of 7201f052, switched by `FETCHLOOM_EXP_HEX_STACK`, printed the same bytes, and as whole commands: `--json export` of 100,000 files, 200,000 digests, 0.960 [0.955, 0.969] on musl and 0.960 [0.947, 0.969] on glibc; the sync that places one file and writes the 100,000-record [indexed state](#an-indexed-project-state-2026-11-12) 1.002 [0.968, 1.028] and 1.002 [0.970, 1.014]; a sync of 10,000 datasets, `add -i` of 1,000 links and `--json status` of 100,000 files within 1.2%; peak memory flat. dropped, below the bar.
- the cpu map found sonic-rs parsing states about 20% faster than serde_json on its own, but only about 5% of a whole status, and that was before the [indexed state](#an-indexed-project-state-2026-11-12). one experiment build per libc of 7201f052 read the whole state and wrote its records with sonic-rs 0.5.10 under `FETCHLOOM_EXP_SONIC`. its state held the same records in the same order as serde_json's, 24,719,420 bytes either way, and serde_json read it back clean. as whole commands, musl then glibc: the settled sync of 100,000 files 0.947 [0.930, 0.968] and 0.954 [0.942, 0.975]; `--json status` of them 0.965 [0.939, 0.975] and 0.981 [0.942, 0.999]; the sync writing their state 0.956 [0.934, 0.966] and 0.974 [0.963, 0.986]; a sync of 10,000 datasets and a settled sync of 3 files within 1%, the latter's peak memory 1.031 [1.016, 1.058] on glibc. it adds 16 packages to Cargo.lock, rkyv among them, and about 0.3 MB to the binary. other users' load flagged nearly half the musl rounds; these keep them, and the clean rounds give the same picture. dropped, below the bar.

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

## short-copy EOF dispatch, 2026-11-12

the operating-system sweep of a8f9dad7 found that `copy_stoppable` calls `io::copy` again after a short completed 1 MiB block has already reached EOF. an optimized standalone Rust reproduction of that loop copied 10,000 distinct 4 KiB files, one binary with a runtime switch, each arm once per seeded shuffled round after a warmup. every output was checked outside the clock. these are primitive measurements, not timings of a changed fetchloom command.

| filesystem | rounds | current loop, median and range | short-block stop, median and range | new/old [95%] |
|---|---:|---|---|---|
| ext4 over nvme | 10 | 0.454 s, 0.449 to 0.464 s | 0.429 s, 0.421 to 0.438 s | 0.941 [0.932, 0.949] |

strace counted one fewer `copy_file_range`, one fewer `sendfile` and two fewer `statx` calls per file, 40,000 calls across this fixture. tmpfs medians suggested about 11% less time, but the first batch and its repeat both contained unrelated-load bursts, so neither supplied a clean acceptance result. the memory fields of those early runs also inherited the harness parent's RSS floor and are not memory-regression evidence.

the ext4 gain is below the 10% keep rule, and there is no checked whole-command win. the change is dropped; the copy loop and its 1 MiB cancellation boundary stay unchanged. raw commands, counts and both noisy batches remain with the sweep outside the repository.

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

## an import of 100,000 files, 2026-11-12

`add <folder>` kept the list of files its walk found, each with its absolute path in the folder, until the end of the command, though the line telling what it copied was its last use: the list stayed alive while the import was pinned and its files placed, which is where the peak of a large import falls. the scale map's replay of 100,000 files found 12.4 MB of those paths and 7.3 MB of the list's spare room still live at the peak. the list is now freed once that line is written. the import tests hold the lines, warnings and pins it gives.

the fixtures are the scale map's files-n folders, hardlinked into a folder of their own, run as `--json --offline --yes add <folder> --as imported` into an empty project with an empty cache of its own on ext4 over nvme, both made again before each run and deleted after it, outside the clock. one release build chose the old or new way by `FETCHLOOM_EXP_IMPORT_DROP`, measured as in [a merge of many datasets](#a-merge-of-many-datasets-2026-11-12): 30 rounds at 1 and 10 files, 30 and 20 at 10,000 on glibc and musl, 16 at 100,000. peak memory is the run's maximum RSS, from wait4.

| files | build | wall, old to new | new/old | peak memory, old to new | new/old |
|---:|---|---|---|---|---|
| 1 and 10 | glibc | 8 and 13 ms, the same | 1.002 [0.989, 1.018], 1.010 [0.975, 1.024] | 14.0 MiB, the same | 1.000 |
| 1 and 10 | musl | 13 and 12 ms, the same | 0.979 [0.974, 1.012], 0.999 [0.990, 1.014] | 18.6 MiB, the same | 1.000 |
| 10,000 | glibc | 0.427 to 0.428 s | 1.002 [0.990, 1.013] | 31.9 to 30.4 MiB | 0.955 [0.948, 0.961] |
| 10,000 | musl | 0.433 to 0.437 s | 1.007 [0.992, 1.026] | 118.5 to 120.7 MiB | 0.974 [0.953, 1.017] |
| 100,000 | glibc | 4.61 to 4.64 s | 0.986 [0.979, 1.011] | 180.9 to 160.7 MiB | 0.890 [0.887, 0.890] |
| 100,000 | musl | 4.30 to 4.34 s | 1.001 [0.988, 1.020] | 216.3 to 194.6 MiB | 0.908 [0.898, 0.927] |

kept: 11% less peak memory for 100,000 files on glibc and 9% on musl, below the replay's 17% of heap, with no time interval away from 1. the import's time is mostly copying the files into the cache.

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

## evidence limits

the sections from here on are non-reproducible historical observations, not benchmark results from the current harness. raw run records and complete reproduction commands were not retained alongside these tables. commits establish the implementation, not the measured ratios. missing machines, profiles, counts or statistics remain unknown; do not infer them or treat a reported percentage as a current guarantee.

use the [benchmark workflow](../engineering/benchmarks.md) for new comparisons with retained commands, run records and independent checking.

## conditions

W means windows 11 with defender real-time protection on. L means ubuntu 24.04 in WSL 2 on ext4. both use the same 11th gen intel i7-1185G7 laptop with 4 cores, 8 threads, 32 GB and nvme. unless a row says otherwise, these are release builds measured on 2026-10-30 or 2026-10-31.

baseline f9bbda5 used at least ten runs after three warm-ups on an otherwise idle machine; its command measurements are in [budgets](budgets.md). later rows preserve their own counts. "not recorded" means no commit, count, median or range was retained for that field; a range's midpoint is not a measured median.

## storage and memory

small-download runs used 1,000 URLs of 100 KiB, one dataset per URL, an empty cache and a local linux server. the older path paid three flushes per file, about 3.3 ms each. the retained change removes checkpoint and index flushes, not object, change-mark, unpack-record or registration flushes.

the memory fixture was a 100,000-member zip of 1 MiB. RSS is resident set size, the process's physical-memory footprint. `/usr/bin/time` measured peak RSS without the profiler; heaptrack identified allocations separately. the recent hash-thread and copy-flush records name linux but do not record a machine model, exact median or full per-configuration range beyond the values below.

## placement and settled sync

warm-placement runs copied `data.toml` and `data.lock` into an empty project using a fully populated cache. fixtures were 10,000 files of 4 KiB in 100 folders and 8,000 files of 2 KiB in one folder. on 2026-10-30, other builds kept the laptop at full load. fd64035 and 2ecb70a alternated ten runs per side. `FETCHLOOM_TRACE` separated command work from process startup, which alone took 1 to 3 s under that load.

placement was 2 to 3 times slower than in the earlier quiet window. the no-readback experiment won 3 of 10 pairs for 10,000 files and 6 of 10 for 8,000; individual pairs differed by up to 2 times either way. those medians did not establish a repeatable gain, so the experiment was reverted.

## network

network clients ran on W against a local test server's eth0 address with shaped traffic and an empty cache per run. "lan", "home" and "far" are the recorded profile names; their exact shaping parameters are not retained here. live rows name the service. paired configurations alternated. these results cannot be treated as fixed speeds for those services.

## observations and rejected experiments

parenthesized bounds are observed minima and maxima. when only a time, rate or peak was recorded, the median and range columns say so rather than supplying a new statistic.

| workload or change | commit | machine and runs | median or recorded value | range | outcome |
|---|---|---|---|---|---|
| local-download baseline | f9bbda5 | W and L; baseline counts | about 180 MB/s W; 460 MB/s L | not recorded | includes hashing and payload flush |
| in-memory SHA-256, 1 GiB | not recorded | same laptop; best of three | median not recorded; sha2 and ring rates at right | sha2 1.70 to 1.83 GB/s; ring 1.63 to 1.69 GB/s | keep sha2 |
| MD5 alone and SHA-256 with MD5 | not recorded | same CPU; best of three over 1 GiB | median not recorded; 0.68 GB/s and 0.48 GB/s | not recorded | compute MD5 only when needed |
| separate hash thread, local 1 GiB | 1648e29 | linux, machine not recorded; four interleaved pairs | median not recorded; about 25% faster | before 2.06 to 2.27 s; after 1.58 to 1.65 s | keep `HashPipe` |
| hand response chunks to the writer and hash threads in batches | 0616aa1 to 57438ae | L, benchmark lab: 1 GiB over HTTPS and HTTP/2 from a separate network namespace, empty cache, page cache dropped; five interleaved pairs | median 5.37 s to 4.03 s, 25% faster; futex calls 117,064 to 6,702 | before 4.73 to 5.73 s; after 3.53 to 4.64 s | keep: 256 KiB batches to the writer, 1 MiB to the hasher |
| the same, writer batches alone | 0616aa1 to a9c909f | same lab; five interleaved pairs | no change: 4.05 to 6.54 s against 4.08 to 5.42 s | as stated | kept with the hasher batches; the hasher handoff was the cost |
| flush a batch of downloads once with `syncfs`, then name them | 57438ae to 3837774 | L, benchmark lab: 1,000 files of 100 KiB over HTTPS, empty cache, page cache dropped; five interleaved pairs | median 4.93 s to 1.79 s, 64% faster; an upper bound without any flush was 1.36 to 1.74 s | before 4.16 to 5.61 s; after 1.62 to 2.14 s | keep on linux 5.8 and later; elsewhere each download still flushes on its own |
| flush 10,000 copies of 4 KiB with `copy = true` | f68ef48 per-copy; 1ef51b7 batching | linux, machine not recorded; six interleaved pairs | reported times: 0.27 s without flushing, 9.19 s per-copy, 0.41 s one `syncfs`; separate medians not recorded | not recorded | keep payload durability and batch on linux |
| checkpoint and index flush removal, 1,000 small downloads | 0533392 | L, local lan; ten interleaved pairs | before 4.46 s; after 2.18 s | before 4.27 to 6.31 s; after 2.09 to 3.87 s | 51% faster; keep |
| archive-member batch flush | 26a3dbc | L, ext4; three runs each | median not recorded; after about half a second; per-file flush 0.7 ms | before 8.5 to 10.2 s | per-file path cost twenty times more; keep batch |
| compact placement metadata, add | 9d3c7b1 to 33825eb | L, 100,000-member zip; count not recorded | peak RSS 162 MiB to 165 MiB; median not recorded | earlier spread not recorded | no add improvement |
| compact placement metadata, warm placement | 9d3c7b1 to 33825eb | L, same zip; count not recorded | peak RSS 183 MiB to 151 MiB; median not recorded | not recorded | keep, still misses 150 MB |
| heaptrack add investigation | 33825eb | L; count not recorded | heap peak 116 MiB; median not recorded | not recorded | retained allocations and allocator arenas explain part of RSS |
| `MALLOC_ARENA_MAX=2` | 33825eb investigation | L, eight unpack writers; count not recorded | add peak 143 MiB; median not recorded | not recorded | no programmatic cap: needs unsafe `mallopt` or another allocator |
| build placement paths per file | 584c9c0 | no isolated measurement recorded | median and peak not recorded | not recorded | keep lower retained path allocation |
| peak memory after compact metadata and per-file paths | 32c6e22 | L, 100,000-member zip of 1 MiB over local HTTP; three runs each | add 136 to 142 MiB; warm placement 94 to 102 MiB | add 6.6 to 7.3 s, one run 40.9 s; placement 1.0 to 1.3 s | both under the 150 MB budget |
| no hardlink readback, retested | 32c6e22 | W, 10,000 files of 4 KiB in 100 folders; two sets of eight interleaved pairs under load | first set: after won 7 of 8, best 2.49 to 2.24 s; second set: after/before ratios 0.70 to 2.77 | outliers to 70 s on both sides | inconsistent again; dropped |
| directory-listing metadata | not recorded | W; count not recorded | about 15 times faster; absolute median not recorded | not recorded | keep; L still needs stat calls |
| live warm placement, hf:datasets/Matthijs/cmu-arctic-xvectors | not recorded | W and L, debug builds under load; single reported runs | about 8,000 files in one folder: W 30.5 s, L 3.8 s | not recorded | investigate placement, not network |
| live warm placement, tar.gz | not recorded | W, debug under load; one reported run | about 52,000 small files: 87 s | not recorded | investigate filesystem cost |
| trace, 10,000 files in 100 folders | fd64035 / 2ecb70a study; traced side not recorded | W under full load; one trace | command 3.85 s; object checks 0.47 s; placing 3.19 s; rest 0.19 s | not recorded | placement dominates |
| trace, 8,000 files in one folder | same study | W under full load; one trace | command 12.1 s; object checks 0.29 s; placing 11.7 s; rest 0.12 s | not recorded | same finding |
| bare links, 10,000 files | not recorded | W; counts and load not recorded | median not recorded; across 100 folders 2.2 s | one folder 10.3 to 12.7 s | spread placement |
| bare links, 8,000 files | fd64035 / 2ecb70a study | W under load; count not recorded | median not recorded; across 100 folders 1.7 s | one folder 10.2 to 12.3 s; single-thread link 0.6 to 3.7 ms | no trend as folder fills |
| thread-count probe, 8,000 links in one folder | same study | W under load; interleaved, count not recorded | medians not recorded | one thread 17 to 149 s; two 10 to 49 s; four 8.5 to 20 s; eight 10 to 19 s | keep 8 threads |
| readback probe, 10,000 spread links | same study | W, 8 threads; count not recorded | 2.48 s without reads; 2.44 s with reads; medians not recorded | not recorded | no readback win |
| folder checks | same study | W; count not recorded | 5 to 10 us per file; 54 to 97 ms per 10,000; medians not recorded | those recorded bounds | about 2% of placing, below the 10% change threshold; retain checks |
| no hardlink readback, 10,000 files, process | fd64035 to 2ecb70a | W full load; ten interleaved runs each | before 11.4 s; experiment 12.3 s | 8.0 to 27.1 s; 8.4 to 28.4 s | drop experiment |
| no hardlink readback, 10,000 files, command | same pair | same runs | before 10.1 s; experiment 10.8 s | 7.0 to 25.6 s; 7.2 to 27.1 s | drop experiment |
| no hardlink readback, 8,000 files, process | same pair | same runs | before 20.0 s; experiment 16.8 s | 14.6 to 27.9 s; 13.3 to 29.3 s | inconsistent pairwise result |
| no hardlink readback, 8,000 files, command | same pair | same runs | before 17.9 s; experiment 15.2 s | 13.2 to 26.4 s; 11.9 to 27.8 s | inconsistent pairwise result |
| settled sync, 10,000 files, command | fd64035 to 2ecb70a | W full load; ten interleaved runs each | before 200 ms; after 131 ms | 153 to 708 ms; 91 to 174 ms | keep shortcut |
| settled sync, 8,000 files, command | same pair | same runs | before 303 ms; after 158 ms | 174 to 480 ms; 102 to 371 ms | keep shortcut |
| settled sync, 10,000 files, process | same pair | same runs | before 1.27 s; after 1.21 s | 1.11 to 3.18 s; 1.07 to 1.61 s | startup obscures command gain |
| settled sync, 8,000 files, process | same pair | same runs | before 1.53 s; after 1.82 s | 1.41 to 5.48 s; 1.23 to 12.8 s | startup remains noisy |
| adaptive split, lan 1 GiB | 0cbc48b | W; ten interleaved pairs | one stream 5.54 s; adaptive 5.48 s | 5.00 to 9.45 s; 5.07 to 6.12 s | no split |
| adaptive split, home 1 GiB | 0cbc48b | W; ten interleaved pairs | one stream 97.55 s; adaptive 97.82 s | 96.39 to 106.79 s; 96.94 to 99.25 s | no split |
| adaptive split, far 100 MiB | 0cbc48b | W; five interleaved pairs | one stream 496.65 s; adaptive 174.03 s | 495.82 to 518.50 s; 135.81 to 181.26 s | 4 requests, 65% faster |
| adaptive split, live cifar-10 at `www.cs.toronto.edu` | 0cbc48b | W; three interleaved pairs | one stream 156.29 s; adaptive 39.81 s | 118.58 to 178.45 s; 37.23 to 40.81 s | 74% faster; one stream about 1 MB/s that night |
| split from first answer, lan 1 GiB | 1583af9 | W, 2026-10-30; count not recorded | one stream 5.96 s; split 7.00 s | 5.01 to 7.41 s; 6.99 to 8.57 s | 6 requests, 17% slower; one stream about 180 MiB/s |
| split from first answer, home 1 GiB | 1583af9 | W; count not recorded | one stream 95.6 s; split 96.7 s | 95.5 to 96.2 s; 95.6 to 97.4 s | 4 to 5 requests, 1% slower |
| split from first answer, far 100 MiB | 1583af9 | W; three runs each | one stream 500 s; split 215 s | 490 to 531 s; 191 to 285 s | 2 to 4 requests, 57% faster; use adaptive default |
| HTTP/1.1 instead of HTTP/2, live `hf:datasets/cais/mmlu` without `data.tar`, 180 files, 99 MiB | commit not recorded; same binary both sides | W; five interleaved pairs | HTTP/2 45.32 s; HTTP/1.1 46.03 s | 42.69 to 49.50 s; 45.31 to 48.19 s | drop; earlier HTTP/2 run of 92 s with eight retries did not reproduce |
| rename publication instead of hardlink plus remove | not recorded | W; estimated saving, no run count | about 1 s of a 34 s unpack; no measured median | not recorded | below 10%; drop |
| skip small-member flushes | not recorded | W, random-file fixture; one recorded run | unpack 54 s; 10,000 scans, scan median 24 ms | not recorded | scan cost remains; drop |
| 8 MB/s split trigger | fafec0c | W, live toronto; two pairs | one stream at 5.6 MB/s: 29.16 s; adaptive 38.86 s | not recorded | 33% later; neutral on home and lan, but one lan run split under load; lower to 1 MB/s |
| fat rather than thin LTO | earlier version, commit not recorded | machine and count not recorded | 7% smaller at same speed; absolute medians not recorded | not recorded | retain fat LTO |
| mimalloc | earlier version, commit not recorded | machine and count not recorded | within noise; medians not recorded | not recorded | keep system allocator |
| historical local 1 GiB, empty cache and second project | 240f17a | L, 2026-10-31; 3 blocks instead of planned 5 | 4.00 s; second project 0.01 s | not recorded | predates hint-flush change |
| historical local 100 MiB | 240f17a | same L run | 0.35 s | not recorded | same older build |
| historical local zip, 10,000 files | 240f17a | same L run | 0.87 s; second project 0.20 s | not recorded | same older build |
| historical local tar.zst, 10,000 files | 240f17a | same L run | 0.76 s | not recorded | same older build |
| historical 1,000 URLs of 100 KiB | 240f17a | same L run | lan 3.53 s; home 9.46 s | not recorded | later hint-flush rows supersede this cost |

related: [budgets](budgets.md), [windows defender study](windows-defender.md), [profiling](../engineering/profiling.md), [network](../architecture/network.md), [placing](../architecture/placing.md).

## a per-run metadata memo, 2026-11-12

100 datasets selected different 4 KiB files from five S3 references, each listing 100 files. `fetchloom --json --yes lock --dry-run` resolved them with an empty cache. a second manifest selected one file from one reference. an isolated veth added 1 or 50 ms RTT, without loss or a bandwidth cap. jobs stayed at 8 overall and 4 per host. every reported dataset, path and size was checked against the fixture, and a dry run wrote no lock.

one stock release executable per libc carried `FETCHLOOM_EXP_API_MEMO`, read once. the supplied interleaved harness ran both arms on cores 12–23 under the shared lock, with one warmup per arm after each acquisition. whole rounds with a load burst were excluded; failed runs remained failures. RSS below is GNU time’s child reading, not the Python parent’s inherited `wait4` high-water mark. times and RSS are medians, with observed ranges.

| selections / RTT | libc | quiet pairs | seconds, before to after | peak MiB, before to after |
|---|---|---:|---|---|
| 100 / 1 ms | glibc | 5/12 | 0.096 (0.095–0.098) to 0.026 (0.024–0.027) | 18.88 (17.77–18.98) to 18.53 (18.24–18.77) |
| 100 / 1 ms | musl | 11/12 | 0.099 (0.096–0.268) to 0.022 (0.022–0.024) | 39.12 (35.23–39.37) to 37.24 (35.15–37.37) |
| 100 / 50 ms | glibc | 11/12 | 0.741 (0.738–0.787) to 0.123 (0.123–0.124) | 18.75 (18.18–19.01) to 18.58 (18.25–19.20) |
| 100 / 50 ms | musl | 12/12 | 0.739 (0.737–0.792) to 0.121 (0.120–0.153) | 37.37 (37.12–39.38) to 37.19 (37.06–37.40) |
| 1 / 1 ms | glibc | 13/20 | 0.013 (0.012–0.017) to 0.013 (0.013–0.017) | 13.17 (12.87–13.34) to 12.96 (12.83–13.34) |
| 1 / 1 ms | musl | 20/20 | 0.013 (0.013–0.014) to 0.013 (0.012–0.014) | 20.96 (19.12–22.98) to 21.14 (19.21–21.21) |
| 1 / 50 ms | glibc | 16/20 | 0.111 (0.111–0.112) to 0.111 (0.111–0.112) | 13.06 (12.86–13.35) to 13.08 (12.73–13.29) |
| 1 / 50 ms | musl | 15/20 | 0.112 (0.111–0.112) to 0.112 (0.111–0.115) | 20.97 (20.90–23.04) to 21.14 (21.02–21.20) |

at 50 ms, requests fell from 100 to 5. paired time ratios were 0.166 [0.166, 0.167] on glibc and 0.163 [0.158, 0.164] on musl, clearing the 10% speed gate. the five quiet glibc pairs at 1 ms are diagnostic, not another qualified result. one musl baseline run at 1 ms hit the harness’s 70 s limit after 77 server GETs; its cause was not isolated. it is retained as a failed run, not discarded as load noise.

kept under the corrected default-memory rule: memory is a budget, not a requirement to spend no extra byte. the one-dataset musl control adds 0.18 MiB at both RTTs, well inside the ordinary default budget of about 150 MB. its paired RSS ratios were 1.0086 [1.0066, 1.0088] at 1 ms and 1.0087 [1.0048, 1.0089] at 50 ms; control time stayed within noise. the initial brief’s stricter control-memory wording was withdrawn. no allocator option was changed to conceal the increase.

the prototype shared in-flight requests and body bytes, bounded retained entries to one sixteenth of the memory setting, at most 64 MiB, and keyed them by the outgoing URL, headers and credential/bucket scope. only 2xx responses were retained; offline and mirror-only checks preceded reuse, and hints and mirror reads bypassed it. a dead origin download invalidated discovery without letting an older flight republish it. regression and mutation checks covered refusals, different URLs and credentials, paging, S3 signing, refreshed google tokens, cookies, cancellation, byte bounds and dead-link recovery. the memo is kept without its experiment switch; the memory cost is reported beside the gain.

build and raw evidence: the retained `net2/results/builds/v3` executables and source freeze, `memo-*-v3-1` paired records, `results/tests` red/mutation logs, and `scripts/ab.py` / `scripts/analyze.py`. the experiment used source base `27811da3`; earlier v1/v2 probes are separate and do not supply the keep decision.

## network slots while a request waits, 2026-11-12

one host returned 429 with `Retry-After: 5` before serving two distinct 4 KiB files; another served two distinct 64 MiB files, each paced at 8 MiB/s. the mixed `fetchloom --json --yes add -i inputs.txt` used `FETCHLOOM_JOBS=2 FETCHLOOM_HOST_JOBS=2`. this constrains connections, not the whole `lean` resources level: memory and local workers kept their defaults. the link had 1 ms RTT, no loss and no bandwidth cap. controls removed the limited host or used the normal 8/4 connection limits. native hash checks remained timed; every placed payload was independently checked with SHA-256 afterwards.

`FETCHLOOM_EXP_RELEASE_WAITS`, read once, switched both arms in each stock release executable. each shuffled batch held the shared lock for at most five minutes on cores 12–23, with one warmup per arm. the two musl controls each needed one bounded two-pair fill after a noisy round; every earlier row remains recorded. no permit-matrix run failed. the paired method and GNU-time child RSS are as in the metadata experiment above.

| workload / jobs | libc | quiet pairs | seconds, before to after | peak MiB, before to after |
|---|---|---:|---|---|
| mixed / 2/2 | glibc | 10/10 | 13.176 (13.072–13.200) to 8.159 (8.106–8.196) | 23.22 (22.45–25.93) to 21.25 (20.29–24.46) |
| mixed / 2/2 | musl | 10/10 | 13.185 (13.107–13.218) to 8.205 (8.193–8.214) | 50.34 (48.53–53.58) to 51.41 (45.96–53.52) |
| healthy host / 2/2 | glibc | 10/10 | 8.159 (8.103–8.217) to 8.114 (8.104–8.233) | 20.80 (20.21–22.30) to 20.59 (20.19–22.49) |
| healthy host / 2/2 | musl | 11/12 | 8.171 (8.104–8.221) to 8.117 (8.106–8.225) | 48.73 (44.03–54.69) to 49.45 (43.79–53.31) |
| mixed / 8/4 | glibc | 10/10 | 8.112 (8.106–8.128) to 8.114 (8.104–8.216) | 22.78 (21.00–23.73) to 22.75 (21.59–25.34) |
| mixed / 8/4 | musl | 11/12 | 8.210 (8.186–8.225) to 8.216 (8.200–8.224) | 52.51 (47.41–57.38) to 52.78 (50.00–58.19) |

mixed-run time ratios were 0.619 [0.619, 0.620] on glibc and 0.622 [0.621, 0.626] on musl, about 38% faster. server timestamps explain the change: the healthy host’s second request started about 5.008 s after the limited host’s first, versus 0.004 s after releasing sleeping requests’ slots. with 8/4 limits, both healthy requests already started immediately, so no default-two-host speedup is claimed. the control time intervals include 1 and end below 1.005. RSS varies between runs; its control intervals also include 1. the largest observed candidate peak was 58.19 MiB, inside the ordinary default budget.

kept. metadata requests, ordinary downloads and split ranges release their host and global slots during retry and host-quota waits, but keep the partial-file claim. requests retake slots and then check the current quota before sending. stop and errors release ownership. after rebasing, fixed-clock tests also caught and fixed spending NCBI’s 334 ms spacing twice during reacquisition, and a one-try hint retaining slots when quota arrived while it queued. every `Retry-After` floor remains.

five focused tests cover retry/quota sleeps, successful reacquisition and acquisition failure, a newer quota learned during reacquisition, NCBI spacing, and one-try hints. each faulty path was observed failing; deliberate release, quota-check, spacing and hint mutations were caught and restored. the recorded A/B source is `27811da3` plus the frozen v3 experiment. the later integration corrections concern spacing and hint paths, not this numeric-host fixture. raw evidence is retained under `net2/results/permit-*-v3-*`, `results/tests` and `results/builds/v3`. the switch is removed from the kept code.

## probing after an unknown quota, 2026-11-12

a local host allowed two successful requests per second, ten distinct 4 KiB files in all, and otherwise returned 429 with `Retry-After: 1` but no remaining-quota headers. the prototype admitted one recovery probe after the floor and widened after its first non-429 answer. older in-flight answers could not widen it. `FETCHLOOM_EXP_QUOTA_PROBE` selected the candidate; a second comparison enabled sleeping-slot release in both arms. default 8/4 limits, 1 ms RTT, native verification and an independent hash oracle were the same in both arms. each case has ten quiet paired rounds, one warmup per arm, and no failures.

| slot release in both arms | libc | total 429s, before to after (range per run) | seconds, before to after | peak MiB, before to after |
|---|---|---|---|---|
| off | glibc | 138 (13–14) to 138 (13–14) | 4.043 (4.041–4.044) to 4.044 (4.043–4.048) | 13.98 (13.77–14.53) to 14.00 (13.77–14.17) |
| off | musl | 137 (13–14) to 135 (13–14) | 4.043 (4.041–4.045) to 4.045 (4.043–4.089) | 24.96 (24.35–26.12) to 25.60 (23.88–25.95) |
| on | glibc | 138 (13–14) to 139 (13–14) | 4.043 (4.040–4.046) to 4.045 (4.043–4.047) | 13.96 (13.75–14.31) to 13.92 (13.61–14.18) |
| on | musl | 137 (13–14) to 136 (13–14) | 4.041 (4.040–4.044) to 4.044 (4.043–4.047) | 25.12 (24.14–26.25) to 25.73 (23.83–26.30) |

dropped. none of the comparisons reduced 429s by the required 20%; time differed by about 0.1%. one successful probe did not reveal the remaining quota, so widening immediately recreated the refusal burst. ordinary-file controls also ran on both libc targets and showed no established time or RSS regression. the proposed narrowing, its state and its switch are not shipped. red and mutation checks for serialization, old in-flight responses, widening and the `Retry-After` floor remain with the prototype evidence.

raw evidence: `net2/results/quota-*-v3-1`, the frozen v3 source and executables, and `results/tests`. these are local HTTP fixtures, not a claim about every provider or HTTPS/HTTP/2 profile.

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

## memoizing recipe input contents, 2026-11-12

dropped after three bounded versions. the prototype remembered fetched recipe-input content by both the pin object's identity and its member-filter object's identity. read-only passes borrowed those objects; a build froze fetched-input hashes after relocking, without caching code hashes or derived trees. the checks before and after executing a recipe remained live. no recipe-key encoding changed.

v1 used a B-tree memo. the fan-out case improved sharply, but independent 32-pair controls confirmed about 0.5–0.8% extra time when every recipe had a different one-file input. v2 used hash lookup, reserved the single-file canonical-line buffer from its format bounds, and built the frozen-input map without a second memo. its glibc 1,000-distinct-input control still slowed by about 1%, with roughly 260 more minor faults and higher system time. an independent repeat reproduced that cost. v3 reserved the complete recipe pass before its per-step allocations and pre-sized the frozen-input map from the selected iterator's bound. the last control pass still slowed, so no version stayed.

v1 used the earlier 27811da3 build; v2 and v3 include c3438f1c's CPU changes and the kept rename index. each version has one release executable per libc with `FETCHLOOM_EXP_RECIPE_CONTENT` read once, not separately compiled arms. the paired wall, noise and bootstrap method follows [the rename comparison](#indexing-pin-membership-and-renames-2026-11-12). v2 used at least 64 clean pairs for one and ten recipes, 32 for protected controls, and 10 for large fan-out gains. native RSS was measured outside the wall lock with GNU time: 32 pairs at small n, five through 1,000 and three above it, with elapsed time retained separately because allocator purging depends on time.

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

## a shorter search grace, 2026-11-13

kept. a search of every site, as `add iris` makes, gives the sites still searching 3 s once one has answered, down from 10 s; sites named with `--source` keep 10 s, and one named alone is waited for. ENA's search through EBI is slow on words it has not seen of late: of 16 searches of new words with the 10 s grace, 13 waited the whole 10 s for it after the six other sites had answered.

each site's answer, from the `-vv` answer lines of 24 default searches of new words, in seconds from the start: hugging face 0.08–0.15, kaggle 0.14–0.36, GEO 0.09–0.66, openneuro 0.10–1.00, zenodo 0.83–3.08 (median 1.45), dandi 0.44–4.53 (median 2.17), and ENA in 3 of the 24, at 0.57–0.77. figshare, which fetchloom does not search, answered its own search API in 0.54–0.58 s to curl. with words searched before, `iris` and `brain`, every site answered within 1.8 s in 7 of 8 searches, and ENA alone in 0.55–3.5 s.

the a/b ran one glibc release build per arm, 585a2a7b with the 10 s grace against the change: 13 rounds, the first a warmup, each with two new search words, one per arm in a shuffled order, every run under the measurement lock on cores 12–23. `bench/ab.py` with a 10% threshold:

| metric | ratio | 95% interval | wins |
|---|---|---|---|
| wall | 0.307 | [0.306, 0.420] | 11/12 |
| peak memory | 1.002 | [0.972, 1.033] | 6/12 |

the change took 3.08–3.11 s in every round, and the 10 s grace 2.68–10.10 s, 10.08 s at the median. the cost is coverage: the change left out ENA in 11 rounds, dandi in 7 and openneuro in 1, each with the warning that names `--source`, where the 10 s grace left out ENA in 8. dandi answered in 2–4.5 s in these later runs, against 0.4–1.2 s in the first ones that night.

raw evidence: `fix-a-search-grace/` beside the lab notes, with `runs.jsonl` and the per-site timings. these are live sites from one machine on one night, not a claim about every network.

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

the cost is page faults: with 4 KiB pages the same memory faults far more often. `perf stat` over 10 runs of the shipped binary, huge pages on and then off by the variable: the no-op over 1,000 datasets 204 to 1,595 faults and 18.1 to 21.6 ms of cpu, the status of 100,000 files 707 to 15,564 faults and 673 to 735 ms. the two switches behave alike here; the build's small extra on the shortest commands is its own binary, another commit's layout. on a system set to `always`, the feature would leave the kernel's huge pages on where the variable turns them off. huge pages stay on. turning them off would not bring the two musl peaks above [the 150 MB budget](budgets.md) under it either: placing 100,001 files, 203.6 to 169.3 MiB, and the sync of 40,000 datasets, 209.0 to 183.5 MiB.

## the pinnacle pass, 2026-11-13

a pass that measured the busiest commands whole, cores and disk and network sampled over time beside fetchloom's own trace, and chased each stretch where work waited on a resource that sat idle. each change ran behind a run-time switch, `FETCHLOOM_EXP_<name>`, read once, in a musl release build (x86_64-unknown-linux-musl, as ci builds it) of 8580a81e, or for the block-wise state read of the two commits before it, so an arm and its base were always one binary.

setup: every arm, a/a included, once a round in a seeded shuffled order after a warmup round, under the measure lock on cores 12–23, on tmpfs unless a row says ext4 (over nvme). the a/a arm runs the base arm's binary and switches again in the same rounds, so its interval is the spread the a/b has to clear. a ratio is the median of paired ratios, new over old, with its 95% bootstrap interval from `bench/ab.py`; peak memory is gnu time's `%M` of the child, and bytes written its `%O`. the downloads came from a local server, a small std-only rust program writing each file from memory, with no pacing: one stream carries 2.1 GB/s, four carry 6 GB/s, and it answers over 17,000 requests a second, so the client sets the pace. the small-file fixtures hold 10,000 or 100,000 files of 4.6 KB on average, 46 MB and 462 MB. measured beside 4 busy training processes, about one core each, which the scheduler moved across all 24 cores; the rounds' other load, read from `/proc/stat` around each run, was about 1 to 2 cores.

### a cold sync's questions to the cache

before downloading, a cold sync asked the cache about each pinned file four times, one after another: whether it held the bytes at another size, which would mean data.lock is wrong, three `lstat`s; then whether it held them at the pinned size, two `lstat`s, once for the files lacking and again for the space check and for the prompt's total. at 10,000 files that was about 90,000 system calls on one thread before the first download could start. now one pass on the workers asks once per file, both questions from one look, and the answers are kept for the rest of the step.

| case | pairs | wall | a/a wall | cpu | peak memory |
|---|---:|---|---|---|---|
| cold sync, 10,000 small files | 12 | 0.877 [0.868, 0.905] | 1.005 [0.964, 1.036] | 0.991 | 1.005 [0.984, 1.034] |
| cold sync, 100,000 small files | 8 | 0.876 [0.841, 0.883] | 0.990 [0.956, 1.009] | 1.002 | 1.020 [0.986, 1.139] |
| cold sync, 10,000 small files, ext4 | 10 | 0.947 [0.934, 0.971] | 1.021 [1.003, 1.040] | 0.986 | 0.990 [0.974, 1.086] |
| `add` of a 10,000-file listing | 12 | 0.986 [0.963, 1.012] | 0.985 [0.973, 1.025] | 0.996 | 0.983 |
| no-op sync of 10,000 files | 12 | 0.999 [0.994, 1.029] | 0.997 [0.983, 1.021] | 1.000 | 0.996 |
| a zip of 10,000 files | 12 | 1.015 [0.993, 1.088] | 1.019 [0.958, 1.059] | 1.000 | 1.016 |

kept: the 100,000-file interval clears the 10% bar and the a/a spread, and the 10,000-file median does too. the zip's wide interval is its own a/a's. `add` asks the cache through another path, which this does not change.

### reading the cache's folders on workers

every command that walks the cache, `cache verify`, the cache report, gc and `cache gc --inputs`, read its 256 fan folders one after another, about 3.6 µs an object, all of it one thread's system calls: 0.36 s of a 0.47 s verify of 100,000 objects on tmpfs. now up to 8 workers read a fan folder each, one per 32 folders the cache has, and the objects come back in the order one thread would give. a first build had each worker make its objects' paths, and peak memory nearly doubled: mimalloc keeps memory a thread frees for 1 s, and with `MIMALLOC_PURGE_DELAY=0` the extra was 4 MiB. the kept build's workers return only each object's digest, size, times and link count, the path of any name not in lowercase aside, and the paths are made after, on the calling thread, into a list sized once.

| case | pairs | wall | a/a wall | cpu | peak memory | peak memory, paths made on the workers |
|---|---:|---|---|---|---|---|
| `cache verify`, 100,000 objects | 12 | 0.371 [0.367, 0.379] | 0.990 [0.984, 1.007] | 1.019 | 0.966 [0.964, 0.968] | 1.975 [1.891, 1.983] |
| `cache verify`, 10,000 objects | 12 | 0.471 [0.467, 0.480] | 1.000 [0.994, 1.026] | 1.000 | 0.928 [0.924, 0.935] | 1.435 [1.401, 1.472] |
| `--json cache` beside 100,000 placed files | 12 | 0.644 [0.641, 0.647] | 1.000 [0.991, 1.005] | 1.005 | 0.960 [0.959, 0.961] | 1.165 [1.163, 1.169] |

the base medians were 0.473 s and 55.5 MiB, 0.051 s and 25.5 MiB, 0.813 s and 146.9 MiB. the first build gave `cache verify` of 1,000 objects 0.763 [0.749, 0.775]; a cache of fewer than 64 fan folders is still read on one thread. kept, and `FETCHLOOM_THREADS` caps the workers as it does elsewhere.

### dropping unpacked archives

once a sync has placed an archive's files, it drops the archive's own bytes unless some registered project still needs them, and it asks about every archive its lock pins. the question read every registered project's data.toml, lock and whole state, and checked that the cache held every member of every unpacked archive of every project, two `lstat`s each, one after another. with 20 projects sharing a zip of 10,000 files, a sync placing them from the cache took 2.1 s, nearly all of it this question: 400,000 system calls and 20 whole states. the drop needs to know only about its candidates: an archive whose files are not among them keeps its bytes whatever its members say. so only a candidate archive's members are checked now, each digest once however many projects pin it, on the workers; and the state is read only for its kept rebuilds, which its index places after the records.

| case | pairs | wall | a/a wall | cpu | peak memory |
|---|---:|---|---|---|---|
| sync of a zip of 10,000 files 20 projects share | 12 | 0.086 [0.085, 0.086] | 1.004 [0.991, 1.030] | 0.190 | 1.051 [0.927, 1.061] |
| sync of a zip of 10,000 files | 12 | 0.802 [0.781, 0.817] | 0.997 [0.972, 1.025] | 1.024 | 1.042 [1.007, 1.096] |
| sync of a tar.gz of 10,000 files | 12 | 0.822 [0.809, 0.834] | 0.997 [0.977, 1.014] | 1.031 | 1.025 [0.975, 1.092] |
| `--json cache` beside 100,000 placed files | 12 | 0.834 [0.830, 0.839] | 1.000 [0.992, 1.013] | 0.867 | 1.000 [0.999, 1.000] |

the base medians were 2.094 s and 102.5 MiB, 0.297 s and 132.8 MiB, 0.356 s and 125.1 MiB, 0.811 s and 146.9 MiB; the cache report's row comes from an earlier build of the same switch. the members' checks on the workers cost the memory: checked on one thread the three syncs gave 0.115, 1.015 and 1.019 at 0.98 to 1.01 of the peak, on two workers 0.098, 0.876 and 0.898 at 0.99 to 1.01. kept on the run's workers, `FETCHLOOM_THREADS` capping them. the zip's peak is the cost: 1.042 here, and 1.085 [1.049, 1.106] in 16 pairs of [the whole branch](#the-whole-branch) beside an a/a arm at 1.034, so 5 to 11 MiB at a peak of 129 to 133 MiB, inside the default's budget; two workers would keep it flat for 0.876 on the zip.

### reading the whole state

a status or a settled sync reads the project's whole state, about 247 bytes a record: at 100,000 files 24.7 MB read into one buffer and parsed into a map one record at a time, some 0.13 s of a 0.40 s no-op. the state already holds an index of its blocks of about 64 KiB for [reads within folders](#an-indexed-project-state-2026-11-12). a whole read now goes through it too: one block at a time into one reused buffer, each record checked against the index and pushed onto one list in path order, and the map built from the sorted list at once. a state whose index does not fit is read whole, as before. a first build that read each block into a map of its own and merged them was 7 to 11% slower than the base.

| case | pairs | wall | a/a wall | cpu | peak memory |
|---|---:|---|---|---|---|
| settled sync, 100,000 files | 12 | 0.839 [0.817, 0.849] | 0.997 [0.983, 1.008] | 0.889 | 0.834 [0.779, 0.838] |
| `--json status`, 100,000 files | 12 | 0.838 [0.827, 0.852] | 1.001 [0.982, 1.024] | 0.898 | 0.904 [0.897, 0.916] |
| `--json status --verify`, 100,000 files | 12 | 0.882 [0.876, 0.892] | 1.001 [0.995, 1.014] | 0.965 | 0.970 [0.968, 0.974] |
| settled sync, 10,000 files | 12 | 0.899 [0.882, 0.903] | 1.009 [0.996, 1.013] | 1.000 | 1.054 [1.041, 1.061] |
| `--json status`, 10,000 files | 12 | 0.894 [0.890, 0.898] | 0.999 [0.988, 1.010] | 0.926 | 0.953 [0.949, 0.959] |

the base medians were 0.400 s and 99.1 MiB, 0.399 s and 94.9 MiB, 0.547 s and 133.3 MiB, 0.041 s and 37.1 MiB, 0.048 s and 40.6 MiB. kept: every interval at 100,000 files clears the bar and the a/a spread, and so do the 10,000-file status and the no-op's median; the no-op's peak at 10,000 files is 2 MiB more.

the release recheck later read a `drop --dry-run` across 1,000 datasets at 1.051 [1.046, 1.055] of the peak, 40.0 to 42.1 MiB. it comes from this read, not from workers: its 476 KB state goes through the index, and the list of records, some 0.2 MiB, lands in a 2 MiB huge page of its own. with `MIMALLOC_ALLOW_THP=0` the two builds peak at 21.8 and 22.0 MiB, five runs each. it does not grow with the state: the list takes about 110 bytes a record where the whole read held about 247, so at 100,000 files the same `status` peaks 19.5 MiB lower without huge pages, 83.5 against 64.0 MiB, and 6 to 18 MiB lower with them.

### the state beside the folder walk

a status or a settled sync reads the state and then walks the data folder, though the walk needs nothing from the state. reading the state on a thread of its own while the walk runs, and for status data.lock on another, cut the time at 100,000 files by a further fifth, and by two fifths with data.lock. but the reading thread's allocations land in memory of its own, which mimalloc backs with 2 MiB huge pages on musl, and at 10,000 files that cost 16 to 20 MiB: a no-op's peak was 38 MiB without the thread and 46 to 63 MiB with it, and 17.5 against 18.7 MiB with `MIMALLOC_ALLOW_THP=0`. over the block-wise read, the base here:

| case | pairs | state beside the walk | peak memory | and data.lock beside both | peak memory |
|---|---:|---|---|---|---|
| settled sync, 100,000 files | 12 | 0.671 [0.656, 0.688] | 0.849 | | |
| `--json status`, 100,000 files | 12 | 0.669 [0.650, 0.681] | 0.914 | 0.501 [0.495, 0.509] | 1.039 |
| `--json status --verify`, 100,000 files | 12 | 0.765 [0.753, 0.772] | 0.932 | 0.638 [0.631, 0.642] | 0.983 |
| settled sync, 10,000 files | 12 | 0.757 [0.747, 0.773] | 1.546 [1.537, 1.550] | | |
| `--json status`, 10,000 files | 12 | 0.788 [0.782, 0.796] | 1.445 [1.437, 1.449] | 0.658 [0.651, 0.663] | 1.343 |

the ratios are against the build before the block-wise read, whose own row above gives 0.839 and 0.899 for the two syncs; its a/a arm sat at 0.997 to 1.009. dropped for the memory at 10,000 files. reading beside the walk only for a state large enough that 20 MiB is a small part of the peak would keep the 100,000-file win; it was not built.

### a download's slot

a cold sync of 10,000 small files from the local server kept each connection idle 85% of the time, a 208 µs gap between one response's end and the next request on average. two things sat in that gap: a download held its request slot until the task writing its bytes had finished, though the body was already read whole, and a download's first attempt read its partial record twice. freeing the slot once the body is read, and reusing the first read of the record, together:

| case | pairs | both | slot alone | record alone | a/a |
|---|---:|---|---|---|---|
| cold sync, 10,000 small files | 12 | 0.892 [0.878, 0.901] | 0.914 [0.905, 0.927] | 0.961 [0.956, 0.989] | 0.993 [0.979, 1.010] |
| `add` of a 10,000-file listing | 12 | 0.919 [0.899, 0.930] | 0.934 [0.908, 0.936] | 0.970 [0.953, 0.986] | 1.007 [0.983, 1.022] |
| 512 MiB `get` | 12 | 1.000 [0.998, 1.002] | 0.998 | 0.998 | 0.997 [0.995, 1.000] |
| cold sync, over the ask-once change | 12 | 0.872 [0.819, 0.913] | 0.908 [0.861, 0.943] | | 0.976 [0.946, 1.033] |
| `add`, over the ask-once change | 12 | 0.922 [0.886, 0.945] | 0.935 [0.889, 0.971] | | 0.998 [0.955, 1.016] |
| cold sync on ext4, over the ask-once change | 10 | 1.002 [0.954, 1.091] | 0.964 [0.917, 1.183] | | 0.996 [0.942, 1.052] |

dropped: no interval clears the 10% bar, and on ext4, where writing the files sets the pace, nothing moved. a server far enough away to matter makes the client's gap a small part of each request: with the local server paced, this pass found large files keeping the link 99.7% full and small ones keeping 3.8 of the 4 requests to one host in flight.

### what `add` writes

`add` of a listing of 100,000 small files wrote 1,083 MB to ext4 for 462 MB of data, where a cold sync of the same files wrote 674 MB, the data on 4 KiB blocks plus the state. the 409.6 MB between them is 100,000 times 4,096 bytes: the URL record `add` keeps for each link it learns, about 380 bytes of json that take a block of their own. nothing needs them for safety: a missing or broken one only means a download. but they are the only place the ETag a link served is kept, which data.lock does not hold, and [`lock --upgrade`](#upgrading-a-bucket-prefix-2026-11-12) and another project's `add` of the same listing skip an unchanged object by it. ext4, 6 pairs:

| case | arm | bytes written | wall | cpu |
|---|---|---|---|---|
| `add`, 100,000 files | no URL records | 0.622, 1,083.4 to 673.8 MB | 0.849 [0.806, 0.890] | 0.859 |
| `add`, 100,000 files | records written in place, not through `tmp/` | 1.000 | 1.006 [0.954, 1.102] | 0.956 |
| `add`, 100,000 files | a/a | 1.000 | 1.013 [0.962, 1.179] | 1.008 |
| `add`, 10,000 files | no URL records | 0.627, 109.7 to 68.8 MB | 0.813 [0.783, 0.945] | 0.866 |
| `add`, 10,000 files | records written in place | 1.000 | 1.090 [0.953, 1.313] | 0.943 |
| `add`, 10,000 files | a/a | 1.000 | 1.028 [0.903, 1.144] | 0.997 |

not changed: leaving the records out would make each upgrade of that listing download its 462 MB again, and writing them in place saved no bytes. a store holding a listing's records in one file would keep both; it was not built.

### more readers for a cold disk

checking files reads them on at most 8 workers. the nvme here reads small files faster the more reads are in flight, about 73,000 a second from 8 threads, 103,000 from 12, 173,000 from 24 and 304,000 from 96, so a cold check waits on the disk with most of its depth unused. more readers than workers, up to the number below and one per 16 files, on ext4 after dropping the files from the page cache, 8 pairs, 10 for 32 readers:

| readers | `cache verify`, 100,000 | peak memory | `status --verify`, 100,000 | peak memory |
|---:|---|---|---|---|
| 12 | 1.001 [0.999, 1.144] | 0.998 | 0.815 [0.723, 0.842] | 1.250 |
| 16 | 0.856 [0.847, 0.892] | 1.066 | 0.730 [0.696, 0.959] | 1.356 |
| 24 | 0.704 [0.702, 0.707] | 1.175 | 0.613 [0.581, 0.628] | 1.707 |
| 32 | 0.631 [0.629, 0.644] | 1.315 | 0.534 [0.530, 0.546] | 1.665 |

the a/a arms sat at 0.997 to 1.003; the base medians were 1.387 s and 57.6 MiB, 2.378 s and 132.6 MiB. with the files in the page cache, 32 readers gave 0.991 and 1.023 for `cache verify` of 100,000 and 10,000 objects and 0.955 for `status --verify`, at 1.31 to 1.68 of the peak. dropped for the memory, which every run would pay for a win only a cold disk sees. a reader that asks the kernel to read ahead, rather than a thread per read in flight, would not cost a thread's memory; it was not built.

### push's spinning

a first push of 100,000 small files to a folder mirror on ext4 took 7.0 to 7.4 s of wall time and 32.7 s of cpu, 30.2 s of it in the kernel. perf put that time in `osq_lock` and `rwsem_spin_on_owner` on `tmp/`'s directory lock, which each new file takes, and `mutex_spin_on_owner` on the lock each rename from `tmp/` into `objects/` takes: the kernel's waiters spin while the lock's owner runs, rather than sleep. there is no retry loop or busy wait in fetchloom. every way to make the waiters sleep cost wall time, since the spinning is what hands each lock on without a wakeup:

| arm | ext4, 10,000: wall, cpu | ext4, 100,000: wall, cpu | tmpfs, 10,000: wall, cpu | tmpfs, 100,000: wall, cpu |
|---|---|---|---|---|
| a lock in fetchloom around each make and rename | 1.555, 0.583 | 1.575, 0.515 | 2.645, 0.907 | 2.608, 0.872 |
| one thread makes the files | 1.126, 0.586 | 1.086, 0.584 | 1.277, 0.972 | 1.305, 0.966 |
| one thread renames them | 1.071, 0.958 | 1.083, 0.856 | 1.542, 0.898 | 1.554, 0.880 |
| both | 1.148, 0.539 | 1.200, 0.469 | 1.778, 0.850 | 1.854, 0.833 |
| 4 workers, by `FETCHLOOM_THREADS` | 1.217, 0.604 | 1.168, 0.552 | 1.241, 0.729 | 1.186, 0.654 |
| 2 workers | 1.782, 0.448 | 1.738, 0.418 | 1.903, 0.636 | 1.943, 0.620 |

8 to 12 pairs a cell; the a/a arms sat at 0.988 to 1.011 in wall time. dropped: none holds the wall time. the conditional staging shards [measured before](#conditional-mirror-staging-shards-2026-11-12) spread the made files over 16 folders and were faster at 100,000 files, 0.732, but cost peak memory; their cpu was not recorded.

### the whole branch

the four kept changes together: a musl release build of the pass's last commit against one of 8580a81e, each arm its own binary, with a byte-identical copy of 8580a81e's at another path as the a/a arm, in the same rounds. tmpfs ran 8 pairs while the merge gate built and tested on cores 0–11, and the second tmpfs batch 16 pairs on a quiet half; ext4 ran 6.

| case | pairs | wall | a/a wall | cpu | peak memory | a/a peak memory |
|---|---:|---|---|---|---|---|
| cold sync, 10,000 small files | 16 | 0.881 [0.864, 0.888] | 1.008 [0.983, 1.020] | 0.997 | 1.011 | 1.006 |
| cold sync, 100,000 small files | 8 | 0.906 [0.854, 0.919] | 0.990 [0.978, 1.050] | 0.996 | 0.973 | 1.001 |
| cold sync, a few large files | 16 | 0.985 [0.928, 1.006] | 0.996 [0.938, 1.006] | 1.001 | 1.023 [0.903, 1.075] | 0.987 |
| sync of a zip of 10,000 files | 16 | 0.804 [0.775, 0.815] | 1.002 [0.983, 1.021] | 1.037 | 1.085 [1.049, 1.106] | 1.034 |
| sync of a tar.gz of 10,000 files | 16 | 0.830 [0.813, 0.840] | 0.998 [0.984, 1.019] | 1.026 | 1.038 [1.020, 1.057] | 0.968 |
| sync of a zip 20 projects share | 8 | 0.086 [0.085, 0.088] | 1.002 [0.991, 1.010] | 0.188 | 1.065 [1.022, 1.149] | 1.075 |
| `add` of a 10,000-file listing | 8 | 0.977 [0.948, 1.007] | 0.995 [0.925, 1.056] | 0.989 | 0.984 | 1.019 |
| 512 MiB `get` | 8 | 1.000 [0.998, 1.002] | 0.998 [0.997, 1.003] | 1.000 | 0.985 | 1.028 |
| settled sync, 10,000 files | 8 | 0.888 [0.874, 0.906] | 0.994 [0.979, 1.010] | 1.000 | 1.056 [1.010, 1.065] | 1.004 |
| settled sync, 100,000 files | 8 | 0.823 [0.806, 0.835] | 1.003 [0.992, 1.040] | 0.887 | 0.842 | 0.999 |
| `--json status`, 10,000 files | 8 | 0.912 [0.903, 0.927] | 1.011 [0.995, 1.023] | 1.000 | 1.002 | 1.000 |
| `--json status`, 100,000 files | 8 | 0.839 [0.789, 0.873] | 1.007 [0.932, 1.029] | 0.886 | 0.849 | 1.002 |
| `--json status --verify`, 100,000 files | 8 | 0.895 [0.882, 0.918] | 1.006 [0.986, 1.066] | 0.965 | 0.970 | 0.999 |
| `cache verify`, 10,000 objects | 8 | 0.469 [0.460, 0.484] | 0.992 [0.976, 1.005] | 1.000 | 0.921 | 0.999 |
| `cache verify`, 100,000 objects | 8 | 0.377 [0.372, 0.397] | 1.020 [0.998, 1.027] | 1.013 | 0.967 | 0.999 |
| `--json cache` beside 100,000 files | 8 | 0.399 [0.395, 0.403] | 0.998 [0.977, 1.005] | 0.815 | 0.878 | 1.000 |
| push of 10,000 files | 8 | 0.972 [0.959, 0.981] | 0.996 [0.984, 1.002] | 0.995 | 0.999 | 1.000 |
| push of 100,000 files | 8 | 0.970 [0.964, 0.974] | 1.000 [0.981, 1.015] | 0.998 | 0.989 | 1.004 |
| `path` beside 100,000 files | 8 | 1.000 [0.949, 1.120] | 1.007 [0.859, 1.124] | | 1.005 | 1.001 |
| ext4: cold sync, 10,000 small files | 6 | 0.945 [0.908, 1.035] | 1.011 [0.960, 1.031] | 0.990 | 1.077 [0.988, 1.200] | 1.027 |
| ext4: `add` of a 10,000-file listing | 6 | 0.981 [0.965, 1.063] | 0.998 [0.983, 1.019] | 0.999 | 1.031 | 1.003 |
| ext4: push of 10,000 files | 6 | 0.977 [0.950, 1.052] | 0.995 [0.955, 1.006] | 0.985 | 0.980 | 1.001 |
| ext4: settled sync, 100,000 files | 6 | 0.819 [0.729, 0.853] | 1.018 [0.989, 1.300] | 0.883 | 0.857 | 1.018 |
| ext4: `--json status`, 100,000 files | 6 | 0.835 [0.824, 1.065] | 1.001 [0.972, 1.030] | 0.891 | 0.854 | 1.000 |
| ext4, cold: `cache verify`, 100,000 objects | 6 | 0.793 [0.783, 0.912] | 1.003 [0.996, 1.179] | 1.007 | 0.926 | 1.005 |
| ext4, cold: `status --verify`, 100,000 files | 6 | 0.973 [0.968, 1.025] | 1.002 [0.999, 1.007] | 0.975 | 0.970 | 1.003 |

the base medians ran from 5.3 ms for `path` to 8.2 s for the cold sync of 100,000 files, and 12.9 to 232.7 MiB. nothing is slower than its a/a arm's spread. the peaks that rose are the archive syncs', from the drop's members checked on the workers, and the 10,000-file no-op's 2 MiB from the block-wise read; the first tmpfs batch, beside the gate, read the zip's peak at 1.118 [1.025, 1.187] and a few large files' at 1.134 [0.938, 1.198], which 16 quieter pairs put at 1.085 and 1.023, the latter inside its a/a arm's spread.

### reading a small state

the release recheck found the [block-wise read](#reading-the-whole-state) slowing a settled sync of 3 files by 0.9% on both libcs, 1.009 [1.001, 1.023] on musl and 1.009 [1.003, 1.017] on glibc against 6ace5b2d, 100 pairs each. strace counted 436 system calls against 428 over six runs of each build: on the 966-byte state, 3 seeks and 2 reads more, as its header, tail and one block were read apart where the whole read took the file at once; the other 3, a futex, a signal mask and an munmap, varied between runs of either build, futexes from 32 to 41 and munmaps from 8 to 9. a state of at most one block, 64 KiB, is now read whole in one read of the length its open file reports, so it takes no `fstat` and `lseek` for a size hint and no read at its end either: 1 call where the pass's first read took 4 and the block-wise read 9.

against 5e18256c, each arm a binary of its own, 300 pairs interleaved in a seeded order under the measure lock on cores 12–23, on the recheck's tiny fixture:

| libc | the fix | 6ace5b2d, before the pass | a/a |
|---|---|---|---|
| musl | 0.996 [0.988, 1.003] | 0.993 [0.985, 0.999] | 1.002 [0.994, 1.009] |
| glibc | 0.976 [0.972, 0.980] | 0.996 [0.994, 1.001] | 1.002 [0.999, 1.006] |

`bench/recheck.py` with its own fixtures per arm, 100 pairs for the 3-file sync and 20 for the status and settled sync of 100,000 files, against 5e18256c: musl 1.008 [0.995, 1.016] beside an a/a arm at 1.010 [0.994, 1.031], status 1.013 beside 1.013, sync 1.005 beside 0.996; glibc 0.996 [0.988, 1.005] beside 0.995. kept: the sync is back to 6ace5b2d's time on musl and below it on glibc, and the large states, which still go through the index, are flat.

### URL records in shards

[what `add` writes](#what-add-writes) found each link's URL record taking a 4 KiB block and an inode for about 380 bytes of json: 409.6 MB of 4 KiB blocks for 100,000 links, and 4 GB and a million inodes for a million. the records now go as lines into 16 files in `urls/`, by the first digit of the SHA-256 of their link, each line its json after a check of it, the first 16 digits of its SHA-256, so a line a killed run cut short is passed over. beside each shard, an index file holds a 12-byte entry for each line: the first 8 bytes of the link's SHA-256 and where the line starts. a save appends both under a file lock on the shard, so two runs writing at once lose nothing. a run reads a shard's index on its first lookup there, then the lines it asks for, through a handle it keeps; a lookup that misses reads on through what other runs appended meanwhile. gc writes each shard again with the last record of each link it keeps. records in the old one-file form are not read: 0.1.0 is unreleased, and a missing record only costs a download.

reading a shard's index once costs a run time and memory in proportion to the store, where the files cost nothing until asked for, so each design was measured beside a store of a million records from other listings, then `add` of a listing of 10,000 and of 100,000 new links, `lock --upgrade` of each and a `get` of one of their links. two designs failed there:

- 256 shards, with an index of every line built by reading and checking each shard on its first lookup: `lock --upgrade` of 10,000 links was 4.302 [4.227, 4.411] times slower, peak memory 1.59, since 10,000 links touch every shard and so checked a million lines.
- 16 shards with an index of 16-byte entries, the line's length among them, sorted on the first read: `lock --upgrade` of 10,000 links 1.095 [1.088, 1.101] beside an a/a arm at 1.006 [0.996, 1.017], and 13 MiB more peak memory. sorting each shard's 62,500 entries on its first read took the time, user cpu 0.11 to 0.15 s, and the 16-byte entries the memory.

the kept index groups each shard's entries by the bits of their key after the shard's, some four to a group, in one counting pass and one placing pass that keep the file's order within a group, so a link's last entry is its latest; it holds each entry as 8 bytes, the key's last 4 and the line's start, and reads a line up to its newline, 1 KiB at a time. a lookup scans its group, then reads the line with one positional read.

setup: a musl release build of ea549486 with the switch, measured beside 4 busy training processes as above; each arm its own cache on ext4 over nvme. cold: a fresh cache each round, 8 pairs. beside the store: each arm's cache kept its store from round to round and took each round's new links, from 1.00 to 1.09 million records over the 10,000-link rounds, then to 1.59 million over the 100,000-link ones, 8 and 4 pairs; the page cache held the stores.

| case | wall | a/a wall | bytes written | cpu | peak memory |
|---|---|---|---|---|---|
| cold `add`, 100,000 links | 0.845 [0.790, 0.861] | 1.034 [0.923, 1.163] | 0.749, 1,083.4 to 811.3 MB | 0.906 | 1.019 [0.990, 1.081], 323 to 329 MiB |
| cold `add`, 10,000 links | 0.810 [0.629, 0.934] | 1.043 [0.724, 1.389] | 0.746, 109.7 to 81.9 MB | 0.907 | 0.994 [0.951, 1.071] |
| `add` of 10,000 links beside the store | 0.839 [0.788, 0.850] | 1.012 [0.974, 1.061] | 0.754 | 0.897 | 1.030 [0.987, 1.076], 178 to 183 MiB |
| `lock --upgrade` of them | 0.739 [0.721, 0.759] | 0.994 [0.973, 1.020] | | 0.735 | 1.095 [1.039, 1.131], 93 to 101 MiB |
| `get` of one of them | 0.747 [0.716, 0.786], 17.3 to 13.0 ms | 0.989 [0.969, 1.104] | | | 1.088, 21.0 to 22.9 MiB |
| `add` of 100,000 links beside the store | 0.826 [0.806, 0.887] | 1.000 [0.885, 1.072] | 0.750 | 0.893 | 1.065 [1.033, 1.136], 336 to 359 MiB |
| `lock --upgrade` of them | 0.729 [0.712, 0.773] | 0.995 [0.981, 1.007] | | 0.723 | 1.037 [1.013, 1.065], 263 to 272 MiB |
| `get` of one of them | 0.270 [0.254, 0.289], 51.1 to 13.6 ms | 1.005 [0.914, 1.113] | | | 1.094 |

after those rounds, the per-file store held 1,590,000 files taking 6.70 GB, the shards 32 files taking 0.60 GB. `cache gc --now` over a million records beside the 10,000-file cache, 2 runs each: when every record's file is held, 15.7 s per file and 4.4 to 4.6 s in shards, which find nothing to rewrite; when none is, 56.0 to 57.5 s per file, removing each, and 11.1 s in shards, removing them, 8.4 s of it system time, about what two failed lookups a record, its file and its unpacked tree, cost.

kept: bytes 0.75 of the per-file store's at every size, 32 files for any number of links, and every case faster. the memory is the index, 9 to 10 bytes a record, 8 for its entry and 1 to 2 for its group, in each shard a run reads, which a run touching 10,000 links beside a million reads whole. an `add` still writes 137 MB more than a cold sync of the same 100,000 files for 37 MB of records: each flush during the download writes the shards' last pages again.

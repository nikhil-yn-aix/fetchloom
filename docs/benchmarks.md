# benchmarks

this is the method for comparing fetchloom with other tools. it was fixed before any comparison ran: the tools, versions, commands, settings and timeouts sit in `bench/plans/`, committed before the runs they describe. changes are listed at the end with their reason. results live in `bench/results/`, one folder per block of runs, with every attempt kept, failures included.

## questions

1. how long does it take to get a pinned dataset onto a machine whose tools have never fetched it?
2. how long does a second project on the same machine take to get the same data, and how much disk do both use?
3. what does a tool cost the machine while it runs: cpu time and peak memory?
4. after a download is cut off halfway, by a killed process or a dropped connection, how much is downloaded again?
5. does the tool refuse bytes that differ from the ones it pinned: from the server, and in its own cache?
6. how do the answers change on slow, distant or lossy networks, and with many small requests?

## primary comparisons

declared now, so no comparison is chosen after seeing the results:

- the lab, one 1 GiB file and one 100 MiB file, verified: fetchloom against curl followed by a sha256 check, and against aria2c tuned.
- the lab, 1,000 files of 100 KiB as separate requests: fetchloom against curl `--parallel` and aria2c.
- the lab, an archive of 10,000 small files fetched and unpacked: fetchloom against curl followed by bsdtar, and pooch with its unzip step.
- live, each source: fetchloom against that source's own client, listed below.

everything else is reported as description, not as a claim.

## tools

each tool is installed at a pinned version, recorded with the results. each runs with its defaults and, where its documentation names a speed setting, once more with that setting; both are reported.

| tool | role | workloads |
|---|---|---|
| fetchloom | the subject | all |
| curl | one stream, transfer only | plain url |
| curl, then sha256 | one stream, verified: the fair floor for a pinned download | plain url |
| aria2c | split downloads: defaults, then `-x 16 -s 16 -k 1M` | plain url, many urls |
| pooch | python downloads with a known hash | plain url; doi for zenodo, figshare and dataverse |
| huggingface_hub (`hf download`) | the hub's own client, with xet | hugging face |
| zenodo_get | zenodo records | zenodo |
| datahugger | doi repositories | zenodo, figshare, dataverse, osf |
| kagglehub | kaggle's own client | kaggle |
| gdown | google drive files and folders | drive |
| dvc (`import-url`) | pinned urls in a data versioning tool | plain url |
| datalad (`download-url`) | git-annex datasets; linux only, since git-annex on windows is limited | plain url |

when a tool cannot do a workload, its cell is empty and the table says why. a time is compared with a floor only when both do the same work: transfer only, or transfer and verification.

## workloads

the lab: a local https server (http/1.1 and http/2, byte ranges) in WSL, pinned to two cpu threads, its outgoing traffic shaped by `bench/lab/net.sh`, for clients in WSL and on windows alike. only the client differs between runs. workloads:

- one 1 GiB file and one 100 MiB file
- 1,000 files of 100 KiB, each its own url. fetchloom gets them as a project of 1,000 url datasets, which is how it asks for many separate urls
- a zip and a tar.zst of 10,000 files of 4 KiB, fetched and unpacked
- a zip of 4 members of 128 MiB, fetched and unpacked

each profile is checked before use: the achieved bandwidth, round trip, loss and http version are measured and recorded. the delay sits on the server's side, so it adds the whole round trip there, and loss hits the data the server sends.

| profile | bandwidth | round trip added | loss |
|---|---|---|---|
| lan | not shaped | none | none |
| fast | 1 gbit | 5 ms | none |
| home | 100 mbit | 20 ms | none |
| far | 20 mbit | 100 ms | 1% |
| poor | 5 mbit | 250 ms, 20 ms jitter | 2% |

live: real datasets from every source fetchloom supports, in `bench/datasets.toml`, each at a fixed version: a zenodo record version, a figshare version, a hugging face commit, a kaggle version. before the first block, each dataset is downloaded once and hashed by the harness itself, which gives the expected sha256 of every file; a run is correct only when its files match. a dataset whose files change between blocks invalidates the blocks around the change. sizes run from under 10 MB to 2 GB, in three shapes: one file, 1,000 or more files, and an archive.

## machine

one laptop: an 11th gen intel i7-1185G7, 4 cores and 8 threads, 32 GB, nvme. windows 11 with defender's real-time protection on, as people have it. linux is ubuntu 24.04 in WSL 2 on the same laptop: a virtual machine with its own network address translation, so linux numbers are WSL numbers. the lab server in WSL shares the machine with the client under test. no other work runs during a block. before each live block, a 100 MB download from a CDN records the network's speed, and the time of day is noted.

## procedure

- first fetch: each tool gets a new, empty home for its caches and settings, and a new output folder. this clears only the tool's own state; the operating system's caches, DNS, TLS sessions and the servers' CDNs are not cleared, so "first fetch" is not "first on a new machine".
- second project: the same data into a new, empty output folder, keeping the tool's cache, using the workflow each tool documents for reusing its cache. a tool without a cache downloads again.
- order: each block runs every tool once, in a random order drawn from a seed recorded with the results.
- repetitions: fixed. 5 blocks in the lab. 3 blocks live, which supports description, not claims of speed.
- per run: wall time; cpu time of the whole process tree from the operating system's accounting (a windows job object, linux's `RUSAGE_CHILDREN`); peak memory (on windows the job's peak committed memory, on linux the largest sampled sum of the tree's resident memory); bytes the server sent for this run, from the lab server's log, or, live, the machine's received bytes; and the sha256 of every file the run produced.
- disk: bytes of distinct files after the first fetch and after the second project, counting a hardlinked file once. on linux, the peak during a run is also measured on an isolated loop volume; on windows that needs an administrator, so only final sizes are reported.
- cut off, lab only: the 1 GiB download is stopped when the server has sent half, once by killing the tool and once by dropping its connection at the server. the rerun's bytes from the server log give what was downloaded again.
- changed bytes, lab only, three cases: the server changes one byte before a first fetch whose sha256 is pinned; a byte of the tool's cached copy changes before the second project; the server changes a byte between two fetches of an unpinned url. each either fails with an error or does not.
- budget: the live blocks are chosen in advance so that every repetition fits in 25 GB counted at its worst. sources left out of the timed blocks are checked for correct results only. a ledger sums the bytes every live run receives and stops before the total would pass 25 GB.

## reading the results

- a failed run is reported as a failure, with the cell's completion rate. times come from correct runs; the share of runs that failed is printed beside them.
- each cell reports the median and the range of its runs.
- in the lab, each block pairs fetchloom with every other tool run in the same block. fetchloom is called faster or slower only for a primary comparison where it wins or loses in all 5 blocks and the ranges do not overlap.
- live timings are description only.
- a ratio to a floor appears only when both do the same work.
- results that favour another tool are reported as plainly as those that favour fetchloom.

## threats to validity

- live servers and their CDNs change from minute to minute; live numbers hold for this place and these days only.
- every number comes from one laptop and one network.
- defender scans every new file on windows, for every tool; the cost depends on the program and the file.
- the lab's 1,000-url workload asks fetchloom for 1,000 datasets, which a person rarely does; the live many-file datasets show the common case.
- authentication, rate limits and expiring links appear only where live sources impose them; fetchloom's handling of them is covered by its own tests, not compared here.
- fetchloom's authors wrote this method. against that: it was fixed and reviewed before any run, the plans and raw data are public, and corrections from other tools' maintainers are welcome.
- the settings chosen for other tools come from their documentation; better ones may exist.

## changes

- before any run, an independent review asked for: fixed repetitions instead of extra runs after a wide spread, paired lab comparisons and live timings as description only, a verified floor for pinned downloads, a many-request workload, separate tests for cut connections and changed bytes, a randomized order, the operating system's own cpu and memory accounting, checked network profiles, frozen expected hashes for live data, a budget fixed in advance, and failures reported instead of dropped. all were taken except two. peak disk on windows needs an administrator, so only linux measures it. shaping sits on the server's side only: its delay adds the stated round trip and its loss hits the data, which is what decides a download's speed, while a second shaper for the other direction added a moving part without changing what is measured.

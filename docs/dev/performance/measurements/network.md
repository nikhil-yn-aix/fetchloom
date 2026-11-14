# Network

## a/b passes on the linux lab, 2026-11

each change below ran behind a switch in one build, against the same build with the switch off, in paired rounds shuffled with the [lab](../benchmark-design.md)'s other tools, five to twelve rounds a case. a ratio is the median of new over old, with its 95% bootstrap interval where one was kept; under 1 is faster or smaller. a change stayed only if it won where it aimed and made no lan case slower (an interval's lower end above 1.02); it then became the only code and its switch went. a change that lost went with its code.

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
| transparent huge pages off for the process | 24cd618 | peak memory 0.33 to 0.73 on every lan case, as ext4 s2 0.327 [0.311, 0.331]; wall flat there, but slower for a no-op sync and for placing from the cache, [measured alone](process.md#ab-passes-on-placement-and-startup-2026-11) | dropped |
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

## upgrading a bucket prefix, 2026-11-12

`lock --upgrade` of a dataset pinning five tar shards of 2,037,760 bytes from an s3-compatible stand-in on this machine, after a sixth appeared there. the stand-in states each object's size and an ETag in its listing and ignores conditional requests, as wf-audio's did; such a service's ETag is no MD5. the counts are the upgrade's alone, from `--json` work counters; one run each, since they do not vary.

| build | stand-in | requests | bytes downloaded | objects written |
|---|---|---:|---:|---:|
| c9da2d6 | ignores conditional requests | 7 | 12,228,135 | 126 |
| reuse by listed ETag and size | ignores conditional requests | 2 | 2,039,335 | 121 |
| c9da2d6 | answers a matching `If-None-Match` with 304 | 7 | 2,039,335 | 121 |
| reuse by listed ETag and size | answers a matching `If-None-Match` with 304 | 2 | 2,039,335 | 121 |

the two requests are the listing and the new shard. the stand-in that answers 304 sent no bytes for the five old shards before the change either, but took a request for each.

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

growing from the default's numbers loses max's win where it is the point: a debug log of one run showed the first doubling, measured on a round that still held requests begun before it, bringing data in only 1.34 times faster, and other runs went back a step there, which the wide interval shows. starting at max and narrowing keeps the far case, but the first round has already run 16 downloads at once, so the local case keeps about half of max's extra memory, 10% on glibc and 5% on musl, neither interval clearing a 10% bar for the code it adds. both were dropped; max stays a fixed 32 and 16, which [resources](../../../user/guides/speed-and-memory.md#which-to-pick) already tells people to pick for far servers. a level that knew a server was near before its first round could do better; nothing read today says so.

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

a status or a settled sync reads the project's whole state, about 247 bytes a record: at 100,000 files 24.7 MB read into one buffer and parsed into a map one record at a time, some 0.13 s of a 0.40 s no-op. the state already holds an index of its blocks of about 64 KiB for [reads within folders](project-state.md#an-indexed-project-state-2026-11-12). a whole read now goes through it too: one block at a time into one reused buffer, each record checked against the index and pushed onto one list in path order, and the map built from the sorted list at once. a state whose index does not fit is read whole, as before. a first build that read each block into a map of its own and merged them was 7 to 11% slower than the base.

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

8 to 12 pairs a cell; the a/a arms sat at 0.988 to 1.011 in wall time. dropped: none holds the wall time. the conditional staging shards [measured before](placement.md#conditional-mirror-staging-shards-2026-11-12) spread the made files over 16 folders and were faster at 100,000 files, 0.732, but cost peak memory; their cpu was not recorded.

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

### folders below a mirror's `tmp/`

[push's spinning](#pushs-spinning) found a push of 100,000 small files to a folder mirror on ext4 spending 30.2 of its 32.7 s of cpu in the kernel, its workers spinning on the lock of `tmp/`, where each made its file. spreading those files over 16 folders below `tmp/` was tried twice before and dropped, for 50-file pushes 1.046 and for peak memory 1.042, its cpu never recorded. measured on cpu, as asked this time, with each item's temporary file in `tmp/<the first digit of its hash>/`, the 16 folders checked by `prepare` as the others are, and the sweep looking in them without following a link:

| push | filesystem | pairs | cpu | wall | a/a wall | peak memory |
|---|---|---:|---|---|---|---|
| 100,000 files | ext4 | 8 | 0.543 [0.524, 0.558] | 0.743 [0.708, 0.779] | 1.046 [0.977, 1.112] | 0.999 [0.967, 1.038] |
| 10,000 | ext4 | 8 | 0.526 | 0.770 [0.763, 0.778] | 0.999 [0.994, 1.010] | 0.997 |
| 1,000 | ext4 | 8 | 0.517 | 0.852 [0.797, 0.895] | 1.007 [0.961, 1.030] | 1.005 |
| 500 | ext4 | 12 | 0.615 | 0.868 [0.843, 0.897] | 1.003 [0.943, 1.013] | 1.005 |
| 100 | ext4 | 12 | | 1.027 [1.017, 1.044] | 1.020 [1.000, 1.035] | 1.000 |
| 50 | ext4 | 12 | | 1.031 [1.020, 1.037] | 0.993 [0.985, 1.002] | 1.002 |
| 10 | ext4 | 12 | | 1.074 [1.060, 1.086] | 1.007 [1.001, 1.013] | 0.998 |
| 100,000 files | tmpfs | 12 | 0.869 [0.867, 0.878] | 0.905 [0.900, 0.909] | 0.995 [0.994, 1.007] | 1.038 [1.011, 1.068] |
| 10,000 | tmpfs | 10 | 0.871 | 0.911 [0.895, 0.923] | 1.001 [0.990, 1.011] | 1.001 |
| 1,000 | tmpfs | 10 | 0.870 | 0.963 [0.942, 0.981] | 1.009 [0.975, 1.031] | 1.001 |
| 500 | tmpfs | 12 | 0.833 | 0.974 [0.967, 0.986] | 0.994 [0.985, 1.001] | 1.000 |
| 100 | tmpfs | 12 | | 1.024 [1.003, 1.044] | 1.001 [0.983, 1.018] | 0.993 |
| 50 | tmpfs | 12 | | 1.008 [0.971, 1.053] | 0.968 [0.945, 1.008] | 0.999 |
| 10 | tmpfs | 12 | | 1.043 [1.015, 1.073] | 1.001 [0.961, 1.032] | 1.000 |

the small pushes' cpu is below the 10 ms the kernel counts in. making the 16 folders costs a push of 100 files or fewer 2 to 7%, so a push of fewer than 500 files, `SHARD_FROM`, keeps the one folder and is unchanged; 500 was the smallest size measured to gain on both filesystems. the memory on tmpfs is mimalloc's huge pages: with `MIMALLOC_ALLOW_THP=0` in both arms, 8 pairs, the folders' peak memory was 0.974 of the one folder's, and with them 1.030 [0.981, 1.070]; the arms without huge pages took 0.857 and 0.835 of the base's peak.

setup: musl release builds of ea549486 and 7b2248cb with the switch on, measured beside 4 busy training processes as above, a fresh mirror each run in the filesystem's run folder. made plain with the 500-file threshold, against its parent, 6 pairs each: ext4 100,000 files 0.755 [0.741, 0.763], cpu 0.535; 500 files 0.877, cpu 0.615; 10 files 1.002 beside an a/a arm at 1.006; tmpfs 100,000 files 0.917, cpu 0.878, and 10 files 1.029 beside an a/a arm at 1.066. kept: on ext4, where the spinning was found, cpu 0.52 to 0.62 with every wall time faster, past the bar of cpu 0.80 at wall 1.02; on tmpfs cpu 0.83 to 0.87, short of 0.80, with wall 0.905 to 0.974.

### reading ahead of the workers

[more readers for a cold disk](#more-readers-for-a-cold-disk) found nvme reads of small files faster the more of them wait at once, but 32 reader threads cost up to 1.71 of the peak memory. the kernel can hold the waiting reads instead: `posix_fadvise(WILLNEED)` on a file starts reading it and returns. each worker checking files, as it takes one, now asks for those up to 256 past it not asked for yet, through an open that follows no link and waits on no fifo, so no thread is added and nothing is read into the process. three versions led there:

- a reader thread ahead of the workers, waking as they took files: cold `status --verify` of 100,000 files 0.568 [0.474, 0.651] but `cache verify` 0.861 [0.692, 0.949] beside an a/a arm at 1.040 [0.815, 1.202], and with the files in memory `cache verify` 1.047 to 1.061; for push's read-back, which drops each file's pages first, 0.888 to 0.996 at cpu 1.16 to 1.29, so push reads back as before.
- the workers asking, always: cold 0.322 to 0.408 with a window of 256 and 0.405 to 0.457 with 64, but with the files in memory on tmpfs `cache verify` 1.431 to 1.440 and `status --verify` 1.101 to 1.126: an open and a hint for each file the check reads anyway. so a probe now reads one byte of up to 8 files spread through the list with `preadv2(RWF_NOWAIT)`, which fails with `EAGAIN` when the byte is not in memory, and the workers ask only when a quarter of them are out; tmpfs, which cannot say, counts as in memory.
- the probe with each path copied for its hint: cold `cache verify` of 10,000 objects at peak memory 1.088 [1.084, 1.097], 23.4 to 25.5 MiB, beside an a/a arm at 1.001. mimalloc's statistics put its peak commit 1.0 MiB higher, a page of another size for each worker, which its huge pages made 2 MiB; without them 0.406 against 0.399 of the base's peak with them. the hint now borrows the object's path, and the peak commit is the base's, 17.7 MiB.

the kept version, 12 pairs a case, a musl release build of 7b2248cb with the switch, measured beside 4 busy training processes as above; cold cases dropped each object's pages before each run:

| case | filesystem | wall | a/a wall | cpu | peak memory | a/a peak memory |
|---|---|---|---|---|---|---|
| `status --verify`, 100,000 files, cold | ext4 | 0.316 [0.304, 0.317] | 0.995 [0.951, 1.001] | 1.052 | 0.987 [0.972, 0.991] | 1.000 [0.993, 1.001] |
| `status --verify`, 10,000 files, cold | ext4 | 0.371 [0.367, 0.373] | 0.999 [0.994, 1.003] | 1.069 | 0.999 [0.997, 1.003] | 0.999 [0.968, 1.002] |
| `cache verify`, 100,000 objects, cold | ext4 | 0.351 [0.351, 0.354] | 0.999 [0.998, 1.004] | 1.090 | 1.007 [0.985, 1.011] | 0.998 [0.979, 1.000] |
| `cache verify`, 10,000 objects, cold | ext4 | 0.529 [0.412, 0.618] | 0.976 [0.914, 1.022] | 1.000 | 1.001 [0.996, 1.004] | 0.998 [0.994, 1.001] |
| `status --verify`, 100,000 files | tmpfs | 0.992 [0.989, 1.000] | 0.990 [0.967, 1.020] | 1.000 | 0.998 | 1.000 |
| `status --verify`, 10,000 files | tmpfs | 1.009 [0.999, 1.018] | 1.001 [0.994, 1.013] | 1.000 | 0.997 | 1.001 |
| `cache verify`, 100,000 objects | tmpfs | 1.003 [0.995, 1.009] | 1.005 [0.994, 1.009] | 1.006 | 1.000 | 1.001 |
| `cache verify`, 10,000 objects | tmpfs | 0.996 [0.972, 1.001] | 0.997 [0.983, 1.008] | 1.000 | 0.998 | 0.998 |
| `status --verify`, 100,000 files, in memory | ext4 | 0.994 [0.968, 1.004] | 1.000 [0.983, 1.010] | 0.997 | 0.998 | 1.000 |
| `status --verify`, 10,000 files, in memory | ext4 | 1.003 [0.998, 1.024] | 1.004 [1.001, 1.016] | 1.059 | 0.999 | 0.998 |
| `cache verify`, 100,000 objects, in memory | ext4 | 1.002 [0.993, 1.005] | 1.001 [0.995, 1.008] | 1.000 | 1.003 | 1.000 |
| `cache verify`, 10,000 objects, in memory | ext4 | 1.007 [0.989, 1.016] | 0.997 [0.982, 1.027] | 1.000 | 1.006 | 0.997 |

the cold `cache verify` of 10,000 objects shared its rounds with about 6 busy cores of other work; the pass before, with the copied paths, measured it at 0.408 [0.400, 0.413] beside an a/a arm at 0.999. kept: every cold case clears its a/a spread by far, and with the files in memory every case and every peak memory sits within its a/a spread. cold, the peak memory of `cache verify` of 100,000 objects, 1.007 [0.985, 1.011] beside 0.998 [0.979, 1.000], is 0.5 MiB more at the medians, its interval overlapping the a/a arm's.

### a large state beside the walk

[the state beside the folder walk](#the-state-beside-the-folder-walk) was dropped for the memory a project of 10,000 files lost to the reading thread's own heap. read on its own thread from some size up, it can keep a large project's gain while a small one keeps its memory. always on, a musl release build of 7b2248cb with the switch, tmpfs, 10 pairs, 5 at 1,000,000 files, measured beside 4 busy training processes as above:

| files | state | no-op sync | its peak memory | status | its peak memory |
|---:|---:|---|---|---|---|
| 10,000 | 2.4 MB | 0.862 [0.841, 0.882], a/a 1.020 | 39.3 to 57.3 MiB, 1.459 | 0.875 [0.861, 0.883], a/a 0.998 | 40.6 to 58.7 MiB, 1.447 |
| 30,000 | 7.2 MB | 0.816 [0.797, 0.822], a/a 1.000 | 55.3 to 69.2 MiB, 1.251 | 0.852 [0.833, 0.860], a/a 1.001 | 62.8 to 76.7 MiB, 1.224 |
| 50,000 | 12.1 MB | 0.792 [0.789, 0.798], a/a 0.997 | 81.1 to 66.9 MiB, 0.827 | 0.834 [0.826, 0.853], a/a 0.999 | 68.5 to 79.7 MiB, 1.164 |
| 100,000 | 24.5 MB | 0.793 [0.778, 0.814], a/a 0.998 | 85.2 to 87.2 MiB, 1.014 | 0.801 [0.797, 0.824], a/a 1.008 | 86.6 to 87.8 MiB, 1.012 |
| 1,000,000 | 242 MB | 0.825 [0.786, 0.897], a/a 1.001 | 554.9 to 595.9 MiB, 1.063 | 0.866 [0.843, 0.876], a/a 1.016 | 566.4 to 598.9 MiB, 1.064 |

the 30,000 and 50,000-file projects are three and five 10,000-file listings. up to 12.1 MB the thread cost 11 to 18 MiB, as much as a state of 2.4 MB, whatever it saved; the 50,000-file no-op came out lower, but its base peaked above both neighbours, as the 2 MiB pages of mimalloc's huge pages can make a peak, so that one is not read as a saving. from 24.5 MB the cost is small beside the run's own. so a survey of the whole data folder now opens the state and reads one of 16 MiB or more, `BESIDE_FROM`, on a thread of its own, through the run's thread helper, so a refused thread reads it after the walk; a run kept to one worker, as `FETCHLOOM_THREADS=1` or the lean level keeps it, starts no thread. a read within named folders and a smaller state go as before. against its parent, 10 pairs, 14 at 30,000 and the second 50,000 pass, 5 at 1,000,000 files:

| case | wall | a/a wall | peak memory | a/a peak memory |
|---|---|---|---|---|
| no-op sync, 10,000 files | 0.999 [0.981, 1.018] | 0.986 [0.968, 1.008] | 0.997, 39.4 MiB both | 0.997 |
| status, 10,000 files | 1.004 [0.995, 1.012] | 0.994 [0.986, 1.010] | 0.999, 40.7 MiB both | 1.004 |
| no-op sync, 30,000 files | 1.005 [0.996, 1.010] | 1.006 [0.994, 1.011] | 1.000 | 1.001 |
| no-op sync, 50,000 files | 1.027 [1.002, 1.050]; again 1.016 [1.006, 1.025] | 1.005 [0.994, 1.017]; 1.001 [0.996, 1.018] | 0.998, 1.003 | 0.999, 1.001 |
| status, 50,000 files | 1.002 [0.997, 1.014] | 0.992 [0.987, 0.998] | 0.997 | 0.999 |
| no-op sync, 100,000 files | 0.794 [0.788, 0.820] | 0.994 [0.979, 1.006] | 1.036 [1.022, 1.049], 83.4 to 87.1 MiB | 1.025 [1.000, 1.028] |
| status, 100,000 files | 0.812 [0.799, 0.827] | 1.001 [0.993, 1.030] | 1.035 [1.014, 1.067], 84.8 to 86.8 MiB | 1.000 [0.997, 1.005] |
| no-op sync, 1,000,000 files | 0.826 [0.805, 0.845] | 0.991 [0.989, 1.012] | 1.072 [1.042, 1.103], 560.0 to 600.4 MiB | 0.999 [0.989, 1.011] |
| status, 1,000,000 files | 0.842 [0.826, 0.846] | 1.002 [0.994, 1.005] | 1.059 [1.045, 1.075], 567.2 to 602.6 MiB | 1.003 [0.995, 1.006] |

below 16 MiB the survey takes the parent's path, the same reads in the same order. the 50,000-file no-op's 1.6 to 2.7% is not work: `perf stat` over 15 runs of each build twice counted 1,027.94 million user instructions against 1,027.71 million, 0.02% more, and cycles of 371 to 379 million against 369 to 383 million. kept: 100,000 files and more 0.79 to 0.84 of their time, with 2.0 to 3.7 MiB more peak memory at 100,000 files and 35 to 40 MiB, 6 to 7%, at 1,000,000. `resources = lean` keeps the read before the walk at any size, since lean spends memory before time; a run there takes the parent's path, as below 16 MiB, and starts no thread, which a test of the decision over sizes, worker caps and levels holds.

### the release recheck after part four

`bench/recheck.py` with its own fixtures for each arm on tmpfs, release builds of 6a580e98, the last of part four, against f03ffccd, the commit before it, musl and glibc, a byte-identical copy of f03ffccd's at another path as the a/a arm in the same rounds, the measured commands on cores 12–23 under the measure lock, beside 4 busy training processes as above:

| case | libc | pairs | base | against the base | a/a arm | against the a/a arm | peak memory | a/a peak memory |
|---|---|---:|---|---|---|---|---|---|
| the 3-file settled sync | musl | 100 | 4.2 ms | 0.994 [0.986, 1.008] | 1.002 [0.991, 1.008] | 0.997 [0.991, 1.002] | 0.998 | 0.999 |
| `version` | musl | 100 | 2.6 ms | 0.990 [0.984, 1.004] | 0.997 [0.981, 1.007] | 0.994 [0.984, 1.006] | 0.996 | 1.000 |
| `help` | musl | 100 | 2.8 ms | 0.996 [0.982, 1.008] | 0.988 [0.975, 1.006] | 0.998 [0.985, 1.006] | 1.002 [1.000, 1.006] | 1.000 [0.998, 1.000] |
| placing 40,000 datasets | musl | 10 | 1,099.8 ms | 1.011 [1.003, 1.013] | 1.004 [0.998, 1.013] | 1.003 [0.997, 1.011] | 1.016 [1.000, 1.050] | 1.027 [0.973, 1.036] |
| merge across 1,000 datasets | musl | 15 | 98.8 ms | 1.011 [1.008, 1.018] | 1.007 [1.001, 1.012] | 1.002 [0.999, 1.015] | 1.001 | 1.001 |
| drop across 1,000 datasets | musl | 15 | 45.8 ms | 0.995 [0.987, 1.000] | 1.005 [0.999, 1.009] | 0.989 [0.983, 0.995] | 0.999 | 1.000 |
| the 3-file settled sync | glibc | 100 | 4.3 ms | 1.006 [1.000, 1.010] | 1.005 [1.001, 1.014] | 0.999 [0.991, 1.005] | 0.997 | 1.005 |
| `version` | glibc | 100 | 2.8 ms | 0.994 [0.990, 1.002] | 0.995 [0.989, 1.001] | 1.001 [0.995, 1.005] | 1.001 | 1.000 |
| `help` | glibc | 100 | 3.1 ms | 0.999 [0.991, 1.001] | 1.006 [1.001, 1.013] | 0.991 [0.983, 0.996] | 0.992 | 1.001 |

"against the a/a arm" is each round's candidate over the same round's copy of the base, with a bootstrap interval of the median. nothing is slower than its a/a arm: merge and placing moved with theirs, which a copy of the base cannot do by its code, and against it they are 1.002 and 1.003, intervals across 1. none of part four's changes is on those paths: under strace the 40,000-dataset sync looks at `urls/` once, as every run that opens the cache does, and reads no record, its state missing before the run and 9.6 MB after; merge touches neither.

# windows defender

defender scanning can dominate the cost of writing many small random-looking files on windows.

## recorded workload

on windows 11 with real-time protection on, defender's performance recorder traced unpacking `files-10k.tar.zst`: 10,000 files of 4 KiB random bytes. the machine was the [baseline laptop](budgets.md#baseline-conditions). `Get-MpPerformanceReport` showed that every staged member was scanned on close.

| probe | commit and runs | measured result | median and range limits |
|---|---|---|---|
| unpack random archive members with eight writers | commit and run count not recorded | 10,000 scans; 228 s total scan time; unpacking 34 to 39 s | scan median 21 ms; wall median and scan range not recorded |
| skip small-member flushes | commit not recorded; one reported run | 10,000 scans; unpacking 54 s | scan median 24 ms; ranges not recorded |
| write 5,000 random 4 KiB files from eight processes | commit and run count not recorded | 16.0 s | median and range not recorded |
| same probe with low-entropy bytes | same study | 4.5 to 5.6 s | median not recorded |
| create files read-only or writable | same study | no change | absolute median and range not recorded |
| use `.new` or `.bin` names | same study | no change | absolute median and range not recorded |
| signed python interpreter writing random `.new` files on one thread | same study; run count not recorded | 0.6 ms per file | median and range not recorded |

scan times overlap across writers. 228 s is the sum of recorded scan durations, not elapsed time. content entropy and executable trust changed the cost; renaming files or skipping flushes did not remove it.

## implications

keep payload flushes. removing them loses durability without removing scans. publication by rename was estimated to save about 1 s of a 34 s unpack, below the 10% threshold for keeping that change.

signed releases that earn reputation, or a dev drive with performance mode, may lower this cost. fetchloom never asks users to disable protection or add exclusions. code changes should preserve those boundaries rather than trying to suppress scanning.

for a new investigation, record defender's own events alongside wall time and process load. compare the same bytes, file count, writer count, filesystem and signing state. an unprofiled repeat is needed before treating a recorder run as ordinary performance.

related: [measurements](measurements.md), [archives](../architecture/archives.md), [releasing](../engineering/releasing.md), [roadmap](../roadmap.md).

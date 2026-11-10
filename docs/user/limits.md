# limits

fetchloom fetches and pins published inputs; it leaves analysis, publication and data hosting to other systems.

| outside fetchloom | what to use or expect |
|---|---|
| preprocessing and workflow execution | call fetchloom from make, snakemake, nextflow or your script |
| publishing and uploads | use the data repository's upload interface |
| data branches and merges | manage produced data separately from fetched inputs |
| cloud copying and mirroring | use a transfer application such as rclone |
| private and requester-pays buckets | use the provider's own tools; fetchloom reads [public buckets](sources/buckets.md) only |
| search beyond hugging face, kaggle and zenodo | choose a reference from the source's own catalog |
| plugins | source adapters live in the fetchloom crate |
| fetchloom accounts, hosting and telemetry | fetchloom is a local command |
| controlled-data access approval | request access from the institution or owner |
| rar extraction | keep the packed file and use another decoder |
| recursive archive extraction | nested archives remain files |
| cross-user cache administration | cache writers are trusted; no group-permission management is provided |

## scale

| what | bound | how to go further |
|---|---|---|
| files one dataset lists | `FETCHLOOM_MOST_FILES`: the memory budget at about 2 KiB a file, at least 100,000; 2,097,152 with a 4 GiB budget | raise `FETCHLOOM_MOST_FILES` or `FETCHLOOM_MEMORY`, or list a folder with select patterns |
| files in `data.lock` | 50,000 a dataset; more go to compressed parts in [`data.lock.d`](reference/data-lock.md#big-datasets) | commit `data.lock.d` with `data.lock` |
| bytes a run downloads | none; past `FETCHLOOM_ASK_ABOVE` it asks first, and it checks the free space of the cache's disk before starting | `--yes`, or a bigger cache disk with `FETCHLOOM_CACHE` |
| downloads at once | `FETCHLOOM_JOBS`, 8, and `FETCHLOOM_HOST_JOBS`, 4 to one site | raise both for a fast link or your own mirror |
| what one archive unpacks to | 10,000,000 entries, 16 TiB, 1000 times its packed size | `FETCHLOOM_ARCHIVE_FILES`, `FETCHLOOM_ARCHIVE_SIZE`, `FETCHLOOM_ARCHIVE_RATIO`, for an archive you trust |

a stopped download keeps what it fetched and goes on from there in the next run, so a pull of hundreds of gigabytes survives a dropped connection or a full disk.

## commands and integrations not implemented

`cite`, archive-service fallbacks and lab administration are not implemented. public S3 and google cloud storage buckets are listed anonymously over HTTPS; signed requests, other S3-compatible hosts and azure blob storage are not implemented. the python module provides `path()` and `sync()`; there is no R wrapper or other language or workflow wrapper.

see the [developer roadmap](../dev/roadmap.md) for future work. current commands are listed in the [command guide](commands/README.md).

## reproducibility boundaries

file hashes and sizes pin bytes. they do not guarantee that a source will host those bytes forever. locked 404 or 410 downloads can try a replacement URL from the source, but the bytes must still match.

cached objects are not rehashed on every use. an edit through a placed hardlink moves the cached copy's time and is caught before the copy is used again, but a change that puts the time back, as `touch -r` does, is caught only by `--verify` or `fetchloom cache verify`. status uses saved metadata and selective hashing, and cache verification does not check independent copies in `data/`.

sync preserves local conflicts by default and can exit 0 while keeping them. `path` returns a location even when an edited file remains there. scripts that require untouched inputs must inspect [status and placement results](scripting.md#checking-and-syncing).

resumption depends on usable hashes or server validators. unsupported ranges, changed validators or damaged saved bytes can require restarting a transfer.

offline mode uses local placements, cache objects and unpack records. it cannot resolve changed manifest entries or repository listings. [offline preparation](concepts.md#offline) avoids discovering those gaps after disconnecting.

## access and platforms

windows, linux and macos are supported on x86_64 and arm64; macos needs version 11 or later. [installation](getting-started.md#install) is currently from source. one executable is installed as `fetchloom`. an accepted source spelling does not guarantee that access or a download will succeed.

source restrictions and listing-size limits apply before most filtering. credentials cannot grant approvals that a source has not given. [source guides](sources/README.md) lists each family's rules.

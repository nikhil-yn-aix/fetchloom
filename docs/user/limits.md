# limits

fetchloom fetches and pins published inputs; it leaves analysis, publication and data hosting to other systems.

| outside fetchloom | what to use or expect |
|---|---|
| running workflows: steps at once, on a scheduler, or in a sandbox | `fetchloom build` runs [recipes](recipes.md) one step at a time, as you, on the machine it runs on, with no sandbox; on windows ending a step ends its own process, not what it started. for steps in parallel or jobs on a scheduler, call fetchloom from make, snakemake, nextflow or your script |
| publishing and uploads | use the data repository's upload interface |
| data branches and merges | manage produced data separately from fetched inputs |
| cloud copying, and web mirrors | [`fetchloom push`](commands/push.md) fills a folder [mirror](mirrors.md); copy that folder to a server or a bucket with a transfer application such as rclone |
| requester-pays buckets, private google cloud storage buckets and AWS keys from roles or IAM identity center | use the provider's own tools, or export the keys; fetchloom reads [private S3 buckets](sources/buckets.md#private-s3-buckets) with keys from the environment or `~/.aws` |
| search beyond hugging face, kaggle and zenodo | choose a reference from the source's own catalog |
| plugins | source adapters live in the fetchloom crate |
| fetchloom accounts, hosting and telemetry | fetchloom is a local command |
| controlled-data access approval | request access from the institution or owner |
| rar extraction | the `.rar` stays packed, and its warning names a command that unpacks it outside the data folder: unrar on linux, unar on macos, 7-Zip on windows |
| recursive archive extraction | an archive inside another remains a file, and the sync that places it names a command that unpacks it beside itself |
| cross-user cache administration | cache writers are trusted; no group-permission management is provided. [one cache for a whole lab](hpc.md#one-cache-for-a-whole-lab) shows how to share one by hand |
| verified erasure of deleted data | fetchloom deletes files without overwriting them; [deleting a dataset from a machine](concepts.md#deleting-a-dataset-from-a-machine) lists the copies to remove |

## scale

| what | bound | how to go further |
|---|---|---|
| files one dataset lists | `FETCHLOOM_MOST_FILES`: the memory budget at about 2 KiB a file, at least 100,000; 2,097,152 with a 4 GiB budget | raise `FETCHLOOM_MOST_FILES` or `FETCHLOOM_MEMORY`, or list a folder with select patterns |
| files in `data.lock` | 50,000 a dataset; more go to compressed parts in [`data.lock.d`](reference/data-lock.md#big-datasets) | commit `data.lock.d` with `data.lock`; `git add data.lock.d` takes a new list, which `status` names until git has it |
| bytes a run downloads | none; past `FETCHLOOM_ASK_ABOVE` it asks first, and it checks the free space of the cache's disk before starting | `--yes`, or a bigger cache disk with `FETCHLOOM_CACHE` |
| downloads at once | `FETCHLOOM_JOBS`, 8, and `FETCHLOOM_HOST_JOBS`, 4 to one site | raise both for a fast link or your own mirror |
| one file on a FAT32 disk | 4 GiB less one byte. a larger file is refused before it is downloaded to a cache there, copied into a data or `get` folder there, or unpacked there from an archive; exFAT has no such limit | a disk formatted as exFAT, NTFS, APFS or ext4 |
| what one archive unpacks to | 10,000,000 entries, 16 TiB, 1000 times its packed size | `FETCHLOOM_ARCHIVE_FILES`, `FETCHLOOM_ARCHIVE_SIZE`, `FETCHLOOM_ARCHIVE_RATIO`, for an archive you trust |

a stopped download keeps what it fetched and goes on from there in the next run, where its server sends parts of a file, so a pull of hundreds of gigabytes survives a dropped connection or a full disk. the stop says when its server cannot continue, and the next run then starts over.

## commands and integrations not implemented

`cite`, archive-service fallbacks and lab administration are not implemented. google cloud storage buckets are listed anonymously over HTTPS; S3 buckets anonymously, or signed with AWS keys for [buckets you list](sources/buckets.md#private-s3-buckets). AWS keys from roles, IAM identity center or `credential_process`, and azure blob storage, are not implemented. the python module provides [`path()`, `sync()` and `get()`](scripting.md); there is no R wrapper or other language or workflow wrapper.

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

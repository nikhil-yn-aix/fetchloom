# limits

fetchloom fetches and pins published inputs; it leaves analysis, publication and data hosting to other systems.

| outside fetchloom | what to use or expect |
|---|---|
| running workflows: steps at once, on a scheduler, or in a sandbox | `fetchloom build` runs [recipes](recipes.md) one step at a time, as you, on the machine it runs on, with no sandbox. for steps in parallel or jobs on a scheduler, call fetchloom from make, snakemake, nextflow or your script |
| publishing and uploads | use the data repository's upload interface |
| data branches and merges | manage produced data separately from fetched inputs |
| cloud copying, and web mirrors | [`fetchloom push`](commands/push.md) fills a folder [mirror](mirrors.md); copy that folder to a server or a bucket with a transfer application such as rclone |
| requester-pays buckets, AWS keys from an instance's or a container's role, and google credentials from a metadata server, federation or impersonation | use the provider's own tools, or export the keys; fetchloom reads [private S3 buckets](sources/buckets.md#private-s3-buckets) with keys from the environment or `~/.aws`, including a `credential_process` and, through the AWS CLI, sign-ins and roles, [private cloud storage buckets](sources/buckets.md#private-cloud-storage-buckets) with a service account's key or a gcloud login, and [private drive files and folders](sources/google-drive.md#private-files-and-folders) with a service account's key or a login through an oauth client of your own |
| a private github repository's release files, gitlab servers other than gitlab.com, and the source archives a gitlab release makes on request | download them with the gh or glab command, then [`fetchloom add`](commands/add.md#files-on-this-machine) the folder to pin them by their bytes |
| search beyond hugging face, kaggle, zenodo, ENA, GEO, dandi and openneuro | find the dataset on its source's own site, then give fetchloom its link or id |
| arXiv's latex sources, and bulk pdfs from arXiv's own bucket | arXiv keeps programs out of its sources and bills the reader of its bucket; download a paper's source from its page, or use the AWS CLI with `--request-payer requester`, as [arXiv](sources/arxiv.md#arxivs-terms) says |
| plugins | source adapters live in the fetchloom crate |
| fetchloom accounts, hosting and telemetry | fetchloom is a local command |
| controlled-data access approval | request access from the institution or owner |
| rar extraction | the `.rar` stays packed, and its warning names a command that unpacks it beside itself in the data folder: unrar on linux, unar on macos, 7-Zip on windows |
| recursive archive extraction | an archive inside another remains a file, and the sync that places it names a command that unpacks it beside itself |
| cross-user cache administration | cache writers are trusted; no group-permission management is provided. [one cache for a whole lab](hpc.md#one-cache-for-a-whole-lab) shows how to share one by hand |
| verified erasure of deleted data | fetchloom deletes files without overwriting them; [deleting a dataset from a machine](concepts.md#deleting-a-dataset-from-a-machine) lists the copies to remove |

## scale

| what | bound | how to go further |
|---|---|---|
| files one dataset lists | `FETCHLOOM_MOST_FILES`: the memory budget at about 2 KiB a file, at least 100,000; 2,097,152 with a 4 GiB budget | raise `FETCHLOOM_MOST_FILES` or `FETCHLOOM_MEMORY`, or list a folder with select patterns |
| files in `data.lock` | 50,000 a dataset; more go to compressed parts in [`data.lock.d`](reference/data-lock.md#big-datasets) | commit `data.lock.d` with `data.lock`; `git add data.lock.d` takes a new list, which `status` names until git has it |
| memory to read a project in a container | about 650 bytes for each file data.lock pins, plus 8 MiB, or 20 MiB with the static build linux installs. a command whose datasets need more than a container's memory limit leaves stops with `local.io`, exit 50, before it reads their files, rather than being ended by the kernel partway. a status or sync of 100,000 files peaked at 77 MiB, 93 MiB on the static build; a first sync or an import of the same files takes about twice that, and only the container's limit stops it | raise the container's memory limit, or name one dataset: `fetchloom path <name>` reads only that dataset's pins and state |
| bytes a run downloads | none; past `FETCHLOOM_ASK_ABOVE` it asks first, and it checks the free space of the cache's disk before starting | `--yes`, or a bigger cache disk with `FETCHLOOM_CACHE` |
| downloads at once | `FETCHLOOM_JOBS`, 8, and `FETCHLOOM_HOST_JOBS`, 4 to one site; arXiv's hosts one at a time, 3 s apart | the [resources level](resources.md) `max`, or raise both, for far servers or your own mirror; arXiv's pace is its own terms, and its mirror on google's storage is not paced |
| one file on a FAT32 disk | 4 GiB less one byte. a larger file is refused before it is downloaded to a cache there, copied into a data or `get` folder there, or unpacked there from an archive; exFAT has no such limit | a disk formatted as exFAT, NTFS, APFS or ext4 |
| what one archive unpacks to | 10,000,000 entries, 16 TiB, 1000 times its packed size | `FETCHLOOM_ARCHIVE_FILES`, `FETCHLOOM_ARCHIVE_SIZE`, `FETCHLOOM_ARCHIVE_RATIO`, for an archive you trust |

a stopped download keeps what it fetched and goes on from there in the next run, where its server sends parts of a file, so a pull of hundreds of gigabytes survives a dropped connection or a full disk. the stop says when its server cannot continue, and the next run then starts over.

## commands and integrations not implemented

`cite`, archive-service fallbacks and lab administration are not implemented. buckets and containers are read anonymously over HTTPS, or with your keys or sign-in for those you list, and private google drive items through drive's api for the folders and files you list, as [signing in](signing-in.md#private-buckets-and-folders) says. AWS keys from an instance or container metadata service and google credentials from a metadata server are not implemented. the python module provides [`path()`, `sync()` and `get()`](scripting.md); there is no R wrapper or other language or workflow wrapper.

see the [developer roadmap](../dev/roadmap.md) for future work. current commands are listed in the [command guide](commands/README.md).

## reproducibility boundaries

file hashes and sizes pin bytes. they do not guarantee that a source will host those bytes forever. locked 404 or 410 downloads can try a replacement URL from the source, but the bytes must still match.

cached files are not hashed again on every use. an edit through a placed hardlink moves the cached copy's time and is caught before the copy is used again, but a change that puts the time back, as `touch -r` does, is caught only by `--verify` or `fetchloom cache verify`. status compares sizes and times, and cache verification does not check copies in `data/`.

sync preserves local conflicts by default and can exit 0 while keeping them. `path` returns a location even when an edited file remains there. scripts that require untouched inputs must inspect [status and placement results](scripting.md#checking-and-syncing).

resumption depends on usable hashes or server validators. unsupported ranges, changed validators or damaged saved bytes can require restarting a transfer.

offline mode uses placed files and the cache. it cannot resolve changed entries in data.toml or list a repository; [offline](concepts.md#offline) says how to prepare.

## access and platforms

windows, linux and macos are supported on x86_64 and arm64; macos needs version 11 or later. until the first release, [installation](getting-started.md#install) is from source. an accepted source spelling does not guarantee that access or a download will succeed.

source restrictions and listing-size limits apply before most filtering. credentials cannot grant approvals that a source has not given. [source guides](sources/README.md) lists each family's rules.

# clusters and shared machines

a cluster or a shared server has a small home folder, a large scratch disk and jobs that run on other machines, often without the internet. keep each project and its cache on one drive, and prepare data on a login node for the jobs.

## keep the project and the cache on one drive

fetchloom keeps one copy of each file in its cache and links it into each project. a link only works when the project's data folder and the cache are on the same drive. on most clusters, home and scratch are different drives. a project in home with its cache on scratch still works, but every project gets its own full copy of the files, in home:

```text
warning: the cache is on another drive than the project, so files were copied
  help: to link files instead, put both on one drive: move the project to the cache's drive, or set FETCHLOOM_CACHE to a folder on the drive of the project. see https://nikhil-yn-aix.github.io/fetchloom/user/concepts/cache-and-placement/#links-need-one-drive
```

pick one of these:

| where the project is | where the cache is | what you get |
|---|---|---|
| scratch | scratch | links, no copies. the best choice for large data |
| home | home | links, no copies, but the data counts against your home quota |
| home | scratch | a copy of the data in each project. fine for small data |

to use scratch for both, clone the project there and point the cache at scratch:

```sh
export FETCHLOOM_CACHE=/scratch/$USER/fetchloom-cache
cd /scratch/$USER
git clone https://example.org/lab/project.git
cd project
fetchloom sync
```

to keep that cache for every shell and job without an `export`, put `cache = "/scratch/<user>/fetchloom-cache"` in the [settings file](../reference/environment.md#settings-file). `fetchloom settings` shows the cache a job will use.

keep the code in git. if scratch deletes old files, `fetchloom status` shows what is missing and `fetchloom sync` downloads it again; a deleted project comes back with `git clone` and `fetchloom sync`.

## many jobs at once

jobs that share one `FETCHLOOM_CACHE` can run fetchloom at the same time; each file downloads once. download on the login node before you submit, so jobs only read. a login node is shared, and `FETCHLOOM_RESOURCES=lean` keeps a sync there to 2 requests at once and a small memory budget, as [resources](speed-and-memory.md) says:

```sh
fetchloom sync --locked
sbatch --array=1-100 job.sh
```

in `job.sh`, ask fetchloom where the data is, without the network:

```sh
export FETCHLOOM_CACHE=/scratch/$USER/fetchloom-cache
python train.py --data "$(fetchloom --offline path iris)"
```

`--offline` makes a job fail at once with a clear error if a file is missing, instead of downloading it on a node that may have no internet. `--locked` stops the sync if `data.toml` and `data.lock` disagree, so every job sees the same files.

## many small files

a cluster's disk usually limits how many files you may keep, an inode quota, which `lfs quota` shows on lustre and `quota` on many other systems. on lustre, every file a job opens is also a request to one metadata server that all users share. webdataset shards avoid both: a `.tar` holds thousands of samples, and the loader reads it packed. unpacked, every sample in every shard becomes a file of its own, and each takes an inode. keep them packed:

```sh
fetchloom add s3://bucket/shards/ --no-extract
```

or set `extract = false` on the dataset in `data.toml` before its first sync. `add`, `get` and `sync` hint this before downloading when a dataset holds at least 8 numbered `.tar` shards in one folder, and `add` of a folder of them before copying it into the cache; [webdataset shards](../reference/archives.md#webdataset-shards) shows the hint.

## the cache on a shared disk

the cache uses file locks, so `cache gc` never deletes a file another fetchloom is about to use. on NFS, the locks work between machines. on an NFS 4.2 server that lets clients keep file times, as recent linux servers do, a file whose download a later run finished, after a stop, is hashed again each time a new project uses it; downloads finished in one run are not. on lustre, they work between machines only when the disk is mounted with the `flock` option; with `localflock` each machine sees only its own locks, so run `fetchloom cache gc` only when no job is using the cache. if your cluster mounts it with `noflock`, or its NFS lock manager is not running, which makes every lock fail with "no locks available", fetchloom cannot lock the cache: `add`, `sync` and `get` go on without the lock, and a download they leave unfinished starts over in the next run: a stopped job's log says its bytes are deleted, since no later run could find them, and they leave nothing in the cache. `cache gc` refuses and says why. ask your admins, or keep the cache on a disk that allows locks.

## one cache for a whole lab

each person's cache is theirs alone: a cache folder fetchloom makes is readable by its owner only. a cache several accounts write is safe only among people who trust each other, since anyone who can write it can change what every project reads. to share one, make its folder first, owned by the lab's group, with the group allowed to write and new files keeping the group:

```sh
mkdir -p /scratch/lab/fetchloom-cache
chgrp mylab /scratch/lab/fetchloom-cache
chmod 2770 /scratch/lab/fetchloom-cache
```

then each member runs `umask 002`, so the files and folders they add stay writable for the group, and sets `FETCHLOOM_CACHE=/scratch/lab/fetchloom-cache`. fetchloom keeps a folder's permissions once it exists. a run that adds to the cache warns with `cache.unshared` when a folder in it lets only its owner write to it, as one a cron job or a notebook server made with the system's usual `umask 022` does, and on linux when its own umask would make such folders; the folder's owner gives the group its write with `chmod g+w`. a download such a folder refuses stops with `local.io`, exit 50, naming the folder, its owner and that `chmod`. such a cache is shared on purpose, so its members hear no `cache.foreign` warning that it belongs to another user, while one every user may write still gets it. linux usually refuses a hardlink to a file someone else owns, so fetchloom places files another member downloaded as reflinks or copies. `cache gc` reads every project that uses the cache before it frees anything, so keep each project's `data.toml`, `data.lock` and `data.lock.d` readable by the group: a project in a folder only its owner may enter, as `/scratch/$USER` often is, stops everyone else's gc, which then names the project and its owner and keeps every file. [concepts](../concepts/cache-and-placement.md#cache) has the rules for sharing a cache.

a file a project's `data.lock` pins is shared by every member, whoever downloaded it, since its SHA-256 checks it. a link no project pins is different: anyone who writes the cache could say any bytes are what a link serves, so `get` takes from the cache only the links you downloaded yourself. your reruns and your jobs share each download, a rerun asks the server only whether the file changed, and `--offline get` works, but a link another member fetched downloads again for you, and offline it stops with `offline.missing`, exit 20. to share such a file with the lab, pin it in a project with `fetchloom add`.

a lab that would rather not share a cache can share a [mirror](mirrors.md): a folder of pinned files each person's own cache copies from and checks before use. each member sets `mirrors = ["/scratch/lab/fetchloom-mirror"]` and `push_to = "/scratch/lab/fetchloom-mirror"` in their settings file; whoever synced a project fills the mirror with [`fetchloom push`](../commands/push.md), and nobody needs to write to anyone else's cache.

## no internet on compute nodes

a node without internet can still use the data the cache holds. prepare online, on the login node, then use `--offline` or `FETCHLOOM_OFFLINE=1` in jobs. a job that leaves it out on a node with no route to the source, or no network at all, ends at once, and its error names `--offline`. an offline job still reads a folder [mirror](mirrors.md) on a shared disk. [CI](ci.md) shows the same idea for build machines.

## backups and copies

a file fetchloom placed by hardlink and its cached copy are one file with two names. `rsync -a` copies each name as a file of its own, so a backup of a project together with its cache stores every file twice. `rsync -aH`, and `cp -a` on linux, keep the links among the files they copy. in one test on linux, a project and its cache holding one 4.0 MiB file took 4.1 MiB after `rsync -aH` and 8.1 MiB after `rsync -a`.

`fetchloom sync` in such a copy, with `FETCHLOOM_CACHE` naming the cache copied with it, links each file to that cache's copy again, reading it first, as [copies of cached files](../commands/sync.md#copies-of-cached-files) describes. to copy a project alone, leave `data/` out and run `fetchloom sync` in the copy. a project copied with its data folder to another machine takes what that folder holds into the new cache, as [moving a project](../commands/cache.md#moving-a-project-to-another-machine) describes.

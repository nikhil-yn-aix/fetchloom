# clusters and shared machines

this page is for a cluster or a shared server: a small home folder, a large scratch disk, and jobs that run on other machines.

## keep the project and the cache on one drive

fetchloom keeps one copy of each file in its cache and links it into each project. a link only works when the project's data folder and the cache are on the same drive. on most clusters, home and scratch are different drives. a project in home with its cache on scratch still works, but every project gets its own full copy of the files, in home:

```text
warning: the cache is on another drive than the project, so files were copied
  help: to link files instead, put both on one drive: move the project to the cache's drive, or set FETCHLOOM_CACHE to a folder on the drive of the project. see https://nikhil-yn-aix.github.io/fetchloom/user/hpc.html
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

to keep that cache for every shell and job without an `export`, put `cache = "/scratch/<user>/fetchloom-cache"` in the [settings file](reference/environment.md#settings-file). `fetchloom settings` shows the cache a job will use.

keep the code in git. if scratch deletes old files, `fetchloom status` shows what is missing and `fetchloom sync` downloads it again; a deleted project comes back with `git clone` and `fetchloom sync`.

## many jobs at once

jobs that share one `FETCHLOOM_CACHE` can run fetchloom at the same time; each file downloads once. download on the login node before you submit, so jobs only read:

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

or set `extract = false` on the dataset in `data.toml` before its first sync. `add`, `get` and `sync` hint this before downloading when a dataset holds at least 8 numbered `.tar` shards in one folder; [webdataset shards](concepts.md#webdataset-shards) shows the hint.

## the cache on a shared disk

the cache uses file locks, so `cache gc` never deletes a file another fetchloom is about to use. on NFS, the locks work between machines. on an NFS 4.2 server that lets clients keep file times, as recent linux servers do, a file whose download a later run finished, after a stop, is hashed again each time a new project uses it; downloads finished in one run are not. on lustre, they work between machines only when the disk is mounted with the `flock` option; with `localflock` each machine sees only its own locks, so run `fetchloom cache gc` only when no job is using the cache. if your cluster mounts it with `noflock`, fetchloom cannot lock the cache: `add`, `sync` and `get` go on without the lock, and a download they leave unfinished starts over in the next run, while `cache gc` refuses and says why. ask your admins, or keep the cache on a disk that allows locks.

## one cache for a whole lab

each person's cache is theirs alone: a cache folder fetchloom makes is readable by its owner only. a cache several accounts write is safe only among people who trust each other, since anyone who can write it can change what every project reads. to share one, make its folder first, owned by the lab's group, with the group allowed to write and new files keeping the group:

```sh
mkdir -p /scratch/lab/fetchloom-cache
chgrp mylab /scratch/lab/fetchloom-cache
chmod 2770 /scratch/lab/fetchloom-cache
```

then each member runs `umask 002`, so the files and folders they add stay writable for the group, and sets `FETCHLOOM_CACHE=/scratch/lab/fetchloom-cache`. fetchloom keeps a folder's permissions once it exists. linux usually refuses a hardlink to a file someone else owns, so fetchloom places files another member downloaded as reflinks or copies. [concepts](concepts.md#cache) has the rules for sharing a cache.

a lab that would rather not share a cache can share a [mirror](mirrors.md): a folder of pinned files each person's own cache copies from and checks before use. each member sets `mirrors = ["/scratch/lab/fetchloom-mirror"]` in their settings file, and nobody needs to write to anyone else's cache.

## no internet on compute nodes

a node without internet can still use the data the cache holds. prepare online, on the login node, then use `--offline` or `FETCHLOOM_OFFLINE=1` in jobs. an offline job still reads a folder [mirror](mirrors.md) on a shared disk. [CI](ci.md) shows the same idea for build machines.

## recipes in jobs

`fetchloom build` runs each [recipe](recipes.md)'s step on the machine it runs on, one step at a time, so run it inside a job when a step needs the cluster. a step reads its inputs from the cache, so a job on a node without internet builds offline once the login node has put them there:

```sh
fetchloom sync                  # on the login node
fetchloom --offline build       # in the job
```

a job that ends before its build does leaves `data.lock` and the data folder as they were, and the next build clears what the step left in the cache's `tmp` folder. two of your jobs that build one recipe from the same inputs into one cache run its step once: the second waits for the first and takes its result. in [one cache for a whole lab](#one-cache-for-a-whole-lab), another person's build waits too, then runs the step itself, since a build reuses only results its own user recorded. a step that submits jobs of its own must wait for them to finish, since fetchloom takes in `{out}` as the step leaves it.

## backups and copies

a file fetchloom placed by hardlink and its cached copy are one file with two names. `rsync -a` copies each name as a file of its own, so a backup of a project together with its cache stores every file twice. `rsync -aH`, and `cp -a` on linux, keep the links among the files they copy. in one test on linux, a project and its cache holding one 4.0 MiB file took 4.1 MiB after `rsync -aH` and 8.1 MiB after `rsync -a`.

`status` and `sync` in such a copy report nothing wrong and leave both copies as they are. [`fetchloom cache`](commands/cache.md#copies-and-backups) counts them as copies that were linked when placed. to link a copy's files to its cache again, delete its `data/` folder, if it holds no edits you need, and run `fetchloom sync` there with `FETCHLOOM_CACHE` naming that cache. to copy a project alone, leave `data/` out and run `fetchloom sync` in the copy.

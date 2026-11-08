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

## the cache on a shared disk

the cache uses file locks, so `cache gc` never deletes a file another fetchloom is about to use. on NFS, the locks work between machines. on an NFS 4.2 server that lets clients keep file times, as recent linux servers do, a file whose download a later run finished, after a stop, is hashed again each time a new project uses it; downloads finished in one run are not. on lustre, they work between machines only when the disk is mounted with the `flock` option; with `localflock` each machine sees only its own locks, so run `fetchloom cache gc` only when no job is using the cache. if your cluster mounts it with `noflock`, fetchloom cannot lock the cache and says so; ask your admins, or keep the cache on a disk that allows locks.

## one cache for a whole lab

each person's cache is theirs alone: a cache folder fetchloom makes is readable by its owner only. a cache several accounts write is safe only among people who trust each other, since anyone who can write it can change what every project reads. to share one, make its folder first, owned by the lab's group, with the group allowed to write and new files keeping the group:

```sh
mkdir -p /scratch/lab/fetchloom-cache
chgrp mylab /scratch/lab/fetchloom-cache
chmod 2770 /scratch/lab/fetchloom-cache
```

then each member runs `umask 002`, so the files and folders they add stay writable for the group, and sets `FETCHLOOM_CACHE=/scratch/lab/fetchloom-cache`. fetchloom keeps a folder's permissions once it exists. linux usually refuses a hardlink to a file someone else owns, so fetchloom places files another member downloaded as reflinks or copies. [concepts](concepts.md#cache) has the rules for sharing a cache.

## no internet on compute nodes

a node without internet can still use the data the cache holds. prepare online, on the login node, then use `--offline` or `FETCHLOOM_OFFLINE=1` in jobs. [CI](ci.md) shows the same idea for build machines.

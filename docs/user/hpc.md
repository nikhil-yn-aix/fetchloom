# clusters and shared machines

this page is for a cluster or a shared server: a small home folder, a large scratch disk, and jobs that run on other machines.

## keep the project and the cache on one drive

fetchloom keeps one copy of each file in its cache and links it into each project. a link only works when the project's data folder and the cache are on the same drive. on most clusters, home and scratch are different drives. a project in home with its cache on scratch still works, but every project gets its own full copy of the files, in home:

```text
warning: the cache is on another drive, so files were copied
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

the cache uses file locks, so `cache gc` never deletes a file another fetchloom is about to use. on NFS, the locks work between machines. on lustre, they work between machines only when the disk is mounted with the `flock` option; with `localflock` each machine sees only its own locks, so run `fetchloom cache gc` only when no job is using the cache. if your cluster mounts it with `noflock`, fetchloom cannot lock the cache and says so; ask your admins, or keep the cache on a disk that allows locks.

## no internet on compute nodes

a node without internet can still use the data the cache holds. prepare online, on the login node, then use `--offline` or `FETCHLOOM_OFFLINE=1` in jobs. [CI](ci.md) shows the same idea for build machines.

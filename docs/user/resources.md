# resources

`resources` trades speed against memory in one word: `lean`, `default` or `max`. a level sets the download, memory and worker settings you have not set yourself, so you need not know which of them matter or what numbers fit your machine.

for one run:

```sh
FETCHLOOM_RESOURCES=max fetchloom sync
```

for every run, in the [settings file](reference/environment.md#settings-file):

```toml
resources = "lean"
```

the variable wins over the file, so `FETCHLOOM_RESOURCES=default` runs once at the default level whatever the file says. with neither, a run uses `default`, which is fetchloom with no level at all. data.toml cannot set a level: a project you clone does not decide what your machine spends.

## what each level sets

| setting | `lean` | `default` | `max` |
|---|---|---|---|
| `jobs`, requests at once | 2 | 8 | 32 |
| `host_jobs`, requests at once to one host | 2 | 4 | 16 |
| `memory`, the budget download buffers aim to stay within | 128 MiB, or the default budget when that is smaller | on linux a quarter of the memory available, from 256 MiB to 4 GiB; 256 MiB on macos and windows | as `default` |
| local workers, as `FETCHLOOM_THREADS` caps them | 2, or 1 on a machine with one processor | up to the processors | as `default` |

`max` leaves memory and local workers at the default, since no measure found a run faster with more of them. verification can use those spare workers for a few large files as well as many small ones. ceilings of one or two keep the old worker count, so lean adds no hash buffers.

a setting you set by name, with its variable or its key in the settings file, wins over any level. `FETCHLOOM_RESOURCES=max FETCHLOOM_HOST_JOBS=8 fetchloom sync` runs 32 requests at once, but at most 8 to one host. lean caps local workers only when `FETCHLOOM_THREADS` is unset. with that derived ceiling, each download keeps a required MD5 beside SHA-256 on its existing worker, without extra queued byte batches.

a level changes speed and memory only. it never changes which files are pinned or how they are checked, and it leaves every other setting as it is: `most_files`, which stays sized from the default budget so that lean lists every dataset `default` lists, the archive limits, `retries`, `timeout`, `ask_above`, `rate`, `split`, the mirrors and the buckets.

## which to pick

- `max` on a big machine that fetches many files from far away, from several servers at once, or from your own mirror that serves many connections well.
- `lean` on a small or shared machine: a laptop, a cluster's login node, a notebook or a container with little memory. it also suits a server that limits each client, since fewer requests at once get fewer of them refused.
- `default` for anything else.

## what was measured

these runs fetched small files from a test server whose link was slowed to the delay named:

- 100 files of 4 KiB from one server 50 ms away took 1.36 s at `default`. at 16 at once, what `max` allows one server, they took 0.46 s, about a third of the time, for 0.6 MiB more memory. at 2 at once, as `lean`, they took 2.64 s.
- the same files from a server 1 ms away took about 0.1 s both at `default` and at 16 at once, so on a fast, near link the level barely matters. at 2 at once they took 0.14 s.
- 100 files spread over five servers 50 ms away took half the time with `jobs` at 32 instead of 8.
- ten files from a server that allows two requests a second took about 4 s whatever the level. 16 at once sent 30 requests, the default 23 or 24, and 2 at once 18: more at once bought no time, only more refusals.

these runs used 12 to 16 MiB of memory at every level, far below any budget. how much lean saves when several large files download at once is not measured yet.

## see what a run uses

[`fetchloom settings`](commands/settings.md) shows the level and where it came from, and names the level as the origin of each value it set. rows from a real run:

```text
resources        lean                     environment       FETCHLOOM_RESOURCES
jobs             2                        resources = lean  FETCHLOOM_JOBS
host_jobs        2                        resources = lean  FETCHLOOM_HOST_JOBS
memory           128.0 MiB                resources = lean  FETCHLOOM_MEMORY
most_files       2,097,152                this machine      FETCHLOOM_MOST_FILES
threads          2                        resources = lean  FETCHLOOM_THREADS
```

`fetchloom settings --json` gives those values the origin `resources`. a level that is none of the three words stops every named command with kind `usage`, exit 2:

```text
error: FETCHLOOM_RESOURCES is "fast", which is not lean, default or max
  help: unset FETCHLOOM_RESOURCES to use the default level, or set it to lean, default or max
```

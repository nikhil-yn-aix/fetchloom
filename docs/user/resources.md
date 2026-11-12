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

`max` leaves memory and local workers at the default. checking placed files spreads a few large files over the local workers, as it does many small ones, but a ceiling of one or two workers keeps each file on one, so lean adds no hashing buffers.

a setting you set by name, with its variable or its key in the settings file, wins over any level. `FETCHLOOM_RESOURCES=max FETCHLOOM_HOST_JOBS=8 fetchloom sync` runs 32 requests at once, but at most 8 to one host. lean caps local workers only when `FETCHLOOM_THREADS` is unset, and under that cap a download hashes a required MD5 on the same worker as its SHA-256, so it queues no extra bytes.

a level changes speed and memory only. it never changes which files are pinned or how they are checked, and it leaves every other setting as it is: `most_files`, which stays sized from the default budget so that lean lists every dataset `default` lists, the archive limits, `retries`, `timeout`, `ask_above`, `rate`, `split`, the mirrors and the buckets.

## which to pick

- `max` on a big machine that fetches many files from far away, from several servers at once, or from your own mirror that serves many connections well.
- `lean` on a small or shared machine: a laptop, a cluster's login node, a notebook or a container with little memory. it also suits a server that limits each client, since fewer requests at once get fewer of them refused.
- `default` for anything else.

## see what a run uses

[`fetchloom settings`](commands/settings.md) shows the level and where it came from, and names the level as the origin of each value it set. rows from a real run:

```text
resources         lean                     environment       FETCHLOOM_RESOURCES
jobs              2                        resources = lean  FETCHLOOM_JOBS
host_jobs         2                        resources = lean  FETCHLOOM_HOST_JOBS
memory            128.0 MiB                resources = lean  FETCHLOOM_MEMORY
most_files        2,097,152                this machine      FETCHLOOM_MOST_FILES
threads           2                        resources = lean  FETCHLOOM_THREADS
```

`fetchloom settings --json` gives those values the origin `resources`. a level that is none of the three words stops every named command with kind `usage`, exit 2:

```text
error: FETCHLOOM_RESOURCES is "fast", which is not lean, default or max
  help: unset FETCHLOOM_RESOURCES to use the default level, or set it to lean, default or max
```

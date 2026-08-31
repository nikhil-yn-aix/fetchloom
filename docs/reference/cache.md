# The cache

The cache is reusable internal storage, addressed by content digest. One object
serves every project on the machine. It is an optimization and never the only
copy you have: your destination is the real copy, and deleting the cache costs
you time and nothing else.

## Where it is

| | Default |
|---|---|
| Windows | `%LOCALAPPDATA%\Fetchloom\Cache` |
| Linux | `$XDG_CACHE_HOME/fetchloom`, or `~/.cache/fetchloom` |

`--cache-dir <path>` sets it for one run, `FETCHLOOM_CACHE_DIR` for the
environment, and a relative `cache.dir` under a `[cache]` table in a project
`fetchloom.toml` sets it per project. `--no-cache` bypasses it entirely for one
operation: the transfer lives beside the destination and is discarded on
success, and verification is unchanged.

`fetchloom explain cache.dir` reports which of those supplied the value.

## What it holds

```
$ find cache -type d
cache/locks
cache/meta
cache/meta/object
cache/meta/prune
cache/meta/resolution
cache/meta/witness
cache/objects
cache/outboard
cache/partial
cache/pins
cache/quarantine
cache/receipts
cache/staging
```

| Directory | What is in it |
|---|---|
| `objects/` | Completed, fully verified objects, named by the hexadecimal of their content digest |
| `outboard/` | The chunk tree for each object above 64 MiB, which is what makes localized repair possible |
| `partial/` | Transfers in progress, each beside a record of where its bytes came from |
| `staging/` | Extraction trees not yet published |
| `quarantine/` | Objects that failed verification, each beside a `.diagnosis` naming what was found |
| `meta/object/` | One record per object: its interop digest and the fingerprint it was published with |
| `meta/resolution/` | One record per reference: the digest it last resolved to and the validator the source gave |
| `meta/witness/` | The witnesses recorded for one artifact |
| `meta/prune/` | Prune marks |
| `receipts/` | One receipt per destination, keyed by the digest of that destination's path |
| `locks/` | Advisory single-writer locks |
| `pins/` | Pin records |
| `format` | The fingerprint of the cache format this build writes |

Nothing appears in `objects/` that has not been fully verified. There is no
other way for a file to get there: a write goes to `partial/`, is flushed
according to the durability tier, and is renamed into place. A rename is
same-volume only, so a torn object cannot exist.

The layout is not a public interface. Read it to understand what is going on;
do not build on it.

## Sharing it

A cache directory may be used by several users at once, and Fetchloom never
assumes otherwise. There is one behavior rather than two, and no way to pick the
unsafe one by mistake. Objects are readable by every user of the directory and
writable only by whoever created them. Advisory locks must be honored across
users, so a volume that cannot express them is refused with
`cache.locking_unsupported` rather than used. Prune removes only objects the
invoking user created, and says what it skipped.

Two processes wanting the same object share the work: one transfers it and the
others wait and reuse the result. There is never a duplicate transfer of one
digest, and never a corrupted object.

## The commands

### status

```
$ fetchloom cache status
C:\Users\you\flman\cache
objects                2
bytes                258
partials               0
pins                   1
quarantined            0
```

`--json` gives the same as an object.

### ls

```
$ fetchloom cache ls
blake3:e17fbafc370dc4ba3552a31486adaa43fdf5d5d9e32884887f346caacb05a632           130
blake3:f7cccdd9d72f756232e20f080a37ef0dc352b525ff73f6b58f2952b0615091d0           128
```

These are the digests `pin` and `unpin` take.

### verify

Rereads and rehashes every object.

```
$ fetchloom cache verify
verified               2
quarantined            0
held                   0
```

A mismatch is moved to `quarantine/` and the command exits 80:

```
$ fetchloom cache verify
verified               1
quarantined            1
held                   0
blake3:e17fbafc370dc4ba3552a31486adaa43fdf5d5d9e32884887f346caacb05a632 was quarantined
```

The object is not deleted: those bytes are the evidence of what went wrong and
the input a localized repair needs. Beside it goes a diagnosis:

```json
{"digest":"blake3:e17fbafc370dc4ba3552a31486adaa43fdf5d5d9e32884887f346caacb05a632",
 "found":"blake3:263e9e009f51aa2121beeb44ce00b958815c25281a064081495bc1b131c91fc5",
 "size":130,
 "damaged":[],
 "localized":"no_tree_stored",
 "source":null,
 "validator":null,
 "quarantined_at":"2026-08-30T21:26:24Z",
 "next_action":"fetchloom repair blake3:e17fbafc370dc4ba3552a31486adaa43fdf5d5d9e32884887f346caacb05a632"}
```

`damaged` lists the byte ranges that failed against the chunk tree, ascending.
It is empty here because this object is 130 bytes, far below the 64 MiB
threshold at which a tree is stored, and `localized` says which of the three
reasons applies. An empty `damaged` is never read as no damage: the object is in
quarantine because it failed.

Quarantined objects are never served, are counted by `status`, and are removed
only by `prune` or `clear`. An object another writer holds is left alone and
counted as `held`.

### pin and unpin

```
$ fetchloom cache pin blake3:e17fbafc370dc4ba3552a31486adaa43fdf5d5d9e32884887f346caacb05a632
blake3:e17fbafc370dc4ba3552a31486adaa43fdf5d5d9e32884887f346caacb05a632 is pinned

$ fetchloom cache unpin blake3:e17fbafc370dc4ba3552a31486adaa43fdf5d5d9e32884887f346caacb05a632
blake3:e17fbafc370dc4ba3552a31486adaa43fdf5d5d9e32884887f346caacb05a632 is not pinned
```

A pinned object always survives a prune. Both take a digest rather than a
reference: the cache is addressed by digest everywhere else, a digest needs no
resolution and no network, and `ls` prints them.

### prune

Marks what nothing refers to, waits out a grace period, then sweeps what has
been marked longest.

```
$ fetchloom cache prune
removed                0
bytes                  0
kept                   2
quarantined            0
```

Objects that are pinned, in use by a running process, or referenced by a lock in
the working directory always survive. It is safe to run while other processes
are working.

The grace period is a race window, not a retention policy: it exists so an
object claimed between the mark and the sweep is not removed underneath whoever
claimed it. It is not configurable, and there is no rule about how long an
unused object is kept. Removing a destination and pruning the cache are separate
operations and neither implies the other.

### repair

Rebuilds the derived data the cache can regenerate from what it already holds: a
missing or failing chunk tree for an object whose bytes still verify, a lost
object record, a lock whose holder is not alive, and a partial or staging entry
with nothing behind it.

```
$ fetchloom cache repair
trees                  0
records                0
locks                  0
orphans                0
held                   0
```

It takes no argument, reaches no network, and resolves no reference. An object
whose own bytes fail is not repairable from the cache; it is left in quarantine
and the report names `fetchloom repair <ref>` as the command that can fetch
those bytes again. The two commands named repair do different jobs:
`cache repair` rebuilds from what is here, `repair <ref>` fetches what is not.

### clear

Removes every object. Refetching can cost hours and, on a metered source, money,
so it reports what it will remove and confirms first.

```
$ fetchloom cache clear
policy.terms_required: run it again with --yes, because clearing the cache needs an answer and this run has nowhere to ask
```

### export and import

`cache export` writes every object the cache holds into one bundle, and
`cache import` reads one back. This is how data crosses to a machine with no
network.

```
$ fetchloom cache export bundle.tar
1 objects  233 bytes  0 already held

$ FETCHLOOM_CACHE_DIR=./other fetchloom cache import bundle.tar
1 objects  233 bytes  0 already held
```

The whole air-gapped sequence, run end to end:

```
$ fetchloom get file://$PWD/sample.tar.gz --output data          # online, writes the lock
$ fetchloom plan file://$PWD/sample.tar.gz --output data2 > dataset.plan
$ fetchloom cache export bundle.tar
                                                                 # carry dataset.plan and bundle.tar
$ FETCHLOOM_CACHE_DIR=./cache2 fetchloom cache import bundle.tar
$ FETCHLOOM_CACHE_DIR=./cache2 fetchloom apply dataset.plan --offline --json
{"status":"materialized","dataset":"sample.tar.gz","tree":"blake3:7f0bdb6db1fb6d9a6377abcf8eb3cb0b5a01b00f9fa844d7f0d435e7c89a5df3","destination":"C:\\Users\\you\\flref\\data2","entries":4,"bytes":233,"work":{"bytes_read":0,"bytes_written":0,"requests":0,"file_operations":6}}
```

Same tree digest, zero requests.

## When the cache cannot be used

A cache that is missing, read-only, or out of space does not stop a run.
Fetchloom says so and continues as though `--no-cache` had been given:

```
{"event":"degrade","requested":"the cache at Q:\\no\\cache","used":"no cache, so nothing is retained","reason":"Q:\\no\\cache: The system cannot find the path specified. (os error 3)"}
```

A format mismatch does stop it, because that one you can fix now:

```
$ fetchloom cache status
cache.format_mismatch: run cache clear, because ...\cache was written in a format this build does not read and nothing is migrated
```

`cache clear` is exempt from the check, because removing a directory does not
depend on what wrote it. Clearing costs only the time to fetch again: nothing in
a cache is durable, and everything in it can be rebuilt from a lock, a bundle,
or a source.

```
$ fetchloom cache clear --yes
removed ...che
```

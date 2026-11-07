# native comparison

the [lab](benchmark-design.md) needs linux namespaces, loop mounts and `tc`, so it cannot measure windows or macos. `bench/native.py` compares two fetchloom builds on the machine it runs on, on the filesystem you point it at: ntfs or a dev drive with defender as configured, apfs, an exfat drive, or a linux filesystem. it needs python 3.9 or later and nothing else.

## what it measures

three scenarios, each from files it generates once under `ROOT/www` and serves from a local http server with keep-alive:

| scenario | files | what it stresses |
|---|---|---|
| small | 10,000 files of 4 KiB | per-file cost: cache publication, placement, defender's scan of each new file |
| zip | one stored zip of 10,000 members of 4 KiB | archive unpacking and member placement |
| large | one file, 1 GiB by default (`--large-mib`) | hashing and write throughput |

each round runs both builds in a seeded random order. for each build it starts from an empty cache and times four phases with the build's own lock:

1. cold: `sync` in a fresh project, downloading everything.
2. warm: `sync` in a second fresh project, placing everything from the cache.
3. settled: `sync` again in that project, which has nothing to do.
4. path: `path f00000` in that project.

the report pairs the builds round by round: the median of candidate/baseline time ratios with a 95% bootstrap interval (the same statistics as [ab.py](../../../bench/ab.py)). "candidate faster" means the whole interval is below 1, "candidate slower" above 1; anything else is "no clear difference".

## running it

```
python bench/native.py --baseline OLD --candidate NEW --root FOLDER [--rounds 5] [--scenarios small,zip,large] [--large-mib 1024]
```

`FOLDER` is an empty folder on the filesystem to measure; fixtures, caches and projects go there, and results go to `FOLDER/results` (`native.json` with every timing, the machine, the filesystem and the binaries' SHA-256; `native.md` with the table). use `py -3` on windows. close other heavy work while it runs, and run the same command again for a second sample before trusting a small difference.

the report records what changes the numbers most: on windows the volume's filesystem (`fsutil fsinfo volumeinfo`), whether it is a dev drive (`fsutil devdrv query`, which may need an administrator) and whether defender's real-time protection is on; on macos the mount's filesystem. measure a dev drive and an ordinary ntfs volume separately, and an exfat drive separately on either system: exfat serializes file creation.

it does not shape the network: the server is local, so it measures the work fetchloom does on the machine, not transfer over a real link. use the lab for that.

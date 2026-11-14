# Offline use and recovery

## offline

```sh
fetchloom sync --offline
fetchloom --offline path iris
```

offline, fetchloom makes no HTTP or DNS requests, and a locked project whose files are cached or already placed works as usual. `FETCHLOOM_OFFLINE=1` turns it on too, as [environment](../reference/environment.md) says.

whatever needs a source fails offline: search, and an entry the lock does not pin yet, even when you know its id or link, since listings are not cached. a link not pinned yet fails with `offline, and a new link must be downloaded once to learn its SHA-256`, unless a `get` or an `add` on this machine kept its file, which `add` then pins from the cache, with `--as` or without. `get` of a link works offline once a `get` on this machine kept its file, and with `-o`, once the file at that path still holds what the link served.

a folder [mirror](../guides/mirrors.md), such as a usb drive, still gives the pinned files the cache lacks, and so do the files already at their places in the data folder. so prepare the project online, and keep `data.toml` and `data.lock` unchanged while offline.

## recovery

a [mirror](../guides/mirrors.md) you name is asked for a pinned file before its source, so a file whose source is gone can still come from a lab's copy. if a locked download returns 404 or 410, fetchloom asks the recorded source for a new link and checks the new download against the pinned size and hashes, without rewriting the lock. when no source can give the file again, as for a plain link that died, the error's help says how to get the pinned bytes from someone who has them: they run `fetchloom push --to <folder>`, then you run `fetchloom sync --mirror <folder>`.

when one file fails, the files that downloaded stay cached. a retry continues from the bytes it kept when hashes or server validators show they belong to the same file, and otherwise starts the file over.

a failed transfer is tried 4 more times, or as many as `FETCHLOOM_RETRIES` sets, and a server's announced rate limit is respected within bounded waits. a waiting download's bar says which try comes next and when, as in `try 3 of 5 · next in 2 s`. several files download at once, and a big file whose one stream stays slow splits into parallel range requests. [environment](../reference/environment.md) lists the settings for network work and rate limits, with their thresholds.

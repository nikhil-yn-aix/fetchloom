# Tuning

Nothing here is aspirational: each command was run to produce the output
shown. This covers the seven settings that bound how a run moves and writes
bytes: `--threads`, and the six flags `get`, `plan` and `apply` share --
`--concurrency`, `--per-host`, `--bandwidth`, `--io`, `--aggressive` and
`--deterministic-io` -- plus the four environment variables that also set
them, and how `explain` reports all seven.

## The flags

| Flag | Default | Meaning |
|---|---|---|
| `--threads <n>` | measured | Ceiling on threads used for processor work. Every command takes it |
| `--concurrency <n>` | measured | Ceiling on transfers in flight across every host |
| `--per-host <n>` | measured | Ceiling on transfers in flight for one host |
| `--bandwidth <rate>` | unlimited | Ceiling on transfer rate |
| `--io <auto\|buffered\|uncached>` | `auto` | Which write path the run takes |
| `--aggressive` | off | Raise politeness ceilings |
| `--deterministic-io` | off | Disable adaptation, so two runs do identical work |

`--concurrency`, `--per-host`, `--bandwidth`, `--io`, `--aggressive` and
`--deterministic-io` belong to `get`, `plan` and `apply`. `--threads` belongs
to every command, `explain` included.

## Concurrency and per-host

`--concurrency` bounds transfers in flight across every host a run touches.
`--per-host` bounds transfers in flight for one host. Left unset, both are
measured for the run: the global ceiling comes from the thread budget clamped
to a fixed transfer ceiling, and the per-host ceiling comes from a politeness
ceiling of 4 unless a host already has a recorded measurement, in which case a
run starts from what that measurement found and lets throughput move it from
there. `--deterministic-io` turns that off; see below.

Both ceilings bound transfers actually in flight. Independent artifacts of one
manifest, and the entries of one container, are transferred at once inside them.
Ordering is unchanged by it: a run publishes and reports in manifest order
whatever order the transfers finished in, and a failing artifact stops the run at
the first failure in that order rather than at whichever thread failed first.

Measured on the many-hosts regime, sixteen objects across two hosts behind 100 ms
of injected latency per request, medians of five: settled defaults 9694 ms, a
fixed per-host ceiling of one 11058 ms, of two 9706 ms, and of four with a global
of eight 9742 ms. A ceiling of one is 14 percent slower than the rest, which is
the measurable difference between one transfer in flight per host and several.
Above two the regime stops separating them, because half its servers answer with
a rate limit and a host that asks to be left alone has its count halved back to
one whatever the ceiling above it says.

`--aggressive` removes the politeness ceiling from the per-host bound, letting
it rise to the global ceiling. It prints a warning naming the ceiling it is
raising past, because a source may answer with a rate limit or refuse the run
outright:

```
$ fetchloom --cache-dir C:\work\cache get "file://C:/work/refs/sample.tar.gz" --output C:\work\out2 --aggressive --json
--aggressive raises the transfers in flight for one host past the 4 a run holds itself to, so a source may answer with a rate limit or refuse the run outright
{"status":"materialized","dataset":"sample.tar.gz","tree":"blake3:1d0119a5d9ddce4b1490473ebdf1ed4bb734e7bc9c4f72a445fbf984f0aa0042","destination":"C:\work\out2","entries":1,"bytes":139,"work":{"bytes_read":278,"bytes_written":17,"requests":0,"file_operations":4},"trust":"tofu"}
```

## Bandwidth

`--bandwidth <rate>` is a ceiling on transfer rate. A rate is a count of bytes
per second, with an optional `k`, `m` or `g` suffix meaning a multiple of
1024. Plain digits mean bytes per second directly:

```
$ fetchloom --cache-dir C:\work\cache get "file://C:/work/refs/sample.tar.gz" --output C:\work\outB4 --bandwidth 100 --json
{"status":"materialized", ... }
```

A count with `k`, `m` or `g` is multiplied by 1024, by 1024 times 1024, or by
1024 times 1024 times 1024:

```
$ fetchloom --cache-dir C:\work\cache get "file://C:/work/refs/sample.tar.gz" --output C:\work\out1 --bandwidth 64k --json
{"status":"materialized", ... }
$ FETCHLOOM_BANDWIDTH=5m fetchloom --cache-dir C:\work\cache --json explain bandwidth
{"key":"bandwidth","value":"5242880 bytes per second","origin":"environment"}
```

Anything else is refused, including zero, a decimal count, and a suffix that
is not `k`, `m` or `g`:

```
$ fetchloom --cache-dir C:\work\cache get "file://C:/work/refs/sample.tar.gz" --output C:\work\outB --bandwidth 0 --json
error: invalid value '0' for '--bandwidth <rate>': write 0 as a count of bytes per second, with an optional k, m or g for a multiple of 1024, because a rate is read in one form and no other

$ fetchloom --cache-dir C:\work\cache get "file://C:/work/refs/sample.tar.gz" --output C:\work\outB2 --bandwidth 3.5m --json
error: invalid value '3.5m' for '--bandwidth <rate>': write 3.5m as a count of bytes per second, with an optional k, m or g for a multiple of 1024, because a rate is read in one form and no other

$ fetchloom --cache-dir C:\work\cache get "file://C:/work/refs/sample.tar.gz" --output C:\work\outB3 --bandwidth 5t --json
error: invalid value '5t' for '--bandwidth <rate>': write 5t as a count of bytes per second, with an optional k, m or g for a multiple of 1024, because a rate is read in one form and no other
```

With no `--bandwidth`, no `FETCHLOOM_BANDWIDTH`, and no `bandwidth` key in a
configuration file, the ceiling is unlimited, and `explain` reports it as `?`.

## I/O path

`--io` chooses the write path: `auto` lets the volume's own capabilities
decide, `buffered` writes through the operating system's page cache, and
`uncached` asks the operating system to release the written bytes from that
cache once they are durable. `auto` is the default.

`auto` decides from what the destination volume answered, never from the name
of the platform. It writes uncached only on a volume whose backing is local and
whose scanner answer is absent, and on a platform that can release written
pages; it writes buffered otherwise. A network volume keeps the page cache,
because there the cache is what hides the latency, and a volume a scanner
watches keeps it too, because a scanner reads the bytes straight back and
dropping them only buys a reread. `auto` never emits a `degrade`, because
`auto` asked for nothing in particular.

`--io uncached` asked for something in particular, so on a platform with no way
to release written pages it writes buffered and says so rather than silently
not doing what was asked:

```
{"event":"degrade","requested":"uncached","used":"buffered","reason":"the platform offers no way to release written pages without constraining every write to sector alignment"}
```

Windows is such a platform today: there is no per-file equivalent that does not
also require every write to be sector aligned in buffer, offset and length.
Linux releases the range with `posix_fadvise` after the flush.

## Precedence

Every one of these seven settings is resolved across the same five levels, in
this order:

1. The command line flag.
2. The environment variable.
3. The project configuration file (`fetchloom.toml`, keys `concurrency`,
   `per-host`, `bandwidth`, `io`, `threads`).
4. The user configuration file (`config.toml`, the same keys).
5. What the setting measures, or a built-in default when there is nothing to
   measure.

`--aggressive` and `--deterministic-io` are the two exceptions: neither has an
environment variable or a configuration file key. Each is only ever the
command line flag or the built-in default of off.

A level supplying a value this build cannot parse is a hard error naming the
level and the setting, rather than falling through to the next level:

```
$ FETCHLOOM_BANDWIDTH=bogus fetchloom --cache-dir C:\work\cache explain bandwidth
bandwidth from the environment is not a value bandwidth takes: set FETCHLOOM_BANDWIDTH to a value bandwidth takes, or unset it
```

A project configuration file overrides a user one, and an environment variable
overrides both:

```
$ cat fetchloom.toml
concurrency = 5
per-host = 3
bandwidth = "2m"
io = "buffered"
$ fetchloom --cache-dir C:\work\cache explain
project config: C:\work\project\fetchloom.toml
user config: none found
offline = false (default)
threads = 16 (measured)
display = plain (default)
cache.dir = C:\work\cache (command line)
concurrency = 5 (project config)
per-host = 3 (project config)
bandwidth = 2097152 bytes per second (project config)
io = buffered (project config)
aggressive = false (default)
deterministic-io = false (default)
log = info (default)
retries = 5 (default)
timeout = 30s (default)
sources = none configured (default)
color = auto (default)
hints = true (default)

$ FETCHLOOM_CONCURRENCY=7 fetchloom --cache-dir C:\work\cache explain concurrency
concurrency = 7 (environment)
```

`threads` follows the same order and additionally clamps a requested count
down to what the platform detected, naming both numbers:

```
$ fetchloom --threads 99999 --cache-dir C:\work\cache explain threads
threads = 16 (command line, clamped from 99999 to the 16 this machine detected)
```

`explain` itself takes no `--concurrency`, `--per-host`, `--bandwidth`, `--io`,
`--aggressive` or `--deterministic-io` flag; those six belong to `get`, `plan`
and `apply` only:

```
$ fetchloom --cache-dir C:\work\cache explain --concurrency 3
error: unexpected argument '--concurrency' found

  tip: to pass '--concurrency' as a value, use '-- --concurrency'

Usage: fetchloom.exe explain [OPTIONS] [KEY]

For more information, try '--help'.
```

What `explain` reports for those six therefore comes from the environment, a
configuration file, a measurement, or the default -- never from a flag on the
`explain` invocation itself.

## Environment variables

| Variable | Sets |
|---|---|
| `FETCHLOOM_CONCURRENCY` | `--concurrency` |
| `FETCHLOOM_PER_HOST` | `--per-host` |
| `FETCHLOOM_BANDWIDTH` | `--bandwidth` |
| `FETCHLOOM_THREADS` | `--threads` |
| `FETCHLOOM_LOG` | the log level, which `--verbose` raises |
| `FETCHLOOM_OFFLINE` | `--offline`, when set to `1` |
| `FETCHLOOM_CACHE_DIR` | `--cache-dir` |
| `FETCHLOOM_CONFIG` | `--config` |

Each takes exactly what its flag takes: a count for the concurrency pair and
for threads, a rate in `--bandwidth`'s syntax, and one of `error`, `info` or
`debug` for the log level.

`--retries` and `--timeout` have no variable of their own; set them on the
command line or in configuration.

## What `explain` reports

`explain` reports the effective value of `concurrency`, `per-host`,
`bandwidth`, `io`, `aggressive` and `deterministic-io`, alongside `offline`,
`threads`, `display`, `cache.dir`, `log`, `retries`, `timeout`, `sources`,
`color` and `hints`, sixteen settings in total. A tuning setting
no level supplied is reported as `measured`, with what was measured and when.

For `concurrency`, the measurement is the global ceiling this run computed
from the thread budget and the politeness ceiling. For `per-host`, it is that
same ceiling unless a host already has a recorded measurement in the cache, in
which case the description names every recorded host:

```
$ fetchloom --cache-dir C:\work\netcache get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --output C:\work\outnet --json
{"status":"materialized","dataset":"hello-2.12.tar.gz", ... }
$ fetchloom --cache-dir C:\work\netcache --json explain per-host
{"key":"per-host","value":"4","origin":"measured","measurement":{"found":"4","taken":"recorded per host: ftp.gnu.org 2 in flight at 87066 bytes per second, 11688 ms to first byte, taken 2026-09-02T10:45:04Z"}}
```

The measurement's description comes from `HostMeasurement::describe`: the
host, the transfers in flight the run settled on, the fastest sustained
throughput in bytes per second, the time to first byte in milliseconds, and
when it was taken. The record itself lives under `<cache>/meta/host` as one
file per host, keyed by a hash of the host name:

```
$ cat C:\work\netcache\meta\host\d998ca432457be5dd2b556dbb9e70d53083fcf3c5b075b5b243da71ea9599cde
{"host":"ftp.gnu.org","measurement":{"concurrency":2,"throughput":87066,"time_to_first_byte_ms":11688,"observed_at":"2026-09-02T10:45:04Z"}}
```

The `per-host` value itself, `4`, is always the ceiling a run would use, not
the recorded concurrency; the recorded measurement only ever appears in the
description of where that ceiling's number was taken from, once a host has
one.

`bandwidth`, `aggressive` and `deterministic-io` are never `measured`.
Unsupplied, `bandwidth` reports as `?` with origin `default`, meaning
unlimited; `aggressive` and `deterministic-io` report as `false` with origin
`default`. None of the three has a measurement behind its default.

## Deterministic I/O

`--deterministic-io` turns off two things a run otherwise does by default:
starting a host's transfer concurrency from what the cache last recorded for
it, and recording a new measurement for that host once the run ends. With it
set, every host starts at the fixed per-host ceiling and no measurement file
under `<cache>/meta/host` is written or updated, so two runs against the same
inputs make the same concurrency decisions regardless of what an earlier run
measured.

# Getting started

## Install

There are no published binaries yet, so you build from source. You need a Rust
toolchain; the repository pins the version it is built with, and `rustup` reads
that pin for you.

```
git clone <this repository>
cd fetchloom
cargo build --release
```

The binary is at `target/release/fetchloom` (`target\release\fetchloom.exe` on
Windows). Put it somewhere on your `PATH`, or run it from there.

```
$ fetchloom --version
fetchloom 0.1.0
```

Windows and Linux are supported. There is no macOS build.

Nothing else is needed. Fetchloom writes no configuration on first run, asks you
nothing, and creates its cache the first time it has something to put in it.

## A first command

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --output hello
```

That fetches the tarball, checks the bytes, extracts it into `hello/`, and
leaves a `fetchloom.lock` beside you. Add `--json` and you get the result on
standard output instead of a progress line:

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --output hello --json
{"status":"materialized","dataset":"hello-2.12.tar.gz","tree":"blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec","destination":"C:\\Users\\you\\hello","entries":462,"bytes":1017723,"work":{"bytes_read":0,"bytes_written":1017723,"requests":2,"file_operations":496}}
```

`tree` is the digest of the directory that was produced: 462 entries that hash
to that one value on Windows and on Linux alike. `work` is what the run did, in
counts rather than seconds, and it is the same on every machine for the same
input.

Beside you is now a `fetchloom.lock` naming what the URL resolved to:

```yaml
datasets:
  hello-2.12.tar.gz:
    artifacts:
      hello-2.12.tar.gz:
        digest: "blake3:ce20127416c48b9e6a7025ea9e7ced637802b6c96262aa59e6ebb7e673a00374"
        interop: "sha256:cf04af86dc085268c5f4470fbae49b18afbc221b78096aab842d934a76bad0ab"
        layout: "keep"
        size: 1017723
    manifest: "blake3:384b61a874779b9f86fac4c6aacc45ff0e16f3b59d6c608d9ef0d17f8afcd631"
    tree: "blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec"
```

Commit that file. From then on `--locked` holds every run to it, on any machine,
and a source that starts serving different bytes fails instead of quietly giving
you something else.

## A local example you can run offline

Everything below runs with no network. Make a small archive:

```
$ mkdir -p src/docs
$ printf 'alpha\n' > src/a.txt
$ printf 'beta\n'  > src/docs/b.txt
$ printf '#!/bin/sh\necho hi\n' > src/run.sh
$ tar -czf sample.tar.gz -C src .
```

Fetch it:

```
$ fetchloom get file://$PWD/sample.tar.gz --output data --json
{"status":"materialized","dataset":"sample.tar.gz","tree":"blake3:9cd1f02513d1c38dc46bb4d965127f9da74a82eac42276f9c35f091d7e03fce7","destination":"C:\\Users\\you\\flgo\\data","entries":4,"bytes":234,"work":{"bytes_read":591,"bytes_written":700,"requests":0,"file_operations":29},"trust":"tofu"}

$ ls data
a.txt  docs  run.sh
```

On Windows the same reference is written `file:///C:/path/to/sample.tar.gz`.

Run it again and nothing is written:

```
$ fetchloom get file://$PWD/sample.tar.gz --output data --json
{"status":"unchanged", ... ,"work":{"bytes_read":234,"bytes_written":0,"requests":0,"file_operations":1}}
```

Change a file in the destination and the next run stops rather than overwriting
it:

```
$ echo tampered >> data/a.txt
$ fetchloom get file://$PWD/sample.tar.gz --output data
destination.modified: run again with --force to overwrite the modified entries and remove the foreign ones, or --adopt to accept the destination as it stands: a.txt (modified)
$ echo $?
60
```

`--force` rewrites it, `--adopt` accepts the destination as it now stands and
reports that tree instead. Neither is ever implied.

## What just happened

The first run wrote three things:

- `data/`, the destination. This is yours. Fetchloom reconciles it and never
  clobbers it.
- `fetchloom.lock`, beside your working directory. It contains nothing about
  this machine.
- A receipt inside the cache, keyed by the destination's path. It records what
  this run did here, and it never leaves this machine.

The cache itself holds the archive object, addressed by its digest, so a second
project fetching the same archive transfers nothing. See [cache.md](cache.md).

## Where to go next

- [commands.md](commands.md) for everything the binary does.
- [references.md](references.md) for the ways to name data.
- [errors.md](errors.md) when a run stops.

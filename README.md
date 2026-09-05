# Fetchloom

Point it at a dataset. It gives you the exact files and a record proving what it gave you.

```
fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz
```

That resolves the reference, reuses anything it already has, downloads the rest, checks every byte against a digest, unpacks it, and writes a lock file. Run it again and it does nothing, because everything is already correct and it can prove it.

No account, no daemon, no config to write first, no Python.

## Install

No installer yet. Build it:

```
cargo build --release
```

The binary lands in `target/release/`. It needs nothing else at runtime.

## What a run looks like

Fetch and unpack an archive:

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz
hello-2.12  materialized  1.0 MB  tofu
```

See what would happen without moving bytes:

```
$ fetchloom plan https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz
dataset: hello-2.12
network: {hosts: [ftp.gnu.org], required: true}
disk: {cache: {volume: C, bytes: 1017723}}
trust: tofu
```

Check that a directory still holds what was written into it:

```
$ fetchloom verify ./hello-2.12
hello-2.12  unchanged  blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39b
```

## What it promises

Same lock, same bytes, on Windows and Linux, or it fails and names the exact entries that differ. It never quietly hands you something else.

Nothing appears half written. Kill it mid run and your directory holds the old tree or the new one, never a mixture.

Nothing degrades in silence. Every time it wanted to do something and could not, it says what it wanted, what it did instead, and why.

Every result carries a trust class with a mechanical definition. Sizes, timestamps and ETags are never treated as proof of content.

## What it does not do

It does not clean, convert, or run anything on your data. No manifest key can express a command.

It does not scrape. It lists what you point it at and never wanders outside that prefix.

It is slower than `cp` and `curl`, on every workload, on purpose. It hashes every byte twice, builds a tree so damage can be located later, publishes through staging, and writes a record. Those tools do none of that and cannot tell you whether they succeeded. Numbers are in [docs/internals.md](docs/internals.md).

## Not built yet

There is no installer, no signed release, and no package manager entry. You build from source.

It speaks HTTP/1.1 only. No FTP, no SFTP yet.

There is no way to keep local edits to a fetched dataset without either losing them or losing the link to upstream.

## Docs

[Guide](docs/guide.md), start here.
[Reference](docs/reference.md), every command, flag, variable and key.
[Contracts](docs/contracts.md), exactly what is promised and what each failure means.
[Internals](docs/internals.md), how it works and why it is built this way.

Apache-2.0.

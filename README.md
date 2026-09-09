```
  __       _         _      _
 / _| ___ | |_  ___ | |__  | |  ___    ___   _ __ ___
| |_ / _ \| __|/ __|| '_ \ | | / _ \  / _ \ | '_ ` _ \
|  _|  __/| |_| (__ | | | || || (_) || (_) || | | | | |
|_|  \___| \__|\___||_| |_||_| \___/  \___/ |_| |_| |_|
```

point it at a dataset. get back the exact files, and a record that proves what you got.

[![verify](https://github.com/nikhil-yn-aix/fetchloom/actions/workflows/verify.yml/badge.svg)](https://github.com/nikhil-yn-aix/fetchloom/actions/workflows/verify.yml)
[![hosts](https://github.com/nikhil-yn-aix/fetchloom/actions/workflows/hosts.yml/badge.svg)](https://github.com/nikhil-yn-aix/fetchloom/actions/workflows/hosts.yml)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

## what a run looks like

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz -o hello
blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec  462 entries  C:\work\hello
```

that resolved the URL, downloaded 1,017,723 bytes, hashed every one of them with
BLAKE3 and SHA-256 in the same pass, unpacked 462 entries, and wrote
`fetchloom.lock` beside the directory. the line it printed is the digest of the
whole tree, so two machines that print the same line hold the same bytes.

run it again and it prints the same line and writes nothing. it does not trust a
timestamp to know that: it can hash the directory and compare.

no account, no daemon, no configuration to write first, no Python.

## install

there is no release build and no installer yet. build it:

```
git clone https://github.com/nikhil-yn-aix/fetchloom
cd fetchloom
cargo build --release
```

`rust-toolchain.toml` pins Rust 1.98.0, and rustup installs it on the first build.
the binary lands in `target/release/` and needs nothing else at runtime. Windows
and Linux, x86_64 and aarch64.

## what people use it for

| | |
|---|---|
| `fetchloom get https://host/x.tar.zst` | one file, verified, unpacked, recorded |
| `fetchloom get ham10000` | a bare name, searched across eight registries at once |
| `fetchloom get hf:datasets/org/name@rev` | a provider record, by its own identifier |
| `fetchloom get --locked` | every dataset the project file names, pinned to the lock |
| `fetchloom probe <ref>` | size, digests, ranges and trust, moving no payload bytes |
| `fetchloom status <dir>` | which entries you changed since the run that wrote them |

fetch a directory, edit part of it, and run again when upstream moves: it merges
per entry, keeps your edits, and writes upstream's version beside yours where
both changed. [the guide](docs/guide.md) starts there.

## why it exists

most public datasets publish no checksum. the reference truth is your own first
successful fetch, and nothing records it. fetchloom records it in a lock and
checks every later run against it, the way `Cargo.lock` does for code.

what it refuses to do is the interesting half.

nothing enters the cache unverified. an object is hashed whole before it is
named, and there is no other way for one to appear.

a digest is checked before anything is published. a mismatch fails the run and
leaves the destination as it was.

a credential never reaches a host it was not resolved for. it is dropped on any
redirect to a different host, and a password is never sent over a control
connection that could not be encrypted.

a source that says come back later is left alone. `Retry-After` is a floor on
the wait, never a ceiling, and a wait longer than the limit hands the object to
the next candidate rather than hammering the host.

nothing degrades in silence. every time it wanted one thing and did another it
says what it wanted, what it did, and why.

## where to go next

[guide](docs/guide.md), the tasks, in the order you hit them.
[reference](docs/reference.md), every command, flag, variable and key.
[contracts](docs/contracts.md), exactly what is promised and what each failure means.
[internals](docs/internals.md), how it works, and the measurements behind it.
[contributing](.github/CONTRIBUTING.md), how code is written here.

## license

MIT. see [LICENSE](LICENSE).

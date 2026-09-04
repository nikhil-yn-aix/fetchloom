# Fetchloom

Fetchloom turns a dataset reference into exact, verified local files, and proves
what it delivered. One command takes a URL, a manifest, or a name, transfers only
what is missing, checks every byte, extracts it safely, and writes down a digest
of the directory it produced so any machine can confirm it got the same thing.

Windows and Linux. One static binary. No daemon, no telemetry, no configuration
before your first fetch.

## Install

There are no published binaries yet. `rust-toolchain.toml` pins the compiler, so
`rustup` fetches the right one for you.

```
git clone https://github.com/nikhil-yn-aix/fetchloom
cd fetchloom
cargo build --release
```

The binary is `target/release/fetchloom` (`fetchloom.exe` on Windows).

## Three runs

Fetch a tarball. Fetchloom transfers it, verifies it, extracts it, and writes
`fetchloom.lock` beside you.

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --output hello --json
{"status":"materialized","dataset":"hello-2.12.tar.gz","tree":"blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec","destination":"C:\\Users\\Naveen\\AppData\\Local\\Temp\\fl-readme\\hello","entries":462,"bytes":4539480,"work":{"bytes_read":0,"bytes_written":6574998,"requests":2,"file_operations":500},"trust":"tofu"}
```

`tree` is the digest of the directory that was produced. The same 462 entries
hash to that one value on Windows and on Linux alike. `work` is what the run did,
in counts rather than seconds, so it is the same on every machine for the same
input.

Run it again and nothing is transferred and nothing is written.

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --output hello --json
{"status":"unchanged","dataset":"hello-2.12.tar.gz","tree":"blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec","destination":"C:\\Users\\Naveen\\AppData\\Local\\Temp\\fl-readme\\hello","entries":462,"bytes":4539480,"work":{"bytes_read":1017723,"bytes_written":0,"requests":1,"file_operations":3},"trust":"tofu"}
```

The destination is yours. Change a file in it and the next run stops rather than
overwriting your work.

```
$ echo tampered >> hello/hello-2.12/README
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --output hello
run again with --force to overwrite the modified entries and remove the foreign ones, or --adopt to accept the destination as it stands: hello-2.12/README (modified)
destination.modified, exit 60
```

`fetchloom verify` says the same thing about a directory on its own, and names
both digests:

```
$ fetchloom verify hello --json
{"kind":"integrity.mismatch","layer":"verify","dataset":null,"artifact":null,"source":null,"attempts":0,"retryable":false,"next_action":"fetch C:\\Users\\Naveen\\AppData\\Local\\Temp\\fl-readme\\hello again, because it now holds blake3:ad2929443cd39a04d155c86916aed8cc492b7d7bc00214331c0e0dc645663de1 where the run that wrote it reported blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec"}
```

Commit `fetchloom.lock` and `--locked` holds every later run to exactly those
digests, on any machine. A source that starts serving different bytes fails
instead of quietly giving you something else.

## Trust classes

Every artifact is labelled with one of four classes. Each is a statement about
what was compared, never a judgment about whether the data is good.

| Class | What it means |
|---|---|
| `verified` | The bytes hashed to a digest written down before the run started, in a manifest or in the lock |
| `corroborated` | Nothing said in advance what the digest should be, and at least two independent recorded observations agree with what was seen |
| `tofu` | Nothing said in advance and nothing corroborates it. What was seen is written down for next time |
| `unverified` | The content could not be hashed, or you turned reuse verification off |

Bytes are always hashed as they arrive, in the same pass that writes them, and
nothing enters the cache until the digest matches. `--verify` governs reuse, not
transfer. Weak classes are never reached by falling through quietly:
[trust.md](docs/reference/trust.md) has the rules.

## Documentation

- [getting-started.md](docs/reference/getting-started.md) walks the first fetch,
  including an example that needs no network.
- [commands.md](docs/reference/commands.md) is every command and flag, each shown
  with output from a real run.
- [references.md](docs/reference/references.md) is every way to name data.
- [errors.md](docs/reference/errors.md) is every error kind and its exit code.
- [files.md](docs/reference/files.md) is the manifest, the lock, the receipt and
  the plan.
- [cache.md](docs/reference/cache.md), [credentials.md](docs/reference/credentials.md),
  [tuning.md](docs/reference/tuning.md) for the rest.

[docs/features.md](docs/features.md) states what the build does, feature by
feature, and marks what is not built yet.
[docs/contracts.md](docs/contracts.md) is the exact behavior.
[CONTRIBUTING.md](CONTRIBUTING.md) is how to build and verify it.

## Status

Pre-1.0 and unreleased. The formats are not frozen and the version promises
nothing until 1.0. Packaging, signing and package manager entries are not built;
[docs/roadmap.md](docs/roadmap.md) owns that.

Apache-2.0. See [LICENSE](LICENSE) and [SECURITY.md](SECURITY.md).

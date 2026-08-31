# Commands

Every command and flag the binary carries. Nothing here is aspirational: each
was run to produce the output shown. `fetchloom --help` lists the same surface.

## Flags every command takes

| Flag | Meaning |
|---|---|
| `--config <path>` | Read this configuration file and search for no other |
| `--no-config` | Read no configuration file at all |
| `--cache-dir <path>` | Put the cache here |
| `--offline` | Refuse every network activity, before anything is attempted |
| `--json` | Write the result to standard output as one JSON object |
| `--events <path\|->` | Write the event stream as newline-delimited JSON |
| `--quiet` | Print no progress |
| `--display <plain\|live\|none>` | How progress is presented |
| `--no-animation` | Do not redraw progress in place |
| `--threads <n>` | Ceiling on threads used for processor work |
| `--yes` | Answer every confirmation with yes |

A display mode never changes bytes, digests, exit codes or the JSON result. When
the terminal cannot carry the one you asked for, the run says so rather than
quietly switching:

```
{"event":"degrade","requested":"the live view","used":"the none view","reason":"the standard error stream is not a terminal"}
```

## get

```
fetchloom get <ref> [--output <path>] [selection] [policy]
```

Resolves the reference, transfers what is missing, verifies it, materializes it
into a destination directory, and records what it did. This build takes one
reference per run.

```
$ fetchloom get file://$PWD/sample.tar.gz --output data --json
{"status":"materialized","dataset":"sample.tar.gz","tree":"blake3:7f0bdb6db1fb6d9a6377abcf8eb3cb0b5a01b00f9fa844d7f0d435e7c89a5df3","destination":"C:\\Users\\you\\flgo\\data","entries":4,"bytes":234,"work":{"bytes_read":468,"bytes_written":234,"requests":0,"file_operations":27}}
```

`status` is one of four words. `materialized` means the destination did not
exist and was built. `unchanged` means it already held everything and nothing
was written. `restored` means only the missing entries were written. `adopted`
means `--adopt` was given and the reported tree is the one already on disk.

| Flag | Default | Meaning |
|---|---|---|
| `-o`, `--output <path>` | `./<name>` | Where the destination directory goes |
| `--select <glob>` | everything | Include matching members. Repeatable |
| `--exclude <glob>` | nothing | Remove from the included set, after all includes |
| `--layout <keep\|flatten:n>` | `keep` | Preserve archive paths, or drop the first n components |
| `--no-extract` | off | Keep a recognized archive as one file |
| `--lock <path>` | `./fetchloom.lock` | Where the lock is read and written |
| `--locked` | off | Fail if anything differs from the lock |
| `--no-cache` | off | Retain no object; the transfer lives beside the destination |
| `--verify <always\|fingerprint\|never>` | `fingerprint` | What a cache hit is checked against before reuse |
| `--force` | off | Overwrite modified destination entries and remove foreign ones |
| `--adopt` | off | Accept the destination as it stands and report that tree |
| `--durability <strict\|normal\|fast>` | `normal` | How far a write is pushed before publication |

Selection is part of identity: changing `--select` or `--layout` changes what
the lock records, not the dataset's name.

```
$ fetchloom get file://$PWD/sample.tar.gz --output s1 --select 'docs/**' --json
{"status":"materialized", ... ,"tree":"blake3:4fe077be5e39abdfb45be30260a9144ef60e50ad157f8d0e2c2e41533ed468f9","entries":2, ... }
$ find s1 -type f
s1/docs/b.txt
```

A pattern that matches nothing is an error rather than an empty result:

```
$ fetchloom get file://$PWD/sample.tar.gz --output s5 --select 'nothing/**' --json
{"kind":"reference.unresolved", ... ,"next_action":"select a pattern that matches, because none of the 4 members matched nothing/**"}
```

`--layout flatten:n` drops the first n path components. A directory left with no
path is the destination itself and is dropped; a file left with no path fails
with `destination.unrepresentable` naming the member and the count.

```
$ fetchloom get ./deep.tar.gz --output d2 --layout flatten:1 --json
{"status":"materialized","dataset":"deep.tar.gz","tree":"blake3:f9995ab0c39285d6fdfa6ef386a235b0716a185c846c61f910d50279b3272a27","entries":3, ... }
$ find d2
d2  d2/README  d2/src  d2/src/m.txt
```

`--verify` decides what a run trusts without reading, both for a cached object
and for a destination entry. Under `fingerprint`, the default, a file whose
recorded volume, file identifier, size and two timestamps still match is taken
as unchanged and its bytes are not read. Under `always` every byte is reread.
Under `never` nothing is checked and the result is `unverified`. Measured on the
same unchanged 1 MiB tree:

```
--verify fingerprint   bytes_read 1048576   (the source only)
--verify always        bytes_read 2097152   (the source and the destination)
--verify never         bytes_read 1048576
```

`--durability strict` flushes the file and its directory to the device before
publication, `normal` flushes the file, `fast` flushes nothing and relies on the
atomic rename alone. `fast` can lose a completed object to a power failure. It
cannot leave a torn one.

## plan

```
fetchloom plan <ref> [--output <path>] [--lock <path>]
```

Resolves the reference and writes a plan to standard output, moving no bytes.
The plan's digests come from the lock, so a reference the lock pins nothing for
cannot be planned. Run `get` once first.

```
$ fetchloom plan file://$PWD/sample.tar.gz --output data2 > dataset.plan
$ cat dataset.plan
artifacts:
  -
    cached: true
    digest: "blake3:20c467cfd6cb266f9989b52976de9ba69d612debad82efb15608d67e3d3207d7"
    id: "sample.tar.gz"
    layout: "keep"
    size: 233
    source: "file:///C:/Users/you/flref/sample.tar.gz"
conflicts: []
credentials: []
dataset: "sample.tar.gz"
destination: "C:\\Users\\you\\flref\\data2"
disk:
  cache:
    bytes: 0
    volume: "C:"
  destination:
    bytes: 0
    volume: "C:"
  partial:
    bytes: 0
    volume: "C:"
  staging:
    bytes: 0
    volume: "C:"
network:
  hosts: []
  required: false
terms: []
trust: "verified"
unknown:
  - "expanded"
```

`unknown` lists what the source could not tell it. A field there is never turned
into a number. `cached`, `disk`, `conflicts` and `destination` describe the
machine the plan was made on; `apply` reports them and acts on none of them.

`plan` accepts `get`'s flags. `--force` and `--adopt` are among them and have
nothing to act on, because a plan writes no destination.

## apply

```
fetchloom apply <plan> [--output <path>] [--offline]
```

Executes a plan. It re-resolves the digests the plan states, so a source now
serving different bytes fails on integrity rather than falling back. `--output`
names the destination when the plan's own is not this machine's.

```
$ fetchloom apply dataset.plan --offline --json
{"status":"materialized","dataset":"sample.tar.gz","tree":"blake3:7f0bdb6db1fb6d9a6377abcf8eb3cb0b5a01b00f9fa844d7f0d435e7c89a5df3","destination":"C:\\Users\\you\\flref\\data2","entries":4,"bytes":233,"work":{"bytes_read":0,"bytes_written":0,"requests":0,"file_operations":6}}
```

That run reached no network and issued no request, because `cache import` had
already put the object it needed in the cache. See [cache.md](cache.md) for the
whole plan-and-carry sequence.

## verify

```
fetchloom verify <path>
```

Recomputes the tree digest of a directory and compares it to the receipt the run
that wrote it left behind.

```
$ fetchloom verify out3 --json
{"entries":2,"path":"C:\\Users\\you\\flman\\out3","status":"verified","tree":"blake3:cc6d1e52b3589084fb184dffd1ec06e79d9176e193ba43f2103c800ede49dc87"}
```

A tree that no longer matches its receipt fails, and says both digests:

```
$ echo tampered >> out3/one
$ fetchloom verify out3 --json
{"kind":"integrity.mismatch","layer":"verify", ... ,"next_action":"fetch ...\\out3 again, because it now holds blake3:21659c9076455da14c944d262c46da17ae7125dcbb682a0d9a76db3cc2355d4d where the run that wrote it reported blake3:cc6d1e52b3589084fb184dffd1ec06e79d9176e193ba43f2103c800ede49dc87"}
$ echo $?
30
```

The receipt supplies the mode of each file, which a filesystem does not state.
It never supplies a digest: a record attesting to its own correctness proves
nothing, so the bytes are always reread. Without a receipt every file is treated
as `0644`, the run says so with a `degrade` event, and nothing is compared.

## repair

```
fetchloom repair <ref>
```

Restores a cached object the cache holds damaged bytes for. It finds which byte
ranges of the object fail against the stored chunk tree and fetches only those.
The reference is resolved exactly as `get` resolves it, so an object this cache
has never fetched cannot be repaired.

After the ranges are written the whole object is reread and hashed, and it
enters the cache only if it hashes to the digest it is named by. A repair whose
localization was wrong fails loudly rather than publishing bytes nothing checked
whole. An object with no stored chunk tree, one whose tree does not check out,
one with too many separate damaged spans, or one damaged over half its length is
fetched whole, with a `degrade` event naming why.

Only objects above 64 MiB carry a chunk tree. Below that the content digest
already authenticates the object whole, and there is nothing to localize.

In this build the reference has to be one the cache fetched over the network. A
`file:` reference, and a manifest, leave no record of what they resolved to, so
`repair` cannot find the digest even when the cache holds the damaged object:

```
$ fetchloom repair file://$PWD/big.bin --json
{"kind":"reference.unresolved", ... ,"next_action":"run get on file:///.../big.bin first, because repair puts right an object this cache already holds and this one has never been fetched here"}
```

`fetchloom cache verify` still finds and quarantines that object, and
`fetchloom cache repair` still rebuilds what it can from what is there.

## cache

```
fetchloom cache <status|ls|verify|pin|unpin|export|import|repair|prune|clear>
```

See [cache.md](cache.md), which documents each subcommand with a run.

## explain

```
fetchloom explain [key]
```

Reports the effective settings and the level each came from.

```
$ fetchloom explain
project config: none found
user config: none found
offline = false (default)
threads = ? (default)
display = plain (default)
cache.dir = C:/Users/you/flman/cache (environment)

$ fetchloom --threads 3 explain threads
threads = 3 (command line)
```

This build reports those six settings and no others, and `--json` has no effect
on it. `?` means no level named a value; for `threads` that means the pool is
sized from the hardware at the moment it is built.

## completions

```
fetchloom completions <bash|elvish|fish|powershell|zsh>
```

Writes a completion script to standard output. Redirect it where your shell
looks:

```
$ fetchloom completions bash > /etc/bash_completion.d/fetchloom
$ fetchloom completions powershell > fetchloom.ps1
```

## What is not here yet

`init`, `watch`, `doctor` and `why` are part of the finished command surface and
are not in this binary. Neither are `--verbose`, `--color`, `--no-hints`,
`--concurrency`, `--per-host`, `--bandwidth`, `--retries`, `--timeout`, `--io`,
`--aggressive`, or `--deterministic-io`. Nothing that cannot perform what it
promises is present, so their absence is the honest answer rather than a stub
that accepts the flag and ignores it.

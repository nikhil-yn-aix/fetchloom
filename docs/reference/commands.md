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

`--threads` is one of the settings [tuning.md](tuning.md) covers in depth,
including the environment variable and the measured default.

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
{"status":"materialized","dataset":"sample.tar.gz","tree":"blake3:9cd1f02513d1c38dc46bb4d965127f9da74a82eac42276f9c35f091d7e03fce7","destination":"C:\\Users\\you\\flgo\\data","entries":4,"bytes":234,"work":{"bytes_read":591,"bytes_written":700,"requests":0,"file_operations":29},"trust":"tofu"}
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
| `--concurrency <n>` | measured | Ceiling on transfers in flight across every host |
| `--per-host <n>` | measured | Ceiling on transfers in flight for one host |
| `--bandwidth <rate>` | unlimited | Ceiling on transfer rate |
| `--io <auto\|buffered\|uncached>` | `auto` | Which write path the run takes |
| `--aggressive` | off | Raise politeness ceilings |
| `--deterministic-io` | off | Disable adaptation, so two runs do identical work |

`get`, `plan` and `apply` share these six. See [tuning.md](tuning.md) for the
syntax of `--bandwidth`, what each one does, the four environment variables
that also set them, and the precedence between command line, environment,
configuration file and measurement.

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

### Which source is taken

An artifact naming several sources is probed rather than raced. Each candidate
gets one bounded metadata request, made against all of them at once, and up to
four candidates are probed however many the manifest names. The candidates are
then scored on a fixed order of readings and the run says which it took and why:

```
{"event":"source.probe","source":"http://127.0.0.1:5001/object"}
{"event":"source.probe","source":"http://127.0.0.1:5002/object"}
{"event":"source.selected","source":"http://127.0.0.1:5002/object","reason":"it serves part of an object and the alternatives do not"}
```

The reason is one of a fixed set, and the last of them is manifest order: when
nothing measured separates two candidates the earlier one in the manifest is
taken, and the run says `nothing measured separated the candidates, so the
manifest did`. A manifest naming one source is not probed at all and the reason
is `the manifest named one source`. The reason is recorded in the receipt as
`source_reason`.

A candidate that refused the run for want of a credential, whose host has been
measured faster than the one that was taken, produces a `credential.offer`
naming the projected saving. A run that cannot ask declines it and says so.

### When one object is split

One object is fetched as several ranges at once only when four conditions all
hold: the object is larger than 64 MiB, the source states an identity that
cannot change under the same name, the source serves ranges, and this run has
measured the host as serving more with more streams. A split that was wanted and
refused emits a `degrade` naming the condition that failed:

```
{"event":"degrade","requested":"one object fetched as several ranges at once","used":"the object fetched whole, in one stream","reason":"the source states no identity that cannot change under the same name"}
```

Over plain HTTP that last reason is the usual one: an entity tag is a strong
validator, not an immutable identity, so only the object store adapter reaches a
split today.

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
    digest: "blake3:d5ffe72da208c4d7eecd7ebe1167426c7ea4b2c8a22135f984c904d9780c967d"
    id: "sample.tar.gz"
    layout: "keep"
    size: 197
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
    volume: "C:"
  partial:
    bytes: 0
    volume: "C:"
  staging:
    volume: "C:"
network:
  hosts: []
  required: false
terms: []
trust: "verified"
unknown:
  - "expanded"
  - "staging"
  - "destination"
  - "cost"

```

`cost` is in `unknown` because no source in this build states who is billed for
its bytes; when one does, an artifact carries `egress_charged` and
`requester_pays`, and never a sum of money. See [files.md](files.md) for what
each plan field means.

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
{"status":"materialized","dataset":"sample.tar.gz","tree":"blake3:9cd1f02513d1c38dc46bb4d965127f9da74a82eac42276f9c35f091d7e03fce7","destination":"C:\\Users\\you\\flref\\data2","entries":4,"bytes":234,"work":{"bytes_read":197,"bytes_written":234,"requests":0,"file_operations":7},"trust":"tofu"}
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
{"entries":2,"path":"C:\\Users\\you\\flman\\out3","status":"verified","tree":"blake3:a57d80c2c45d8758434b86dad1d4f2733f3ce2950d35ef7d090ac0c951786964"}
```

A tree that no longer matches its receipt fails, and says both digests:

```
$ echo tampered >> out3/one
$ fetchloom verify out3 --json
{"kind":"integrity.mismatch","layer":"verify", ... ,"next_action":"fetch ...\\out3 again, because it now holds blake3:cb7d8739486d77a44b2835d23dd582dfd1f769df3bd53c3274ccb7c0d8c06549 where the run that wrote it reported blake3:a57d80c2c45d8758434b86dad1d4f2733f3ce2950d35ef7d090ac0c951786964"}
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
threads = 16 (measured)
display = plain (default)
cache.dir = C:\Users\you\AppData\Local\Fetchloom\Cache (default)
concurrency = 8 (measured)
per-host = 4 (measured)
bandwidth = ? (default)
io = auto (default)
aggressive = false (default)
deterministic-io = false (default)
log = info (default)
retries = 5 (default)
timeout = 30s (default)
sources = none configured (default)
color = auto (default)
hints = true (default)

$ fetchloom --threads 3 explain threads
threads = 3 (command line)
```

`measured` means no level named a value and the number was taken from this
machine or this cache. See [tuning.md](tuning.md) for what each of the six
tuning settings means and what its measurement is.

Under `--json` the same answer is one object, with the config files it found and
every setting:

```
$ fetchloom --json explain
{"files":[{"level":"project","path":null},{"level":"user","path":null}],
 "settings":[{"key":"offline","value":"false","origin":"default"},
             {"key":"threads","value":"16","origin":"measured","measurement":{"found":"16","taken":"this run, from the platform"}},
             {"key":"display","value":"plain","origin":"default"},
             {"key":"cache.dir","value":"C:\\Users\\you\\AppData\\Local\\Fetchloom\\Cache","origin":"default"},
             {"key":"concurrency","value":"8","origin":"measured","measurement":{"found":"8","taken":"this run, from the thread budget and the politeness ceiling"}},
             {"key":"per-host","value":"4","origin":"measured","measurement":{"found":"4","taken":"this run, from the politeness ceiling, because no host has a recorded measurement"}},
             {"key":"bandwidth","value":"?","origin":"default"},
             {"key":"io","value":"auto","origin":"default"},
             {"key":"aggressive","value":"false","origin":"default"},
             {"key":"deterministic-io","value":"false","origin":"default"},
             {"key":"log","value":"info","origin":"default"},
             {"key":"retries","value":"5","origin":"default"},
             {"key":"timeout","value":"30s","origin":"default"},
             {"key":"sources","value":"none configured","origin":"default"},
             {"key":"color","value":"auto","origin":"default"},
             {"key":"hints","value":"true","origin":"default"}]}
```

Every command-line tuning flag documented in [tuning.md](tuning.md) belongs to
`get`, `plan` and `apply`; `explain` does not carry them, so it can only ever
report a tuning setting from the environment, a configuration file, a
measurement, or the built-in default, never from a flag on the `explain`
invocation itself.

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

## init

```
fetchloom init <url|dir> [--output <path>] [--force]
```

Walks a directory or a listing and writes a manifest for what it holds. The
manifest is the result of the command, so it goes to standard output unless
`--output` names a file.

```
$ fetchloom init src
artifacts:
  -
    digest:
      blake3: "blake3:a74e619132c4c530d0d738f3cceddefaf06a79aad18b5be1a3bcbc054c1f3f84"
      sha256: "sha256:480c2336b410f1ad5f8bf1b28944490255804b65350c527787e74ebdd511e3a4"
    id: "nested/two.txt"
    layout: "keep"
    size: 7
    sources:
      - "nested/two.txt"
name: "src"
```

Every digest is one init observed. Over a directory it reads each file; over a
listing it fetches each object. It never takes a digest from a metadata request,
because an entity tag is not a digest.

That makes the run itself trust-on-first-use: it had no prior digest and
recorded its own first observation. The manifest is a first-use pin, not a
publisher's attestation. A later run against it is `verified`, which is what
pinning is for.

| Flag | Default | Meaning |
|---|---|---|
| `-o`, `--output <path>` | standard output | Write the manifest to this file |
| `--force` | off | Overwrite the file `--output` names |

A path that already holds a file is refused without `--force`, because a
manifest is edited after it is generated.

## doctor

```
fetchloom doctor
```

Reports what this machine can do and changes nothing. It reads a format
fingerprint rather than opening the cache, removes every probe it creates, and
makes no request, so it answers the same offline.

```
$ fetchloom doctor
configuration        ok         no project or user configuration file was found
cache                ok         C:\Users\you\cache matches this build's format
disk                 ok         47770595328 bytes free on the volume holding C:\Users\you\cache
permissions          ok         C:\Users\you\cache can be written to
certificates         ok         the platform trust store loaded
provider.amazon_s3   ok         Amazon S3: no credential is present
```

It reports whether a credential is present and never what it is. Exit is 0 when
every check passed and 50 when one found something you can act on.

## why

```
fetchloom why <ref>
```

Explains a decision a run already made: how the reference resolved, which source
was chosen and why, and what the trust class rests on. It reaches no network and
invents nothing, so a reference no run has touched is reported as exactly that
rather than guessed at.

```
$ fetchloom why src
resolution   a local path: src
destination  C:\Users\you\src
source       no run has been recorded for this destination
trust        no run has been recorded for this destination
```

Everything it prints comes from the receipt and the cache. When the class is
trust-on-first-use it also names how many independent witnesses were found and
how many `corroborated` needs.

## watch

```
fetchloom watch <events|->
```

Renders a run's event stream through the live view, live or after the fact. The
same renderer draws a file and a pipe, so a run writing its events somewhere can
be watched from another terminal.

```
$ fetchloom get ./src --output out --events ev.ndjson &
$ fetchloom watch ev.ndjson
dataset  src
source   ?
                       ########################         13 B  1 in flight  0 retries
cache    2 hit  0 miss     verified 0
entries  0          bytes 13 B
```

The view reads the event stream and nothing else. That is a property of where it
lives rather than a promise: it is a crate whose one dependency is the event
types, and a test fails if it gains another or names a filesystem, a network, or
a process. So it can never show you something `--json` and `--events` do not
already carry.

Fetchloom never backgrounds itself. The `&` above is your shell's.

## Flags every command takes, continued

| Flag | Default | Meaning |
|---|---|---|
| `-v`, `--verbose` | off | Raise the log level one step. Repeatable |
| `--color <auto\|always\|never>` | `auto` | When output carries color |
| `--no-hints` | off | Never print a hint |

A log level decides which of the events the run already emits are rendered to
standard error. It never decides which events exist, and the stream `--events`
writes is byte-identical at every level.

| Level | Rendered |
|---|---|
| `error` | errors and degradations |
| `info` | those, plus the run's start, end, and result. Default |
| `debug` | every event the stream carries, one line each |

`FETCHLOOM_LOG` names a level. Raising past `debug` is clamped, and the clamp is
reported rather than passed over.

Two more flags belong to `get` and `apply`:

| Flag | Default | Meaning |
|---|---|---|
| `--retries <n>` | 5 | Attempts per transient failure |
| `--timeout <duration>` | 30s | Idle timeout per connection, as `500ms`, `30s`, `2m`, or `1h` |

## Hints

After a run, Fetchloom may print one line about something you could have done
differently, such as a credential that would have made the transfer shorter by a
stated number of minutes. It never tells you a fact about itself, never
interrupts a transfer, and never says the same thing to you twice.

Hints go to standard error only. They never appear in the JSON result or the
event stream, and they are suppressed when the stream is not a terminal, in
continuous integration, and when no cache is available to record that one was
already said. `--no-hints` and `hints = false` turn them off for good.

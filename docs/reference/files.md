# The files you touch

Four formats. A manifest you may write; a lock, a receipt and a plan Fetchloom
writes.

All four share one canonical form. A manifest is accepted as YAML, TOML or JSON
and all three parse into the same model, so reformatting one never changes its
digest. Locks, receipts and plans are written in a small YAML subset and read
back from any of the three. Unknown keys are an error everywhere, and the `x-`
prefix is reserved and rejected.

The YAML accepted is block mappings, block sequences, flow sequences, flow
mappings, and plain, single-quoted and double-quoted scalars. Anchors, aliases,
merge keys, tags, directives, block scalars, document separators and tabs used
as indentation are each refused by name. `true`, `false` and `null` are the only
words read as anything but text, and a run of digits with an optional sign is
the only text read as a number, so `yes` is the word yes.

## Manifest

A manifest describes a dataset: what it is called and what artifacts it holds.
You write it, or a publisher does. A local path whose extension is `.yaml`,
`.yml`, `.toml` or `.json` is read as one; nothing else decides it, and a
directory is never searched for one.

```yaml
name: sample
release: "2026-08"
artifacts:
  - id: one
    sources:
      - ./one.tar.gz
  - id: two
    sources:
      - ./two.tar.gz
```

`name` and at least one artifact are required. `sources` is ordered: earlier
entries are preferred and later ones are failover only. An artifact may also
carry `size`, a `digest` with `blake3` and `sha256` keys, `media_type`, an
`archive` table naming its `format`, and `select` and `layout`. A `license`
table may carry `spdx`, `url` and `requires_acceptance`.

A `digest` may be absent, which forces a weak trust class. No key may express a
command, a script, or a path to execute: a manifest is data.

Fetching that file:

```
$ fetchloom get ./data.yaml --output out --json
{"status":"materialized","dataset":"sample","tree":"blake3:a57d80c2c45d8758434b86dad1d4f2733f3ce2950d35ef7d090ac0c951786964","destination":"C:\\Users\\you\\flman\\out","entries":2,"bytes":261,"work":{"bytes_read":512,"bytes_written":917,"requests":0,"file_operations":31},"trust":"tofu"}
```

## Lock

The lock is the portable artifact. It pins what a reference resolved to, and it
contains no absolute path, no hostname of your machine, no username, and no
timestamp. Commit it.

That run wrote:

```yaml
datasets:
  sample:
    artifacts:
      one:
        digest: "blake3:468143fafe02746206d16bed24d1af2e42c3e958c93c2b0bb0314f6faa4b74e6"
        interop: "sha256:ca423e79be9b78f89ec764e2ea1f2a11203a1bdf855c97a01d999a471d2e2ccf"
        layout: "keep"
        size: 127
      two:
        digest: "blake3:6ce2f3af4671c518acc720b0c90462390119bf87bb435f36dba944de65d7b6cf"
        interop: "sha256:3fb1ac41f84ba4195207bc2898fc4b74bb79f82557cd526be299153bc130ccff"
        layout: "keep"
        size: 129
    manifest: "blake3:64b8fb67085ff4b90adf48a4e8938b8543256d99ae2bcbabcf15ed4e75cca122"
    release: "2026-08"
    tree: "blake3:a57d80c2c45d8758434b86dad1d4f2733f3ce2950d35ef7d090ac0c951786964"
```

`digest` is the cache key and the resume authority: a BLAKE3 root over the
object's bytes. `interop` is SHA-256 of the same bytes, recorded so you can
match a publisher's published checksum, and never used as a key. `manifest` is
the digest of the manifest's canonical form, so reformatting the manifest does
not move it. `tree` appears only after a successful materialization; a lock
without one still pins the bytes.

`select` and `layout` are in the lock because selection is part of identity.
Two runs of the same dataset with different `--select` pin different entries.

`--locked` holds a run to all of it. What each difference means is in
[errors.md](errors.md).

## Receipt

The receipt is local. It may contain absolute paths, it is never committed, and
it is never read as an authority for identity. It lives in the cache, keyed by
the digest of the destination it describes, so it is found from any working
directory and a destination you moved is simply not found.

```yaml
artifacts: {}
completed_at: "2026-08-30T20:16:34Z"
dataset: "blob.txt"
destination: "C:\\Users\\you\\flgo\\o1"
fetchloom: "0.1.0"
fingerprints:
  blob.txt:
    changed_nanos: "1788120994627174700"
    file: "9288674232546529"
    modified_nanos: "1788120994627174700"
    size: 20
    volume: "8072152529104517852"
manifest: "blake3:93ee5a5fefd4b14d13c554fab5c3c3bdf0661cfc87d41d699b8f1d3dd9cdf32c"
tree: "blake3:b20d51dd8baf30927977baf94a3a1f8c990f6c57ba5402f7fa73e94e471a2995"
```

`artifacts` maps each artifact to the digest it resolved to, the source actually
used, and its trust class. It is empty above because the run resolved to no
artifact; a run over an archive or a URL fills it.

`executable`, absent here, lists the entry paths materialized with the
executable mode. Every other file entry carries the read and write mode, because
a tree digest records those two and no others. This is what lets `verify`
reproduce a digest a filesystem cannot state on its own.

`fingerprints` records the volume, file identifier, size and two timestamps each
file carried when the run published it. It is a cache of the phrase *probably
unchanged*, never evidence of content, never compared against another machine's,
and never in a lock. It is what `--verify fingerprint` reads to decide a
destination entry is unchanged without reading its bytes.

`fetchloom` is provenance: it says which build produced the result and nothing
ever branches on it.

## Plan

A plan is a resolved run written to a file, so it can be carried to a machine
with no network and executed there. Its digests come from the lock.

```yaml
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

`network.hosts` names who would be contacted and `required` says whether the run
needs any of them; both are empty here because the object is already cached.

`disk` reports four requirements separately: the partial transfer, the cache
object, the extraction staging, and the destination. Each is attributed to a
volume, and requirements on one volume are summed.

`unknown` lists every field the source could not supply. A field there is never
turned into a number.

`cached`, `disk`, `conflicts` and `destination` describe the machine the plan
was made on. `apply` reports them and acts on none of them; `--output` names the
destination when the plan's own is not yours.

## Bundle

A bundle is not a format you write, but it is a file you carry. `cache export`
writes one and `cache import` reads one. It is an uncompressed tar whose every
member is named by the lowercase hexadecimal of the content digest of its own
bytes.

A tar has no index, so there is nothing in a bundle that could be trusted
instead of its bytes. Import derives every digest from what it reads: a member
name is a claim, never an instruction, never used as a path, and always compared
against what the bytes hash to. Every member is staged and checked before
anything is published, so a bundle that fails anywhere publishes nothing.

See [cache.md](cache.md) for the export-carry-import sequence with real output.

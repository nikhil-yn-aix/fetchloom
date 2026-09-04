# Trust

Every artifact a run resolves is labelled with one of four classes. Each has a
mechanical definition: it is a statement about what was compared, not a judgment
about whether the data is good.

| Class | What it means |
|---|---|
| `verified` | The bytes hashed to a digest that was already written down before this run started, in the manifest or in the lock |
| `corroborated` | Nothing said in advance what the digest should be, but at least two independent recorded observations agree with what was seen |
| `tofu` | Nothing said in advance and nothing corroborates it. What was seen is written down for next time |
| `unverified` | The content could not be hashed, or you turned verification off |

`tofu` is trust on first use. It is what you get the first time you fetch
anything that publishes no checksum, which is most public data. It is accepted
by default for a first fetch and never for a locked run.

`unverified` requires an explicit flag on every invocation. It is not a claim
that the bytes are wrong; it is a statement that nothing checked them.

Three facts are tracked separately and none implies another: who published
something, whether a manifest is authentic, and whether content is intact.
Fetchloom records content integrity. It makes no claim about the other two.

## Verification during a transfer is never optional

Bytes are hashed as they arrive, in the same pass that writes them, and nothing
enters the cache until the digest matches. `--verify` governs what happens when
an object is *reused*: a cache hit, or a destination entry a later run finds
already in place. It never turns off the check during a transfer.

`--verify never` therefore does not mean "accept bad bytes". It means the reuse
path does not verify at all, which is exactly why the class it produces is
`unverified` rather than anything else.

## What raises a class

A witness is one recorded observation that an artifact hashed to a digest. It
holds the digest seen, the machine that saw it, the origin that served the
bytes, the run that recorded it, and when.

A witness is written only by a run that transferred the bytes in full and
verified them as they arrived. A cache hit writes none, because it observed
nothing. Nothing read from a source, a bundle, a lock, a receipt or a plan ever
becomes a witness, so no remote party can manufacture one.

Two witnesses are independent only when they differ in **all three** of the
machine, the origin, and the run. Two observations from one machine are one
observation. So are two from one origin, and two from one run.

`corroborated` therefore needs at least two machines, at least two origins, and
at least two runs. Fetching the same file twice from one mirror on one laptop
stays `tofu`, and that is the point of the definition rather than a limitation
of it.

The only channel by which another machine's witness reaches yours is a cache
directory shared between them, and it is exactly as trustworthy as that
directory, which any writer could edit directly.

## Where you see it

The class is recorded per artifact in the receipt:

```yaml
artifacts:
  corpus:
    digest: "blake3:..."
    source_used: "https://host/silesia.tar.zst"
    trust: "tofu"
```

and in the `trust` field of a plan and of the `--json` result of `get`.

## What none of this means

Size, ETag, Last-Modified, a valid certificate, and a provider's identity are
supporting evidence only. None of them is ever treated as content identity, and
none of them raises a class.

A `304 Not Modified` answer to a revalidation leaves the class unchanged. The
source is restating a validator it already gave; it has said nothing new about
the bytes.

Fetchloom makes no legal determination about licences. If a manifest records
`requires_acceptance`, the run stops until you assert acceptance, and it records
that you asserted it. That is all it records.

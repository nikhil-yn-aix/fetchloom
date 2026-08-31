# When a run stops

Every failure names a kind and exits with a code you can branch on. Under
`--json` the failure is one object on standard output; otherwise it is one line
on standard error.

```
$ fetchloom get ./nope.tar --json
{"kind":"reference.unresolved","layer":"resolve","dataset":null,"artifact":null,"source":null,"attempts":0,"retryable":false,"next_action":"check that ./nope.tar names a path that exists"}
$ echo $?
10
```

`layer` says which part of the run failed. `attempts` and `retryable` say
whether it was retried and whether retrying could help. `next_action` is what to
do about it.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 2 | You asked for something that is not a command |
| 10 | The reference could not be resolved |
| 20 | The network failed after every retry |
| 30 | The bytes are not what they were supposed to be |
| 40 | Policy blocked the run |
| 50 | Not enough disk, or a resource limit was exceeded |
| 60 | The destination is in the way |
| 70 | The archive contains something unsafe |
| 80 | The cache could not be used |
| 130 | Cancelled |

## Kinds, and what to do

### Resolution, exit 10

| Kind | What it means | What to do |
|---|---|---|
| `reference.unresolved` | Nothing is there, or what is there cannot be read | Check the path or URL. The message says which of the two |
| `manifest.invalid` | A manifest does not parse, or holds a key the model does not name | The message names the key and the line |
| `alias.unstable` | A locked run resolved a different manifest, release, selection or artifact than the lock pins | Run once without `--locked` to record what it resolves to now |

An empty selection is `reference.unresolved` and not a silent no-op:

```
$ fetchloom get file://$PWD/sample.tar.gz --output s5 --select 'nothing/**' --json
{"kind":"reference.unresolved", ... ,"next_action":"select a pattern that matches, because none of the 4 members matched nothing/**"}
```

### Transfer, exit 20

| Kind | What it means |
|---|---|
| `network.timeout` | The source went quiet for longer than the idle timeout |
| `network.refused` | The connection did not complete |
| `network.status` | The source answered with a status the run cannot use |
| `network.tls` | The certificate could not be verified |
| `source.unsupported_range` | A range was asked for and the source served the whole object |

`source.identity_changed` is in the taxonomy for a source whose identity moves
mid-transfer, and nothing in this build produces it: a changed validator is
caught by the resume ladder, which drops a rung and reports it.

A transient failure is retried before the run gives up, and `attempts` says how
many times. A source asking to be left alone is honored up to the retry ceiling.

```
$ fetchloom get https://no-such-host.invalid/x.tar.gz --json
{"kind":"network.refused", ... ,"attempts":5,"retryable":true,"next_action":"try the source again, because the request did not complete: io: No such host is known. (os error 11001)"}
$ echo $?
20
```

### Integrity, exit 30

| Kind | What it means | What to do |
|---|---|---|
| `integrity.mismatch` | The bytes do not hash to the digest that was expected | For a cached object, `fetchloom repair <ref>`. For a destination, fetch it again |
| `integrity.truncated` | The source stopped short of the length it stated | Run again; a resume picks up from what arrived |
| `integrity.range_mismatch` | A ranged answer covered a span other than the one asked for | Run again. The partial is discarded first |

```
$ echo tampered >> out3/one
$ fetchloom verify out3 --json
{"kind":"integrity.mismatch","layer":"verify", ... ,"next_action":"fetch ...\\out3 again, because it now holds blake3:21659c9076455da14c944d262c46da17ae7125dcbb682a0d9a76db3cc2355d4d where the run that wrote it reported blake3:cc6d1e52b3589084fb184dffd1ec06e79d9176e193ba43f2103c800ede49dc87"}
$ echo $?
30
```

### Policy, exit 40

| Kind | What it means | What to do |
|---|---|---|
| `policy.offline` | The run needed the network and `--offline` was given | Drop `--offline`, or plan on a connected machine and carry a bundle |
| `policy.terms_required` | A confirmation is needed and there is no terminal to ask on | Run again with `--yes` |
| `policy.trust_refused` | A locked run found no lock entry for the dataset, or a plan found no pinned digest | Run once without `--locked` |

`policy.credential_missing` and `policy.credential_invalid` are in the taxonomy
and no run in this build produces either. Nothing here asks a source for a
credential, so `FETCHLOOM_TOKEN_<HOST>` is read by nothing. Both arrive with the
providers that need them.

```
$ fetchloom get https://host.example/x.tar --offline --json
{"kind":"policy.offline", ... ,"next_action":"run the command again without --offline to reach https://host.example/x.tar"}
```

### Resource, exit 50

| Kind | What it means |
|---|---|
| `resource.disk` | A volume has no room for what the run needs |
| `resource.limit` | The processor pool could not be built for the thread ceiling given |

Disk is checked as the volume refuses a reservation, at the partial transfer,
the cache object, the extraction staging and the destination alike.

### Destination, exit 60

| Kind | What it means | What to do |
|---|---|---|
| `destination.modified` | A file you changed is in the way | `--force` to overwrite it, `--adopt` to accept the destination as it stands |
| `destination.foreign` | Something is there that the resolved tree does not name | `--force` to remove it, `--adopt` to accept it |
| `destination.unrepresentable` | A name cannot exist on this volume, or a flattened member would be left with no path | The message names the member and what the volume said |
| `destination.cross_volume` | A publication would have had to cross volumes | Put the destination on the volume the staging is on |

```
$ fetchloom get file://$PWD/sample.tar.gz --output data
destination.modified: run again with --force to overwrite the modified entries and remove the foreign ones, or --adopt to accept the destination as it stands: a.txt (modified)
$ echo $?
60
```

Nothing is ever partially reconciled. The run stops before anything is staged,
and a destination is never left holding a tree that is neither the one it held
nor the one that was resolved.

### Archive, exit 70

| Kind | What it means |
|---|---|
| `archive.unsafe_path` | A member path is absolute, traverses upward, holds a drive letter or a NUL, is not valid UTF-8, or is longer or deeper than allowed |
| `archive.link_escape` | A link target resolves outside the destination, or a hard link names a member the archive does not hold |
| `archive.collision` | Two members land on one path, including under the volume's case folding or Unicode normalization |
| `archive.bomb` | More entries, more expanded bytes, or a higher expansion ratio than the limits allow |
| `archive.unsupported` | A format, compression method, entry type, or permission bit this build does not carry |

Every rejection stops the whole run. No member is ever skipped: an archive
holding one rejected member cannot be fetched, and excluding that member with
`--select` does not change it. Nothing is published, and the staging directory
is emptied.

```
$ fetchloom get file://$PWD/bad.tar --output dbad --json
{"kind":"archive.unsupported","layer":"extract", ... ,"next_action":"rename it or state its format in a manifest, because the name said \"tar\" and the archive's bytes said unrecognized bytes"}
$ echo $?
70
```

### Cache, exit 80

| Kind | What it means | What to do |
|---|---|---|
| `cache.locked` | Another process holds the object and the wait ended without it | Run again |
| `cache.corrupt` | A cached object does not hash to its name, or a cache file could not be read or written | `fetchloom cache verify`, then `fetchloom repair <ref>` |
| `cache.format_mismatch` | The cache was written by a build that used a different format | `fetchloom cache clear` |
| `cache.cross_volume` | A publication into the cache would have crossed volumes | Put the cache on the volume the staging is on |
| `cache.locking_unsupported` | The volume cannot express the advisory locks a shared cache needs | Put the cache somewhere else |

```
$ fetchloom cache status --json
{"kind":"cache.format_mismatch", ... ,"next_action":"run cache clear, because ...\\cache was written in a format this build does not read and nothing is migrated"}
$ echo $?
80
```

`cache clear` is the one command the check does not apply to, because removing
a directory does not depend on what wrote it.

A cache that is missing, read-only, or out of space is not this. It does not
stop a run: Fetchloom says so with a `degrade` event and continues as though
`--no-cache` had been given.

### Cancelled, exit 130

Contracted, not implemented in this build. Interrupting a run terminates the
process with whatever code your shell reports. The cache is still safe: nothing
enters it without a completed verification and an atomic rename, so an
interrupted run leaves either a valid object or a resumable partial.

## Degradations

Not a failure. Whenever any capability, optimization or trust level is lower
than what was asked for, the run says so and continues. Silence is never used to
signal that something was given up.

```
{"event":"degrade","requested":"the cache at Q:\\no\\cache","used":"no cache, so nothing is retained","reason":"Q:\\no\\cache: The system cannot find the path specified. (os error 3)"}
{"event":"degrade","requested":"the mode each file carries","used":"0644 for every file","reason":"a filesystem tree states no mode, and reading one back from a volume that carries an executable bit would digest the same tree differently than a volume that does not"}
{"event":"degrade","requested":"the live view","used":"the none view","reason":"the standard error stream is not a terminal"}
```

Degradations reach you on the progress stream, and in full with
`--events <path|->`.

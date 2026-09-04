# Security

## Reporting a vulnerability

Report privately at
https://github.com/nikhil-yn-aix/fetchloom/security/advisories/new. Do not open
a public issue for a suspected vulnerability.

Include the reference or input that triggers it, the command that was run, the
version the binary prints, and the platform. A corrupted archive, a manifest, or
a captured response is more useful than a description of one.

Fetchloom has one maintainer and no published release. There is no response time
commitment before 1.0. You will get an acknowledgement and, if the report is
accepted, the commit that closes it.

## What is in scope

Fetchloom's threat model is that everything on the far side of the network is
hostile and everything the user typed is not. A remote source can serve any
bytes it likes, and no amount of them may cost more than the limits in
contracts.md allow.

In scope:

Anything a remote source controls: manifest files and manifest URLs, directory
listings, HTTP responses and headers, archive containers and their members, and
digests or sizes a source declares.

Escaping the destination: a member path, a symlink target, or a hard link that
places a file outside the tree the user named.

Resource exhaustion driven by remote input: memory, disk, file descriptors, or
processor time that grows without a bound the limits in contracts.md state.

Cache corruption or cross-process interference: one run reading bytes another
run wrote, a killed process leaving an object that does not hash to its name, or
a cache shared between users leaking content.

Credential and secret handling: a token, a signed query string, or a presigned
parameter appearing in a log record, an event, a receipt, a plan, or an error
message.

Trust decisions: certificate verification, the digests a lock pins, and anything
that lets a run accept bytes that do not match what it verified.

## What is out of scope

A dependency's vulnerability, which belongs upstream. Report it there, and open
an advisory here only if Fetchloom's use of it makes it worse.

Anything requiring an attacker who already controls the machine, the cache
directory, or the user's configuration file.

Cost the user asked for. A reference that names a large dataset transfers a large
dataset.

Missing hardening that is not a defect: a feature marked **Not built.** in
docs/features.md is not a vulnerability.

## Classes already closed

Two remote-input denial of service classes were found and fixed before any
release.

A zip symlink member was read in full before the bomb guard was consulted, so a
100 MB archive could force about 100 GB of decompression before the expanded-byte
limit or the ratio was checked. The guard is now asked with the declared size
before the member kind is acted on.

Glob matching recursed over every suffix at each recursive wildcard, so a pattern
in a manifest fetched from a remote URL could be made to cost exponentially in
the number of `**` components. Matching is now linear in the pattern and the
path.

Both are recorded in docs/decisions.md with their measurements and the tests that
hold them closed.

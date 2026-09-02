# Fetchloom

Fetchloom turns a dataset reference into exact local files and proves what it
delivered.

```
fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --output hello
```

That resolves the reference, reuses anything valid you already have, transfers
what is missing, checks the bytes against a digest, extracts the archive into a
directory only after every check passes, and writes down what it produced. No
account, no daemon, no project file, no language runtime.

## The problem

Most published datasets ship no checksums. You download a tarball, it works,
and six months later you cannot tell whether the file on the server is still the
file you used. Retrying a large download over a bad link starts from zero. A
second run against a directory you have edited overwrites your work or creates
`data (1)`. When a file does turn out to be corrupt, you refetch all of it to
repair a few bad bytes.

Fetchloom's answer is that your first successful fetch is the reference truth.
It records the digest of what you got, and every later run is checked against
that record. This is what a lock file does for source code, applied to data.

Around that one idea:

- A transfer resumes from what already arrived, and only when the bytes on disk
  provably belong to the same object.
- A destination is reconciled, never clobbered. A file you changed stops the run
  by name.
- Damage is located at the byte range. A corrupt megabyte inside a large object
  is repaired by refetching that megabyte.
- Every result names how much it is worth trusting, with a definition you can
  check rather than a word you have to believe.
- A run can be planned on a connected machine and executed on one with no
  network at all.

## What this build does today

Fetchloom is before 1.0 and ships nothing it cannot do. A command or a flag is
in the binary only once it performs what is documented. That means the surface
here is smaller than the finished product, and everything in it works.

Today it fetches over HTTPS and from local paths, reads ten archive formats,
keeps a content-addressed cache, writes and enforces locks, writes receipts,
plans and applies offline, exports and imports bundles, repairs damaged byte
ranges, and tunes its own concurrency and write path to what a host and a
volume measure. It does not yet speak to object stores or dataset providers,
does not list directories, and does not resolve a remote manifest.

## This directory

| File | What it answers |
|---|---|
| [getting-started.md](getting-started.md) | How to build it and run a first real command |
| [commands.md](commands.md) | Every command and flag, with a run of each |
| [tuning.md](tuning.md) | The concurrency, bandwidth and I/O flags, the environment variables, and the precedence between them |
| [references.md](references.md) | The ways to name data, and which this build resolves |
| [files.md](files.md) | The manifest, lock, receipt and plan formats |
| [trust.md](trust.md) | What each trust class means |
| [errors.md](errors.md) | Every exit code and error kind, and what to do about it |
| [cache.md](cache.md) | Where the cache is, what it holds, how to control it |

Everything else under `docs/` is written for the people building Fetchloom:
`contracts.md` is the exact specification, `decisions.md` is why each choice was
made, `standards.md` is how the code is written, and `roadmap.md` is what is
coming.

# Naming data

A reference is what you type after `get`. One verb takes every form, and you
never have to say which kind you have.

## The forms

| Form | Example |
|---|---|
| Bare name | `silesia` |
| Namespaced with a release | `acme/imagenet@2012` |
| Local manifest | `./data.yaml` |
| Remote manifest | `https://lab.edu/eeg.yaml` |
| Direct file | `https://host/x.tar.zst` |
| Local file or directory | `file:///data/raw`, `./src` |
| Object store | `s3://bucket/prefix/` |
| Provider | `hf:datasets/org/name@rev`, `zenodo:10.5281/zenodo.1234567` |
| Metadata document | `croissant:https://host/metadata.json` |
| Content address | `blake3:<hex>` |

Resolution is deterministic: an explicit scheme wins, then a local path that
exists, then configured source priority. A bare name that matches nothing fails.
It is never guessed at.

## What this build resolves

Four of those forms work today. The rest fail with `reference.unresolved`.

**A direct HTTPS file.**

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --output hello --json
{"status":"materialized","dataset":"hello-2.12.tar.gz","tree":"blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec","entries":462, ... }
```

**A local file, as a `file:` URL or as a path.** A recognized archive is
extracted; anything else is materialized as one file inside the destination.

```
$ fetchloom get file://$PWD/sample.tar.gz --output data --json
{"status":"materialized","dataset":"sample.tar.gz","entries":4, ... }
```

**A local directory.** Every file under it is copied into the destination. A
directory states no file modes, so every file is `0644` and the run says so:

```
{"event":"degrade","requested":"the mode each file carries","used":"0644 for every file","reason":"a filesystem tree states no mode, and reading one back from a volume that carries an executable bit would digest the same tree differently than a volume that does not"}
```

**A local manifest.** A path whose extension is one a manifest is written in
(`.yaml`, `.yml`, `.toml`, `.json`) is read as a manifest and nothing else
decides it. A directory is never searched for one. See [files.md](files.md).

```
$ fetchloom get ./data.yaml --output out --json
{"status":"materialized","dataset":"sample","tree":"blake3:cc6d1e52b3589084fb184dffd1ec06e79d9176e193ba43f2103c800ede49dc87","entries":2, ... }
```

The rest fail, and the object-store form says exactly why:

```
$ fetchloom get s3://bucket/prefix/ --json
{"kind":"reference.unresolved", ... ,"next_action":"this build resolves only a local path or a file: location, not s3://bucket/prefix/"}
```

A bare name, a provider reference and a content address report that the string
does not name a path that exists, which is true and is not the reason you want.
None of them is resolvable in this build.

## One object or many

A reference naming one object produces a destination directory holding that one
entry, under the object's own name. A reference naming a container produces
every entry the container holds. The two differ only in what is walked, never in
what a destination is.

Directory listing, which is how a container reference over the network is
expanded, is not in this build. Only a local directory can be walked.

## Archives

A reference whose final extensions name a container is extracted, unless
`--no-extract` is given. The archive's own header must agree with what the name
said, or the run fails with `archive.unsupported` naming both. Neither the name
nor the bytes decides alone.

| Name | What it is |
|---|---|
| `tar` | A POSIX ustar stream |
| `tar+gzip`, `tar+zstd`, `tar+xz`, `tar+bzip2` | A tar wrapped in one compression |
| `zip` | A zip container, store and deflate methods only |
| `gzip`, `zstd`, `xz`, `bzip2` | One compressed object, materialized as one file |

Recognized extensions are `.tar.gz`, `.tar.zst`, `.tar.xz`, `.tar.bz2`, `.tgz`,
`.tbz2`, `.zip`, `.gz`, `.zst`, `.xz`, `.bz2` and `.tar`. A manifest may state
the format instead, and it is still checked against the bytes.

In this build a bare, uncompressed `.tar` is refused. The sniff reads too few
leading bytes to find a tar's magic, which sits at offset 257, so every bare tar
reports that its bytes are unrecognized:

```
$ fetchloom get file://$PWD/posix.tar --output d --json
{"kind":"archive.unsupported","layer":"extract", ... ,"next_action":"rename it or state its format in a manifest, because the name said \"tar\" and the archive's bytes said unrecognized bytes"}
$ echo $?
70
```

Every compressed form works, including `.tar.gz` of the same content. Compress
the tar, or pass `--no-extract` to take it as a file.

## What a lock is written for

An unlocked run records what it resolved. A reference that resolves to an object
gets a lock entry; one that resolves to no object gets none, and the run says so
with a `degrade`.

In this build a plain local file — one that is not a recognized archive — is
walked as a tree rather than resolved as an object, so it writes no lock even
though it resolved to exactly one thing. The degradation it emits says the
reference names a directory, which for a single file is not true. The
consequences are that `plan`, `apply` and `--locked` cannot be used with such a
reference; `get` and `verify` work normally.

An archive, a directory-of-files, a manifest and an HTTPS reference are all
unaffected.

## Selection

`--select` and `--exclude` take globs against the canonical `/`-separated member
path. Matching is on raw bytes, case-sensitive, with no normalization.

A pattern is four rules and nothing else:

- `*` matches any run of bytes inside one path component, including none.
- `**` as a whole component matches any number of components, including none.
- `?` matches exactly one byte inside one component.
- Every other byte is literal, including `[`, `{` and `\`.

A pattern matches member paths, not subtrees. `--select data` selects a member
named `data`; `--select 'data/**'` selects what is under it. Every ancestor
directory of a selected member comes along whether a pattern matched it or not.

`--exclude` is applied after every `--select`.

# decisions

a decision changes only with new evidence, recorded in its entry.

## 1. rust, one binary, also shipped as a python wheel

one binary serves python, R and shell users alike, starts in milliseconds, and runs through `uvx fetchloom` with no setup. a python package would start slower and could clash with the packages in a user's environment.

## 2. the loop and nothing else

v0.1 is add, remove, sync, lock, status, path, get and the cache, as listed in [scope](scope.md). steps, publishing, search, plugins, copy and pack are cut, because other tools do those jobs, and an earlier version of fetchloom never shipped while building them all.

## 3. SHA-256 is a file's identity

it runs at 1.13 GB/s on one laptop core in our test, faster than almost any network, and it is what hugging face, osf, github release assets, nix, OCI images and software heritage already use. the `sha2` crate picks the CPU's SHA instructions at runtime. blake3 is faster, but no source states it, so every lookup would need a second index.

## 4. the first download defines the hash when a source states none

google drive, kaggle, dropbox and plain URLs give no checksum. the first download is hashed, every later download must match it, and a mismatch is an error. where a source states MD5, it is checked on the first download too.

## 5. the lock lists what was downloaded

each downloaded file gets one line in `data.lock`. an archive gets one line plus the tree hash of what it unpacks to, not one line per member, so a zip of 100,000 images keeps the lock small and every machine can still prove it unpacked the same files.

## 6. archives unpack by default

66% of the 200 most-viewed zenodo datasets contain archives (zip, tar, 7z or rar), and people want the files inside. `extract = false` keeps an archive packed. once its members are stored and verified, the archive's own copy leaves the cache unless a project keeps it packed, so a 50 GB zip does not cost 100 GB.

## 7. sources are rust modules in this repository

no metadata standard gives trustworthy checksums: signposting carries none, croissant on hugging face holds a placeholder, and schema.org and datacite carry none. so each source is a small module that calls its own API. the earlier version's TOML and JSONPath engine grew new features for every source, and plugins would add a protocol to maintain before anyone asked for one.

## 8. a DOI is routed by the host it lands on

doi.org is asked where a DOI points, without following the redirect, and that host picks the source. DOI prefixes are never used, because every dataverse or CKAN install registers its own.

## 9. google drive, kaggle and dropbox are in v0.1

github readmes mention drive.google.com about 358,000 times, kaggle.com/datasets about 134,000 times and dropbox share links about 27,000 times, against about 35,000 for zenodo records. gdown alone is downloaded about 3 million times a month.

## 10. `fetchloom` is the command and `fl` is its short name

windows powershell already uses `fl` for `Format-List`, and an alias there wins over any program. `fetchloom` also makes `uvx fetchloom sync` work, since uvx runs the command named after the package.

## 11. reflink, else hardlink, else copy, and never symlinks

reflinks and hardlinks cost no disk. symlinks need admin rights on windows and dangle when a cache is cleared. hardlinked objects are read-only, and a placed file found changed gets its object hashed before reuse, so an edit cannot spread to other projects unnoticed. inside folders that onedrive, dropbox or icloud drive sync, files are copied, because those tools break hardlinks. `copy = true` exists for tools that write into their inputs.

## 12. the cache writes without locks

an object is written to a temp file, flushed, and put in place by an operation that fails if the name exists, so racing processes cannot corrupt it. file locks cannot be trusted on NFS, lustre and GPFS, which is where shared caches live.

## 13. one crate

seven crates for an unreleased tool meant seven sets of boundaries to maintain before any were proven. modules with clear ownership give the same separation. a module becomes a crate when a second consumer needs it.

## 14. reqwest on tokio, with rustls and the operating system's certificate store

HTTP/2 lets many small files from one host share a connection, and hyper's connector races IPv4 and IPv6. `rustls-platform-verifier` trusts what windows and macos trust, so university and company certificates work. ring is the crypto provider because it builds on windows without extra tools. ureq is smaller, but it connects to one address at a time and has no HTTP/2.

## 15. network work on tokio, disk work on threads

async file IO on tokio is slower than plain threads for writing, hashing and unpacking. bounded channels between the two keep memory flat.

## 16. timeouts limit silence, never duration

a slow 100 GB download must finish and a stalled one must fail. the earlier version lost a 3.7 GB download at 30 seconds because a connect timeout bounded the whole body.

## 17. no config file in v0.1

a handful of environment variables and flags cover every setting. a file adds precedence rules and docs before anyone needs them. a cluster-wide file comes with the lab features.

## 18. errors have a kind, a sentence and a next step

kinds like `network.timeout` are stable for scripts and `--json`. a few exit codes group them, so a script can tell "network" from "integrity" without parsing text.

## 19. the terminal stack is small

clap parses arguments and suggests "did you mean". anstream and anstyle color the output, indicatif and console draw progress, and dialoguer asks the two prompts: yes or no, and a line of text. nothing else draws on the terminal. symbols fall back to ASCII on the legacy windows console.

## 20. standard archive crates behind our own guard

`zip` at 9.1.2 or later, `tar`, flate2 on zlib-rs, liblzma, zstd and bzip2. october 2026 brought critical advisories in zip's extraction, and no audited crate enforces every limit, so one guard of ours checks every path, refuses every link inside an archive and counts every byte written. 7z waits for demand. rar is refused, because its only decoder is not open source.

## 21. no coverage or mutation gates

the earlier version spent whole milestones killing mutants while no user had run the tool. tests prove behavior through scenarios, snapshots and counters. coverage is looked at, and mutation testing runs on core modules before a release.

## 22. CI gates on counters, never on wall clock

requests, bytes, file operations and instruction counts are deterministic. wall clock time on laptops and shared runners swings by 2x, so it is recorded and tracked, never used to fail a build.

## 23. dist for binaries, maturin for wheels

uv and ruff ship the same way: one version tag, binaries and installers on the github release, wheels on PyPI through trusted publishing, with attestations for both.

## 24. no telemetry

fetchloom talks only to the hosts the data lives on. researchers work with sensitive projects, and trust is worth more than usage numbers.

## 25. the design is public

these docs live in the repository, so anyone can see why fetchloom works the way it does and argue with it.

## 26. apache-2.0

as permissive as MIT, with a patent grant, a NOTICE that travels with every copy, and no rights to the names fetchloom and KairosLab.

## 27. editing a dataset never upgrades it

changing a dataset's filters resolves it again at the version already in `data.lock`. only `fetchloom lock --upgrade` moves to a newer version, as in uv, so tidying `data.toml` can never change your data behind your back.

## 28. a dataset lists at most 100,000 files in v0.1

that keeps `data.lock` under about 20 MB and memory bounded. archives can hold up to 1,000,000 members, since they are recorded by one tree hash. the limit moves when a real dataset needs it.

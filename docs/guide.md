# Guide

Everything you actually do, in the order you hit it. If you want a lookup table instead, that is [reference.md](reference.md).

## Your first run

```
fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz
```

You get a directory named after the dataset, in the folder you are standing in. The archive is unpacked. A file called `fetchloom.lock` appears beside it recording exactly what you got.

Run the same command again and it prints `unchanged` and writes nothing. That is the point. It knows the directory is already correct because it can hash it and compare.

## What you can point it at

All of these work with `get`, and you never have to say which kind it is.

```
./data.yaml                              a manifest on disk
https://lab.edu/eeg.yaml                 a manifest on a server
https://host/x.tar.zst                   one file
file:///D:/raw                           a folder on this machine
https://s3.amazonaws.com/bucket/prefix/  everything under a prefix
hf:datasets/org/name@rev                 a provider
blake3:9f2c...                           bytes by their digest
ftp://ftp.ebi.ac.uk/pub/                 a directory on an FTP server
ham10000                                 a name, searched for across the registries
```

Eight providers are reached by their own identifier rather than by a location.

```
hf:datasets/org/name@rev                       a Hugging Face repository
zenodo:10.5281/zenodo.1234567                  a Zenodo record
kaggle:uciml/iris                              a Kaggle dataset
openml:61                                      an OpenML dataset
github:BurntSushi/ripgrep@15.2.0               the assets of one GitHub release
figshare:1234567                               a Figshare article
ckan:demo.ckan.org/a-dataset                   a dataset on any CKAN install
dataverse:dataverse.harvard.edu/doi:10.7910/DVN/OMV93V   a dataset on any Dataverse
```

CKAN and Dataverse are not one site each, they are software many organizations run, so the host is part of the reference. `ckan:data.gov.uk/...` and `ckan:demo.ckan.org/...` go to different installations through the same adapter.

Leave the tag off a GitHub release and it takes the latest one. Every one of these names the whole record; take part of it with `--select`.

If all you have is a DOI, give it the DOI.

```
fetchloom get doi:10.7910/DVN/OMV93V
```

It reads the registration, works out which provider holds the record, and tells you what it decided. A DOI belonging to a provider it has no adapter for stops and says which provider that is, rather than guessing at an API.

A folder, an object store prefix, a WebDAV directory, an FTP directory, or a generated HTML index all expand to the files inside them. It lists what you point at. It does not follow links out of that prefix and it never opens a file to find more work.

## FTP, when that is what the data is on

Much of public biology is still on FTP, so `get` speaks it.

```
fetchloom get ftp://ftp.ncbi.nlm.nih.gov/genomes/README.txt
fetchloom get ftp://ftp.ebi.ac.uk/pub/databases/ --output ebi
```

You log in as anonymous unless you say otherwise. An interrupted transfer resumes from where it stopped.

`ftp://` tries to encrypt the connection and tells you when the server would not let it. Write `ftps://` and it will not go on unencrypted at all.

```
fetchloom get ftps://host/pub/reads.fastq.gz
```

There is no flag that turns that off, and if you have set a password for a host it is never sent over a connection that could not be encrypted. If you need one, set it as `user:password`.

```
setx FETCHLOOM_TOKEN_HOST_EXAMPLE "someone:their-password"
```

SFTP is a different protocol and this does not speak it.

## Finding something by name

If you know what the dataset is called but not where it is, just say the name.

```
fetchloom get ham10000
```

With no `sources` in your configuration it searches Hugging Face, Kaggle, OpenML, Zenodo, Figshare, CKAN, Dataverse and DataCite at once, and one of three things happens.

One place has it, so it tells you what it took and gets on with it.

```
resolved ham10000 to kaggle:kmader/skin-cancer-mnist-ham10000
```

Several places have it, so it stops and shows you, because they may not be the same data.

```
name one of them, because ham10000 matched 2 records and a name is never guessed at:
  kaggle:kmader/skin-cancer-mnist-ham10000 — Skin Cancer MNIST: HAM10000, 5.2 GiB, no checksum, so a first fetch is trusted on first use
    fetchloom get kaggle:kmader/skin-cancer-mnist-ham10000
  dataverse:dataverse.harvard.edu/doi:10.7910/DVN/LZJTKO — Segmented HAM10000, an unstated size, a checksum the install states, verified when it is SHA-256
    fetchloom get dataverse:dataverse.harvard.edu/doi:10.7910/DVN/LZJTKO
```

Copy whichever line you want. Nowhere has it, and it tells you what came closest.

```
did you mean one of these, because ham10k matched nothing exactly:
  ham10000
    fetchloom get kaggle:kmader/skin-cancer-mnist-ham10000
```

The name is never what goes in the lock. What it resolved to goes in, so the first run searches and every run after it goes straight there.


## Where things land

Two places, and they do different jobs.

The **destination** is your directory. It is the real copy. Default is `./<name>`, change it with `--output`.

The **cache** is shared storage of verified bytes, one per user, at `%LOCALAPPDATA%\Fetchloom\Cache` on Windows or `~/.cache/fetchloom` on Linux. Ten projects fetching the same dataset download it once. You can delete the whole cache any time and lose nothing but time.

If the cache is missing, full, or read only, the run says so and continues without it.

## Taking part of something

```
fetchloom get <ref> --select "**/*.txt"
fetchloom get <ref> --select "train/**" --exclude "**/*.tmp"
fetchloom get <ref> --layout flatten:1
```

Patterns match the member path inside the archive, with `/` separators, case sensitive. Four rules and nothing else: `*` is any run of bytes inside one path component, `**` alone as a component is any number of components, `?` is one byte, everything else is literal.

`--select data` picks a member called `data`. `--select data/**` picks what is under it.

Selecting nothing is an error, not a quiet no-op. It tells you how many members it considered and which of your patterns matched none.

## When your directory has changed

If you edited a file that the dataset also has, the run stops before writing anything:

```
destination.modified: run again with --force to overwrite the modified entries
and remove the foreign ones, or --adopt to accept the destination as it stands: a.txt (modified)
```

Two ways out today. `--force` throws your edits away and rewrites from upstream. `--adopt` keeps your edits and records the directory as it stands, which means the link back to upstream is gone.

Nothing is ever half reconciled. It stops before staging anything, so you are never left with a directory that is neither the old tree nor the new one.

## Making a run repeatable

The lock file pins exact digests and contains no paths from your machine. Commit it.

```
fetchloom get <ref> --locked
```

With `--locked` the run fails rather than fetching anything the lock does not describe. If upstream moved, you get `alias.unstable` naming what changed. If the dataset is not in the lock at all, you get `policy.trust_refused`, because a first fetch is not something a locked run is allowed to do.

That is your dataset version control. Check out last March's commit, run with `--locked`, and you get March's bytes or a loud failure.

## Working offline

Plan on a connected machine, carry the plan, run it somewhere with no network:

```
fetchloom plan <ref> > run.plan
fetchloom apply run.plan
```

A plan holds resolved digests, so it needs no lookups. To carry the bytes too:

```
fetchloom cache export data.bundle
fetchloom cache import data.bundle
```

A bundle is an uncompressed tar where every member is named by the digest of its own bytes. Import rehashes everything and never uses a member name as a path.

`--offline` forbids DNS, connections, probes and credential lookups. Anything needing one fails with `policy.offline` before doing avoidable work.

## When something is wrong

Failures print two lines. What to do, then the kind and exit code:

```
check that ./nope.tar names a path that exists
reference.unresolved, exit 10
```

The first line is for you, the second is for grep and for `if` statements in scripts. Add `--json` and you get one object on stdout instead.

Three commands for when you are stuck:

```
fetchloom doctor            checks this machine, changes nothing
fetchloom why <ref>         what this reference is, where it resolves, how far it is trusted
fetchloom explain           every setting and which level set it
```

`explain` is the one to reach for when a run behaved differently than you expected. It shows the value in force and whether it came from a flag, an environment variable, the project config, the user config, or the built in default.

## Credentials

Fetchloom never asks for a token at startup and never shows a setup screen. It asks at the moment a token would change the outcome, and it says what the change is.

Scoped per host, and dropped on any redirect to a different host:

```
FETCHLOOM_TOKEN_<HOST>=...
```

For sources that sign requests instead of sending a secret:

```
FETCHLOOM_ACCESS_KEY_<HOST>, FETCHLOOM_SECRET_KEY_<HOST>, FETCHLOOM_REGION_<HOST>
```

Two providers define a variable of their own, and Fetchloom reads it so you do not have to restate a token you already set:

```
KAGGLE_API_TOKEN=...    for Kaggle
GITHUB_TOKEN=...        for GitHub releases
```

Those hold the bare token the provider printed. `FETCHLOOM_TOKEN_<HOST>` holds the whole `Authorization` header instead, is read first, and is how you send a header that is not `Bearer`.

If a source needs a credential you do not have, the run stops and prints numbered steps: which page to open, which button to press, the narrowest permission that works, where to put the value, and the command that confirms it worked. Those steps assume you have never used that provider before.

Tokens, keys, `Authorization` headers, the userinfo part of a URL, and every query parameter value are stripped from logs, events, receipts, plans and error messages. Query values are removed as a class rather than by a list of known names, because a list can be incomplete.

## Configuration

Five levels. Highest wins.

```
command line  >  environment  >  fetchloom.toml  >  user config  >  default
```

`fetchloom.toml` is found by walking up from where you are standing until one turns up or the drive root is reached. User config lives at `%APPDATA%\Fetchloom\config.toml` or `~/.config/fetchloom/config.toml`.

```toml
sources = ["https://lab.edu/data/", "hf:datasets/acme/"]
cache = { dir = "D:/fetchloom-cache" }
hints = false
```

`sources` is what makes a bare name work. The name is appended to each base in order and the first that resolves wins.

Unknown keys are refused, not ignored. A typo in a config file is an error with the key named, never a setting that silently did nothing.

Turn config off entirely with `--no-config`, or use exactly one file with `--config <path>`.

## Tuning, if you ever need to

You should not need to. Every number it needs, it measures, and `explain` will show you what it picked.

If you want to override anyway, the knobs are `--concurrency`, `--per-host`, `--bandwidth`, `--retries`, `--timeout`, `--io`, `--durability`, and `--verify`. Every one of them is documented in [reference.md](reference.md).

Two rules hold no matter what you set. No optimization can be disabled in a way that also disables a correctness check. And nothing you tune can change the bytes, the digests, or the tree digest. Tuning changes how long it takes, never what you get.

## On Windows

Paths with spaces need quotes in both `cmd` and PowerShell:

```
fetchloom get <ref> --output "C:\My Data\corpus"
```

Forward slashes work everywhere and save you escaping trouble.

If a member of an archive has a name Windows will not accept, or the path would be too long for the volume, the run fails with `destination.unrepresentable` naming the member and what the volume said. It does not rename it for you, because that would give you a different tree than the same lock gives on Linux.

Defender inspects every write. On a cold fetch of many small files that is the largest cost in the run, and `doctor` reports the measured ratio so you can decide whether to add an exclusion. Fetchloom reports it and never works around it.

## Shell completion

```
fetchloom completions powershell >> $PROFILE
fetchloom completions bash > /etc/bash_completion.d/fetchloom
```

Also works for `zsh`, `fish` and `elvish`.

# guide

the things people do, in the order you hit them. for a lookup table instead, use
[reference.md](reference.md).

every output block below was copied from a real run. the runs were made in
`C:\work` on Windows, so the absolute paths in them are that machine's and yours
will be your own.

## your first run

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz -o hello
blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec  462 entries  C:\work\hello
```

you get a directory, the archive unpacked into it, and a `fetchloom.lock` beside
it recording exactly what you got. the line it printed is the digest of the
whole tree, the number of entries in it, and where it landed.

leave `-o` off and the directory is named for the reference itself, so that URL
lands in `./hello-2.12.tar.gz`. the extension is part of the name because the
name is the reference's own last segment, not a guess at what is inside.

run the same command again and it prints the same line and writes nothing. it
knows the directory is already correct because it can hash it and compare.
`--json` says which of the two happened:

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz -o hello --json
{"status":"unchanged","dataset":"hello-2.12.tar.gz","tree":"blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec","destination":"C:\\work\\hello","entries":462,"bytes":4539480,"work":{"bytes_read":0,"bytes_written":2035446,"requests":2,"file_operations":16},"trust":"tofu"}
```

`status` is one of `materialized`, `unchanged`, `restored` or `adopted`, and
never anything else.

## what you can point it at

all of these work with `get`, and you never have to say which kind it is.

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

eight providers are reached by their own identifier rather than by a location.

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

CKAN and Dataverse are not one site each. they are software many organizations
run, so the host is part of the reference. `ckan:data.gov.uk/...` and
`ckan:demo.ckan.org/...` go to different installations through the same adapter.

leave the tag off a GitHub release and it takes the latest one. every one of
these names the whole record. take part of it with `--select`.

if all you have is a DOI, give it the DOI.

```
fetchloom get doi:10.7910/DVN/OMV93V
```

it reads the registration, works out which provider holds the record, and tells
you what it decided. a DOI belonging to a provider it has no adapter for stops
and says which provider that is, rather than guessing at an API.

a folder, an object store prefix, a WebDAV directory, an FTP directory, or a
generated HTML index all expand to the files inside them. it lists what you
point at. it does not follow links out of that prefix and it never opens a file
to find more work.

## finding something by name

if you know what the dataset is called but not where it is, say the name.

```
fetchloom get ham10000
```

with no `sources` in your configuration it searches Hugging Face, Kaggle,
OpenML, Zenodo, Figshare, CKAN, Dataverse and DataCite at once, and one of three
things happens.

one place has it, so it prints what the name resolved to as a resolution alias
and gets on with the fetch.

several places have it, so it stops and shows you, because they may not be the
same data. copy whichever line you want.

```
$ fetchloom get ham10k
name one of them, because ham10k matched 4 records and a name is never guessed at:
  hf:datasets/dharmajitbaro/ham10k — ham10k, an unstated size, no checksum, so a first fetch is trusted on first use
    fetchloom get hf:datasets/dharmajitbaro/ham10k
  kaggle:devendraprasadpudi/ham10k — Ham10K, 54.8 MiB, no checksum, so a first fetch is trusted on first use
    fetchloom get kaggle:devendraprasadpudi/ham10k
  kaggle:mathlouthimoez/ham10k — HAM10K, 2.6 GiB, no checksum, so a first fetch is trusted on first use
    fetchloom get kaggle:mathlouthimoez/ham10k
  kaggle:sanskar6877/ham10k — ham10k, 2.6 GiB, no checksum, so a first fetch is trusted on first use
    fetchloom get kaggle:sanskar6877/ham10k
reference.unresolved, exit 10
```

nowhere has it, and it says so rather than reaching for something close.

```
$ fetchloom get zzqxwvnothingatall
name the location instead, because zzqxwvnothingatall matched nothing in the 8 registries this build searches and a name is never guessed at
reference.unresolved, exit 10
```

the name is never what goes in the lock. what it resolved to goes in, so the
first run searches and every run after it goes straight there.

## FTP, when that is what the data is on

much of public biology is still on FTP, so `get` speaks it.

```
fetchloom get ftp://ftp.ncbi.nlm.nih.gov/genomes/README.txt
fetchloom get ftp://ftp.ebi.ac.uk/pub/databases/ --output ebi
```

you log in as anonymous unless you say otherwise. an interrupted transfer
resumes from where it stopped.

`ftp://` tries to encrypt the connection and tells you when the server would not
let it. write `ftps://` and it will not go on unencrypted at all.

```
fetchloom get ftps://host/pub/reads.fastq.gz
```

no flag turns that off, and a password you set for a host is never sent over a
connection that could not be encrypted. if you need one, set it as
`user:password`.

```
setx FETCHLOOM_TOKEN_HOST_EXAMPLE "someone:their-password"
```

SFTP is a different protocol and this does not speak it.

## taking part of something

```
fetchloom get <ref> --select "**/*.txt"
fetchloom get <ref> --select "train/**" --exclude "**/*.tmp"
fetchloom get <ref> --layout flatten:1
```

patterns match the member path inside the archive, with `/` separators, case
sensitive. four rules and nothing else: `*` is any run of bytes inside one path
component, `**` alone as a component is any number of components, `?` is one
byte, everything else is literal.

`--select data` picks a member called `data`. `--select data/**` picks what is
under it.

selecting nothing is an error, not a quiet no-op. it tells you how many members
it considered and which of your patterns matched none.

## where things land

two places, and they do different jobs.

the destination is your directory. it is the real copy. the default is the
reference's own name beside you, and `--output` moves it.

the cache is shared storage of verified bytes, one per user, at
`%LOCALAPPDATA%\Fetchloom\Cache` on Windows or `~/.cache/fetchloom` on Linux.
ten projects fetching the same dataset download it once. you can delete the
whole cache any time and lose nothing but time.

if the cache is missing, full, or read only, the run says so and continues
without it.

## editing what you fetched

fetchloom records what it wrote into a directory, so it can always tell you how
that directory differs from it.

```
fetchloom get ./corpus-source -o corpus
```

edit a file, delete one, drop one of your own in, then ask:

```
$ fetchloom status corpus
deleted   notes/old.txt
added     scratch.parquet
modified  train/labels.csv
```

four states and no others: unchanged, modified, deleted, added. an unchanged
entry is not printed, so a directory that is exactly what the run left prints
nothing at all. entries come out sorted by path. it reads no bytes it does not
have to: where the size and timestamps still match what the run recorded, the
file is not hashed again.

`fetchloom diff corpus` prints the same states with the digest and the length on
each side.

```
$ fetchloom diff corpus
deleted   notes/old.txt  blake3:87b86a9f9e06007dc88bef0b92d8f046e2795cbdb25c211a4f2326570e2b820c 4 -> absent
added     scratch.parquet  absent -> blake3:ffaa7f53830b0e1744450c94db3c1264ffcd799e0131f9911529b30af4a87c16 2
modified  train/labels.csv  blake3:c42223f1fbf292f60491e1d0666e49af4b7eb75a63385041b98391acecf68562 8 -> blake3:71a5c4d915c3a30b5d1fbabe5767e7ac671e4999b914e7259f0a85fd877c9550 8
```

it never looks inside a file, because a parquet file and a JPEG have no lines.

to undo an edit:

```
$ fetchloom revert corpus train/labels.csv
1 restored  C:\work\corpus
```

the bytes come out of the cache, so this works with the network unplugged. name
no entry and everything changed goes back. if the cache no longer holds what an
entry came from, revert says so and names the command that would fetch it,
rather than quietly going to the network.

## when your directory has changed and upstream has not

if you edited a file the dataset also has, the run stops before writing
anything:

```
$ fetchloom get ./corpus-source -o corpus
run again with --force to overwrite the modified entries and remove the foreign ones, or --adopt to accept the destination as it stands: scratch.parquet (foreign)
destination.foreign, exit 60
```

that is what happens while upstream still serves what it served last time.
there is nothing new to give you, so the run refuses rather than choosing
between your work and its own. `--force` throws your edits away and rewrites
from upstream. `--adopt` keeps your edits and records the directory as it
stands. `fetchloom status` says which entries it is talking about, and
`fetchloom revert` puts one back.

nothing is ever half reconciled. it stops before staging anything, so you are
never left with a directory that is neither the old tree nor the new one.

## when upstream moves under your edits

this is the case the tool is really for. you fetched something, you changed part
of it, and the publisher released a new version.

```
fetchloom get ./corpus-source -o corpus
```

the record of the first run is the common ancestor, your directory is one side,
and what the reference resolves to now is the other. fetchloom decides per
entry.

- you did not touch it, upstream did: you get upstream's.
- you changed it, upstream did not: yours stays.
- you added a file: it stays.
- upstream added a file: you get it.
- you deleted a file upstream did not touch: it stays deleted.
- you both changed it: a conflict.

nothing merges the inside of a file, ever. a conflict writes upstream's version
beside yours:

```
$ fetchloom get ./corpus-source -o corpus
decide each of these yourself, because upstream and you changed the same entry and upstream's version is beside yours as <name>.upstream: train/labels.csv
destination.conflict, exit 60
```

your file is untouched. `train/labels.csv.upstream` is what upstream now serves.
the run exits 60 so a script notices. look at both, keep the one you want,
delete the other, and run again.

the whole thing is one publication. killed halfway, you have the old directory
or the new one, never something half merged.

`--force` still throws your edits away and takes upstream whole. `--adopt` still
accepts the directory as it stands.

## publishing what you edited

your edited copy can become a dataset of its own.

```
$ fetchloom promote corpus -o corpus-2024-06.yaml
corpus-source  3 artifacts  fetchloom.lock
```

that reads every file, keeps the bytes in the cache, writes a manifest naming
each file with both digests, and writes a lock pinning them. the manifest states
`derived_from`, so the thing you made remembers the thing you started with.
leave `-o` off and the manifest goes to stdout.

promote pins the bytes. it does not upload them. to move them to someone else:

```
$ fetchloom cache export corpus.bundle
5 objects  1017739 bytes  0 already held
```

that writes every object the cache holds, not only the ones this dataset needs,
which is why the count above is larger than the three files that were promoted.

they import the bundle and run against your manifest, and get byte for byte what
you had.

## making a run repeatable

the lock file pins exact digests and contains no paths from your machine. commit
it.

```
fetchloom get <ref> --locked
```

with `--locked` the run fails rather than fetching anything the lock does not
describe. if upstream moved, you get `alias.unstable` naming what changed. if
the dataset is not in the lock at all, you get `policy.trust_refused`, because a
first fetch is not something a locked run is allowed to do.

that is your dataset version control. check out last March's commit, run with
`--locked`, and you get March's bytes or a loud failure.

## a project, and one command that fetches all of it

put a `fetchloom.toml` at the top of your project and name what it needs:

```toml
[datasets]
imagenet = "acme/imagenet@2012"
eeg      = { ref = "https://lab.edu/eeg.yaml", output = "data/eeg" }
corpus   = { ref = "https://lab.edu/corpus.zip", select = ["train/*"] }
```

then, from anywhere inside the project:

```
fetchloom get
```

three datasets, in one command. `imagenet` lands at `./imagenet` beside that
file, `eeg` at `./data/eeg`, `corpus` at `./corpus` with only the training
members taken. paths resolve against the file, not against where you are
standing, so this works the same from the top of the tree and from four
directories down inside it. the lock lands beside the file too.

in CI, one line:

```
fetchloom get --locked
```

every dataset, pinned to exactly what the lock records, and a loud failure
rather than a quiet difference. that is the reproducible install.

two entries that would write to the same place fail before anything is fetched,
naming both. a key you misspelled inside an entry is an error naming the key.

## asking what is there, before taking it

```
$ fetchloom probe https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz
hello-2.12.tar.gz  1017723  tofu  ranges  cached  https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz
```

the size, every digest the source states, whether it serves byte ranges, how far
a fetch could be trusted, and whether your cache already holds it. it moves no
payload bytes. anything the source will not say comes back unknown rather than
guessed at, and unknown is still an answer: it exits 0. `--json` gives the same
facts as one object.

```
$ fetchloom list https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz
directory             0  hello-2.12/
file              93787  hello-2.12/ABOUT-NLS
file                580  hello-2.12/AUTHORS
file              35149  hello-2.12/COPYING
```

what is inside, without downloading it. a zip keeps its index at the end, so
over a source that serves ranges this costs a few small reads. a tar keeps no
index at all, so listing one means reading all of it: fetchloom says so before
it spends the bandwidth, and keeps what it read, so asking twice costs once.

## one place for datasets, and a path a script can read

a library is one directory holding materialized datasets, so no script has to
carry a path and two projects do not fetch the same bytes twice.

```
fetchloom get hf:datasets/org/name --library
fetchloom where hf:datasets/org/name
```

`where` prints the path and fetches nothing. the path is decided by what the
reference resolves to, so it is the same on every run and two versions of one
dataset sit beside each other. from Python:

```python
import subprocess, pathlib

path = pathlib.Path(
    subprocess.run(
        ["fetchloom", "where", "hf:datasets/org/name"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
)
rows = (path / "train.parquet").read_bytes()
```

if you want to fetch and be told the path in one step, `fetchloom get <ref>
--library --json` prints `destination`.

nothing is ever removed from the library on its own. `fetchloom library ls`
shows what it holds and what that costs you. `fetchloom library rm <path>`
removes one entry.

## working offline

plan on a connected machine, carry the plan, run it somewhere with no network:

```
fetchloom plan <ref> > run.plan
fetchloom apply run.plan --offline
```

a plan holds resolved digests, so it needs no lookups. that also means `plan`
refuses a reference nothing has resolved yet, with `policy.trust_refused`: run
`get` once so the lock pins it, then plan. to carry the bytes too:

```
fetchloom cache export data.bundle
fetchloom cache import data.bundle
```

a bundle is a tar where every member is named by the digest of its own bytes.
import rehashes everything and never uses a member name as a path.

`--offline` forbids DNS, connections, probes and credential lookups. anything
needing one fails with `policy.offline` before doing avoidable work:

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --offline
run the command again without --offline to reach https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz
policy.offline, exit 40
```

## when something is wrong

failures print two lines. what to do, then the kind and the exit code:

```
$ fetchloom get ./nope.tar
check that ./nope.tar names a path that exists
reference.unresolved, exit 10
```

the first line is for you, the second is for grep and for `if` statements in
scripts. add `--json` and you get one object on stdout instead.

three commands for when you are stuck:

```
fetchloom doctor            checks this machine, changes nothing
fetchloom why <ref>         what this reference is, where it resolves, how far it is trusted
fetchloom explain           every setting and which level set it
```

`doctor` writes one line per check, each saying `ok` or what is wrong, and
naming the thing it looked at: the configuration it found, whether the cache
matches this build's format, the free bytes on the volume holding it, whether it
can be written to, whether the platform trust store loaded, and one line per
provider credential it looked for. it changes nothing.

`explain` is the one to reach for when a run behaved differently than you
expected. it shows the value in force and whether it came from a flag, an
environment variable, the project config, the user config, or the built in
default. the first lines of it on a machine with no configuration at all:

```
$ fetchloom explain
project config: none found
user config: none found
offline = false (default)
threads = 16 (measured)
display = plain (default)
```

`measured` means it asked this machine rather than reading a number out of a
file. every setting is listed, and every one says where its value came from.

## credentials

fetchloom never asks for a token at startup and never shows a setup screen. it
asks at the moment a token would change the outcome, and it says what the change
is.

scoped per host, and dropped on any redirect to a different host:

```
FETCHLOOM_TOKEN_<HOST>=...
```

for sources that sign requests instead of sending a secret:

```
FETCHLOOM_ACCESS_KEY_<HOST>, FETCHLOOM_SECRET_KEY_<HOST>, FETCHLOOM_REGION_<HOST>
```

two providers define a variable of their own, and fetchloom reads it so you do
not have to restate a token you already set:

```
KAGGLE_API_TOKEN=...    for Kaggle
GITHUB_TOKEN=...        for GitHub releases
```

those hold the bare token the provider printed. `FETCHLOOM_TOKEN_<HOST>` holds
the whole `Authorization` header instead, is read first, and is how you send a
header that is not `Bearer`.

if a source needs a credential you do not have, the run stops and prints
numbered steps: which page to open, which button to press, the narrowest
permission that works, where to put the value, and the command that confirms it
worked. those steps assume you have never used that provider before.

tokens, keys, `Authorization` headers, the userinfo part of a URL, and every
query parameter value are stripped from logs, events, receipts, plans and error
messages. query values are removed as a class rather than by a list of known
names, because a list can be incomplete.

## configuration

five levels. highest wins.

```
command line  >  environment  >  fetchloom.toml  >  user config  >  default
```

`fetchloom.toml` is found by walking up from where you are standing until one
turns up or the drive root is reached. user config lives at
`%APPDATA%\Fetchloom\config.toml` or `~/.config/fetchloom/config.toml`.

```toml
sources = ["https://lab.edu/data/", "hf:datasets/acme/"]
cache = { dir = "D:/fetchloom-cache" }
library = { dir = "D:/datasets" }
hints = false
```

every relative path in that file is read as relative to the file, so
`cache = { dir = ".fetchloom" }` means the same thing from anywhere in the
project.

`sources` is what makes a bare name work. the name is appended to each base in
order and the first that resolves wins.

unknown keys are refused, not ignored. a typo in a config file is an error with
the key named, never a setting that silently did nothing.

turn config off entirely with `--no-config`, or use exactly one file with
`--config <path>`.

## tuning, if you ever need to

you should not need to. every number it needs, it measures, and `explain` will
show you what it picked.

if you want to override anyway, the knobs are `--concurrency`, `--per-host`,
`--bandwidth`, `--retries`, `--timeout`, `--io`, `--durability`, and `--verify`.
every one of them is documented in [reference.md](reference.md).

two rules hold no matter what you set. no optimization can be disabled in a way
that also disables a correctness check. and nothing you tune can change the
bytes, the digests, or the tree digest. tuning changes how long it takes, never
what you get.

## on Windows

paths with spaces need quotes in both `cmd` and PowerShell:

```
fetchloom get <ref> --output "C:\My Data\corpus"
```

forward slashes work everywhere and save you escaping trouble.

if a member of an archive has a name Windows will not accept, or the path would
be too long for the volume, the run fails with `destination.unrepresentable`
naming the member and what the volume said. it does not rename it for you,
because that would give you a different tree than the same lock gives on Linux.

Defender inspects every write. on a cold fetch of many small files that is the
largest cost in the run, and `doctor` reports the measured ratio so you can
decide whether to add an exclusion. fetchloom reports it and never works around
it.

## shell completion

```
fetchloom completions powershell >> $PROFILE
fetchloom completions bash > /etc/bash_completion.d/fetchloom
```

also works for `zsh`, `fish` and `elvish`.

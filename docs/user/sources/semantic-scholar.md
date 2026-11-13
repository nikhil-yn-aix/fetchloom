# semantic scholar

semantic scholar references list papers from its academic graph, each at the open-access pdf semantic scholar links, or the datasets of its releases, such as every paper's metadata or abstracts. papers and search need no account: its API answers anyone, and a key from semantic scholar gives you a rate of your own. the files of a dataset need the key.

```sh
fetchloom add s2:DOI:10.1371/journal.pcbi.1003285
fetchloom add s2:ARXIV:1706.03762
fetchloom search attention is all you need --source s2
fetchloom add s2:papers
```

## references

| spelling | behavior |
|---|---|
| `s2:204e3073870fae3d05bcbc2f6a8e263d9b72e776` | a paper, by semantic scholar's own id of 40 hexadecimal digits |
| `s2:DOI:10.1371/journal.pcbi.1003285` | a paper by its DOI |
| `s2:ARXIV:1706.03762` | a paper by its arXiv id, without a version |
| `s2:CorpusId:2180281` | a paper by its semantic scholar corpus id, as search offers it |
| `s2:PMID:24204232`, `s2:PMCID:3812051`, `s2:MAG:2036318837`, `s2:ACL:W12-3903` | a paper by its id in PubMed, PubMed Central, microsoft academic graph or the ACL anthology |
| `https://www.semanticscholar.org/paper/Ten-Simple-Rules-for-Reproducible-Sandve-Nekrutenko/12b056fd90c1300c44c4a8cff9f01f0c7a75cab4` | accept a paper's page |
| `s2:papers` | a dataset of semantic scholar's newest release, pinned at that release when it is added |
| `s2:papers@2026-11-10` | a dataset of one release |

a prefix is read in any case, `s2:doi:` as `s2:DOI:`, and a DOI is kept in lowercase. other pages of semantic scholar's site are refused with `reference.invalid`. the release `2026-11-10` held `abstracts`, `authors`, `citations`, `embeddings-specter_v1`, `embeddings-specter_v2`, `paper-ids`, `papers`, `publication-venues`, `s2orc`, `s2orc_v2` and `tldrs`; a name the release does not hold is refused with that list.

## one paper

a paper takes its title, DOI and pdf link from semantic scholar's API, and its license where semantic scholar states one. the dataset is named after the title, at most four words, and the pdf after the DOI, as publishers name their pdfs, or else after the corpus id. stderr from a real dry run:

```text
$ fetchloom add s2:DOI:10.1371/journal.pcbi.1003285 --dry-run
  Resolving s2:DOI:10.1371/journal.pcbi.1003285
   Resolved s2:12b056fd90c1300c44c4a8cff9f01f0c7a75cab4 · Ten Simple Rules for Reproducible Computational… · 1 file · CC-BY
      Would pin ten-simple-rules, new in data.lock: s2:12b056fd90c1300c44c4a8cff9f01f0c7a75cab4 · Ten Simple Rules for Reproducible Computational… · 1 file · size unknown
      Would fetch ten-simple-rules/journal.pcbi.1003285.pdf · size unknown
      Would place data/ten-simple-rules · 1 file
      Would add ten-simple-rules to data.toml, its files in data/ten-simple-rules
```

data.lock pins the pdf link semantic scholar gave, here the publisher's, and the bytes it served, so a teammate's sync downloads that link and asks semantic scholar nothing. semantic scholar's license names become the usual ones, `CCBYNC` as `CC-BY-NC`, so a license holding `-NC` or `-ND` gets the [warning](README.md#what-a-pin-records) every source's does. a license semantic scholar does not name, as `publisher-specific-oa`, is kept as it says it.

a paper on arXiv whose only pdf is arXiv's comes through the [arXiv](arxiv.md) source, from cornell's mirror where it can and at arXiv's pace: `s2:ARXIV:1706.03762` resolves to `arxiv:1706.03762@v7`, with arXiv's title and license.

a paper semantic scholar links no pdf of stops with `reference.unresolved`, exit 10, naming the page that says where it is published:

```text
$ fetchloom add s2:240bf8044739451452d2c30780d7d9f031d2d89c
  Resolving s2:240bf8044739451452d2c30780d7d9f031d2d89c
error: semantic scholar links no open-access pdf of "Deep learning" (s2:240bf8044739451452d2c30780d7d9f031d2d89c)
  help: its page, https://www.semanticscholar.org/paper/240bf8044739451452d2c30780d7d9f031d2d89c, says where it is published, which may need a subscription
```

so does a paper whose link is a DOI's, `https://doi.org/...`, which leads to a publisher's page rather than a pdf; the help names that link. a link semantic scholar gives may still lead to a page on some publishers' sites, or to a site that turns programs away: fetchloom pins what the link serves, so check a pdf you have not seen before you rely on it.

## search

`fetchloom search <words> --source s2` asks semantic scholar's relevance search and offers each paper by its corpus id. semantic scholar is searched only when `--source` names it, since its papers are not datasets. stdout from a real run:

```text
$ fetchloom search attention is all you need --source s2
s2:CorpusId:13756489	Attention is All you Need
s2:CorpusId:225068152	Attention Is All You Need In Speech Separation
s2:CorpusId:275470886	Tensor Product Attention Is All You Need
```

at a terminal each match also shows its first author and year, and a paper add would refuse says `no open-access pdf`.

## datasets

semantic scholar makes a release of its whole graph about once a week, as datasets of files on S3. `s2:<dataset>` takes its name, description and license from the newest release, which anyone may read, and its files from the links the API gives a key alone. without a key it stops with `auth.required`, exit 40, once the name is known good. stderr from a real run:

```text
$ fetchloom add s2:tldrs --dry-run
  Resolving s2:tldrs
error: semantic scholar gives the files of s2:tldrs@2026-11-10 only for an API key
  help: get a free key at https://www.semanticscholar.org/product/api#api-key-form, then sign in with `fetchloom auth login s2`, or set S2_API_KEY
```

with a key, the same dry run listed the 30 files in 5.0 s, its first two requests refused once each for load:

```text
$ fetchloom add s2:tldrs --dry-run
  Resolving s2:tldrs
warning: api.semanticscholar.org answered 429. trying again in 2.0 s (try 2 of 4)
warning: api.semanticscholar.org answered 429. trying again in 1.5 s (try 2 of 4)
   Resolved s2:tldrs@2026-11-10 · A short natural-language summary of the contents… · 30 files · ODC-By-1.0
      Would pin tldrs, new in data.lock: s2:tldrs@2026-11-10 · A short natural-language summary of the contents… · 30 files · size unknown
```

the API states no file's size, so a size is known once the file has downloaded. the dataset is named after its reference, never after a file, whose name is a time and an id. a whole dataset can be large: semantic scholar describes `tldrs` as 30 files of about 200 MB, and `papers` as 30 files of 200 million records, so check the description before you add one. the smallest, `publication-venues`, is one file:

```text
$ fetchloom add s2:publication-venues
  Resolving s2:publication-venues
warning: api.semanticscholar.org answered 429. trying again in 1.9 s (try 2 of 4)
   Resolved s2:publication-venues@2026-11-10 · Details about the venues in which papers are… · 1 file · ODC-By-1.0
Downloading publication-venues/20261113_123241_00043_gief7_83d699d7-c621-48bd-809a-7e4064a0a6e0.gz · 15.3 MiB
 Downloaded 1 file · 15.3 MiB in 599 ms
```

the links semantic scholar gives are signed and last a week, or less, since they carry a session token that may end first, so data.toml and data.lock never keep one. data.lock keeps the API's listing of the release's dataset, with each file's name after `#`:

```toml
files = [
  { path = "20261113_123241_00043_gief7_83d699d7-c621-48bd-809a-7e4064a0a6e0.gz", size = 16077278, sha256 = "8213ed458e0a131803e1ff2ecbcc0ce699735805cfe911562ca03b176b34057f", url = "https://api.semanticscholar.org/datasets/v1/release/2026-11-10/dataset/publication-venues#20261113_123241_00043_gief7_83d699d7-c621-48bd-809a-7e4064a0a6e0.gz" },
]
```

every download asks that listing for a fresh signed link, once a run, and takes the link to the file of that name; the pinned SHA-256 then checks the bytes. a link that is past its `Expires`, or that S3 answers has expired, as `Request has expired` or `ExpiredToken`, is asked for once more. a teammate with a key syncs the same files by their names; one without a key gets the refusal above. a file the release no longer lists stops with `reference.unresolved`.

`fetchloom export --format links` writes those listing links, which other tools cannot download: they need the key, and the file's own signed link from the listing.

### a newer release

`fetchloom lock --upgrade tldrs` pins the newest release in place of the one data.lock holds, and the next sync downloads its files in full; a dataset written with a release, as `s2:tldrs@2026-11-10`, takes a new one by editing its `ref` in data.toml. semantic scholar's API also gives diffs between two releases, to update a copy without downloading it all again; fetchloom does not read them, since it pins whole files.

## without a key and with one

| | without a key | with a key |
|---|---|---|
| papers and search | yes, at a rate every program without a key shares | yes, at your key's own rate |
| a dataset's name, description and license | yes | yes |
| a dataset's files | no | yes |
| requests to `api.semanticscholar.org` | one at a time, 3 s apart | one at a time, 1 s apart, the rate a new key gets |

semantic scholar's API answers anyone, but every program without a key shares one pool of requests, and it often answers 429, too busy. fetchloom then waits longer before each next request: about 3 to 6 seconds after the first refusal, 6 to 12 after the second, 12 to 24 after the third, each wait a random share of its range so programs refused together do not come back together. after the fourth refusal in a row it stops and asks semantic scholar nothing more in that run, so each retry is numbered of four. the last lines of stderr from a real run without a key:

```text
$ fetchloom add s2:CorpusId:2180281 --dry-run
warning: api.semanticscholar.org answered 429. trying again in 5.3 s (try 2 of 4)
warning: api.semanticscholar.org answered 429. trying again in 11.5 s (try 3 of 4)
warning: api.semanticscholar.org answered 429. trying again in 12.7 s (try 4 of 4)
error: api.semanticscholar.org refused one request after another because it is busy, so this run asks it nothing more
  help: every program without a key shares one pool of semantic scholar's requests: run `fetchloom add s2:CorpusId:2180281 --dry-run` again later, or get a free key at https://www.semanticscholar.org/product/api#api-key-form and sign in with `fetchloom auth login s2`, for a rate of your own
```

a key is free: ask for one with the form at [semantic scholar's API page](https://www.semanticscholar.org/product/api#api-key-form), which sends it by email. then sign in, or set `S2_API_KEY`, the variable semantic scholar's own examples read:

```sh
fetchloom auth login s2
```

the key goes to `api.semanticscholar.org` alone, in its `x-api-key` header, never to the links on S3 it gives for dataset files, or to a publisher's or arXiv's host. a key semantic scholar refuses stops with `auth.required`, exit 40, naming where the key comes from, as `the key in S2_API_KEY`, never its value.

the pdfs come from other hosts, a publisher's or arXiv's, each with fetchloom's usual limit of requests at once to one host, and arXiv's at its own pace. dataset files come from S3, which needs no pacing.

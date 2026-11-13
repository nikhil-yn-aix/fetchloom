# arXiv

arXiv references list the pdfs of papers on arXiv: one paper, at a version or its newest, the papers of a month, or the papers one category got in a month. no account is needed. each pdf comes from cornell's public mirror of arXiv on google cloud storage, `gs://arxiv-dataset`, where it holds that version, and otherwise from export.arxiv.org, the host arXiv keeps for programs.

```sh
fetchloom add arxiv:1706.03762
fetchloom add arxiv:cs.LG/2401
fetchloom search attention is all you need --source arxiv
```

## references

| spelling | behavior |
|---|---|
| `arxiv:1706.03762` | a paper, pinned at its newest version when it is added |
| `arxiv:1706.03762@v7`, `arxiv:1706.03762v7` | one version of a paper |
| `arxiv:hep-th/9901001` | a paper from before april 2007, by its archive and number; a subject class, as in `math.GT/0309136`, is dropped, since arXiv's API knows that paper as `math/0309136` |
| `https://arxiv.org/abs/1706.03762`, `https://arxiv.org/pdf/1706.03762v7` | accept a paper's page, its pdf or its html link, with the version the link names |
| `doi:10.48550/arXiv.1706.03762` | accept arXiv's DOI of a paper, which doi.org sends to the paper's page |
| `arxiv:2401` | the papers whose ids arXiv gave in january 2024, from the mirror |
| `arxiv:cs.LG/2401` | the papers of the category cs.LG whose first version was submitted in january 2024 |

`arXiv:1706.03762`, as citations write it, works too. a category is written as arXiv writes it, `cs.LG`, `hep-th` or `cond-mat.mes-hall`, in any case. arXiv's other pages, such as its list pages, are refused with `reference.invalid`.

## one paper

a paper takes its title, authors and newest version from arXiv's API, and its license from arXiv's OAI-PMH record. the dataset is named after the title, at most four words, since a file named `1706.03762v7.pdf` says little. the first lines of stderr from a real run:

```text
$ fetchloom add arxiv:1706.03762
  Resolving arxiv:1706.03762
   Resolved arxiv:1706.03762@v7 · Attention Is All You Need · 1 file · 2.1 MiB · http://arxiv.org/licenses/nonexclusive-distrib/1.0/
warning: the license http://arxiv.org/licenses/nonexclusive-distrib/1.0/ does not allow sharing copies
Downloading attention-is-all-you/1706.03762v7.pdf · 2.1 MiB
 Downloaded 1 file · 2.1 MiB in 235 ms
```

a paper arXiv does not have, or a version it does not have, stops with `reference.unresolved`, exit 10. so does a version that withdrew the paper, which has no pdf:

```text
$ fetchloom add arxiv:2401.01626
  Resolving arxiv:2401.01626
error: arxiv:2401.01626 was withdrawn at v2, which has no pdf
  help: its earlier versions stay on arXiv: add one by its version, as arxiv:2401.01626@v1
```

arXiv's API knows a paper from the day after arXiv announces it.

## months and categories

the mirror keeps every version of every paper in a folder for the month its id was given: `arxiv/arxiv/pdf/2401/` holds 30,011 pdfs of 18,082 papers. `arxiv:2401` lists each paper once, at the newest version the mirror holds, which for 2401 is 18,082 files and 57.3 GiB. to take every version instead, add the folder itself, which gets the name `pdf-2401`:

```sh
fetchloom add gs://arxiv-dataset/arxiv/arxiv/pdf/2401/
```

before april 2007 arXiv numbered papers within each archive, so `arxiv:9901` is refused and names the mirror's folder of one archive's month, such as `gs://arxiv-dataset/arxiv/hep-th/pdf/9901/`. a paper the authors withdrew after the mirror took it appears at its version before the withdrawal, since the mirror keeps no pdf of a withdrawal.

`arxiv:cs.LG/2401` asks arXiv's API for the papers of the category, cross-lists included, whose first version was submitted in that month, in GMT, as the API's only date filter dates them. the API sends them 1,000 at a time, and lists at most 30,000 for one query. arXiv's own list page of cs.LG for 2024-01 counts papers by their id instead, 2,488 against the API's 2,450: an id is given when a paper is announced, which can be weeks after it was submitted, as 2401.00001 was submitted on 2023-11-18. each paper comes at the newest version the API names, from the mirror when it holds that version. a paper the mirror holds only at an earlier version is checked in its OAI-PMH record: a paper whose newest version is a withdrawal is left out with a warning, and one revised since the mirror took it comes from export.arxiv.org. the first lines of stderr from a real dry run, which took 49 s: three pages from the API, then eleven records, each 3 s after the last.

```text
$ fetchloom add arxiv:cs.LG/2401 --dry-run
  Resolving arxiv:cs.LG/2401
  Resolving arxiv:cs.LG/2401 · 10 s
  Resolving arxiv:cs.LG/2401 · 20 s
  Resolving arxiv:cs.LG/2401 · 30 s
  Resolving arxiv:cs.LG/2401 · 40 s
   Resolved arxiv:cs.LG/2401 · arXiv cs.LG, January 2024 · 2,440 files
warning: left out 10 papers whose newest version is a withdrawal, which has no pdf, as arxiv:2401.01626@v2; an earlier version can be added by its id
warning: the mirror does not hold 1 paper at its newest version yet, so it comes from export.arxiv.org, one every 3 seconds
warning: most arXiv papers come under arXiv's own license, which does not allow sharing copies; each paper's page names its license
```

`arxiv:2401 --dry-run` took 4.1 s, the mirror's listing of 30 pages. the mirror takes in new papers about once a week: on 2026-11-13 its newest pdfs were from 2026-11-08, and its folder for 2611 held 8,489 papers. a month is partial until the mirror catches up, and a category's newest papers come from export.arxiv.org, one every 3 s.

## pins

| reference | `resolved` | file `url` |
|---|---|---|
| a paper | `arxiv:<id>@v<n>` | the mirror's `https://storage.googleapis.com/arxiv-dataset/arxiv/arxiv/pdf/<month>/<id>v<n>.pdf?generation=<n>`, with the MD5 it states, else `https://export.arxiv.org/pdf/<id>v<n>` |
| a month or a category's month | `arxiv:2401`, `arxiv:cs.LG/2401` | the same, for each paper |

a file is named as arXiv names it, `1706.03762v7.pdf`, and an old id keeps its archive, `hep-th9901001v3.pdf`. a paper also records its title, its license and its DOI, `10.48550/arXiv.1706.03762`.

the mirror's pdf of a version is not always arXiv's: arXiv builds a pdf again from time to time, and the mirror keeps the build it copied. both copies of 1706.03762v7 are 2,215,244 bytes, but the mirror's was made on 2023-08-03 and arxiv.org's on 2024-04-10, so their SHA-256 differ, in 145 bytes of dates. data.lock keeps the link each file's bytes came from, so a sync never asks the other place for them. a pin of the mirror's copy names its generation, which nobody changes; a pin from export.arxiv.org stops with `integrity.mismatch`, exit 30, if arXiv builds that pdf again later: [push](../commands/push.md) the pinned bytes to keep them, or run [`lock --upgrade`](../commands/lock.md#upgrading), which takes the mirror's copy once it holds the paper.

## licenses

each paper keeps the license its authors chose for it. most choose arXiv's own, http://arxiv.org/licenses/nonexclusive-distrib/1.0/, which "gives limited rights to arXiv to distribute the article, and also limits re-use of any type from other entities or individuals", as [arXiv's license page](https://info.arxiv.org/help/license/index.html) says. so a paper under it gets a warning, as `-NC` and `-ND` licenses do, and its `access` is left out of data.lock, so [push](../commands/push.md#data-you-may-not-share) asks before copying it into a mirror. a paper under a creative commons license records `access = "public"`. a month or a category's month records no license, since each paper has its own, and warns once. papers from before 2004 state none.

## arXiv's terms

arXiv asks a program to send no more than one request every three seconds, on one connection at a time, to its API and its OAI-PMH interface, as [its API terms](https://info.arxiv.org/help/api/tou.html) say, and to use export.arxiv.org rather than arxiv.org. fetchloom sends every request to arxiv.org and its hosts, pdfs included, one at a time and three seconds apart; the mirror on google's storage is not paced. it never reads arXiv's list pages, and it does not fetch a paper's latex source: arXiv's robots.txt keeps programs out of `/e-print` and `/src`, and its terms ask tools to send people to arXiv for source files. download a paper's source from its page, or many from arXiv's [bulk data on S3](https://info.arxiv.org/help/bulk_data_s3.html), whose bucket bills the reader, which fetchloom [never agrees to](buckets.md).

## search

`fetchloom search <words> --source arxiv` asks arXiv's API for papers whose titles hold the words as a phrase, the most relevant first, then, when fewer than the limit do, for papers that hold them anywhere. arXiv is searched only when `--source` names it, since its papers are not datasets. stdout from a real run:

```text
$ fetchloom search attention is all you need --source arxiv
arxiv:1706.03762	Attention Is All You Need
arxiv:2104.04692	Not All Attention Is All You Need
arxiv:2501.05730	Element-wise Attention Is All You Need
```

## other scholarly data

OpenAlex's snapshot of every work it knows is in its public bucket, which [cloud buckets](buckets.md) lists like any other: `fetchloom add s3://openalex/data/jsonl/works/` lists 1,900 files, 629.1 GiB, in under a second.

# search

`search` finds public datasets by name without downloading their files.

```sh
fetchloom search iris
fetchloom search fashion mnist
fetchloom search iris --source kaggle,zenodo --limit 5
fetchloom search iris --json
```

## usage

```sh
fetchloom search [options] <words>...
```

words may be quoted or separate arguments. no words at all is a usage error, exit 2, before any request.

| option | meaning |
|---|---|
| `--source <source>` | search only this site: `hf`, `kaggle`, `zenodo`, `ena`, `geo`, `dandi`, `openneuro`, `arxiv` or `s2`. repeat it or separate names with commas. without it, every site but `ena`, `dandi`, `arxiv` and `s2`, which are searched only when named. any other name is a usage error |
| `--limit <n>` | keep up to n matches from each site, 1 to 20, and show them all. without it, each site gives up to 3 and at most 12 show, as [results](#results) says; any other value is a usage error, exit 2 |

[global options](global-options.md) apply.

## results

fetchloom asks hugging face, kaggle, zenodo, GEO and openneuro at once, or only the `--source` sites, and keeps up to `--limit` matches from each. ENA and dandi, specialist archives slow to answer, need `--source`, as `--source ena` for sequencing studies or `--source dandi` for neurophysiology, so a search without it stays quick. so do arXiv, `--source arxiv`, and semantic scholar, `--source s2`, since their papers are not datasets. the sites interleave by rank, each site's first match, then each second, and so on; matches whose title or last id part equals the search words, ignoring case and treating spaces, `-` and `_` alike, then move to the front in that order. this is each site's own order, not a common relevance score: hugging face lists the most downloaded first, zenodo the most viewed, dandi the most starred, and the others their own way.

without `--limit`, at most 12 matches show, so the list and the picker fit a terminal 24 rows high: each site's best, then matches named as the search words, then each site's next by rank, as interleaved. a hint says when some were left out, and `--limit` shows every match it asks for. JSON keeps the same 12.

each site finds its own kind of dataset. ENA's matches are studies, found through EBI's search, and adding one adds every run in it. each ENA study is offered by the files `add` would take from it, as ENA's report of its first 100 runs tells: `ena:<id>` when one has FASTQ files, `ena:submitted/<id>` when they have only the files their submitter sent, and nothing when they have neither, as a study of sequences or analyses alone, or one whose runs have no files yet or are all under controlled access. the reports are asked for together as ENA answers, one try each, within the grace the sites get, which for ENA named alone starts with its own answer: a study whose report has not come back by then is offered as `ena:<id>`, for `add` to try. GEO's are series, found through NCBI's E-utilities, which take a second request for their titles. dandi leaves out embargoed dandisets and ones with no assets yet, and describes one never published by its draft. openneuro leaves out the private datasets it finds and ones with no snapshot yet. arXiv's are papers, found through arXiv's API by their titles first, then by every field when fewer than the limit hold the words in their title, each with its first author and the year as its note; arXiv states no size, and its requests go 3 s apart, as its [terms](../sources/arxiv.md#arxivs-terms) ask. semantic scholar's are papers too, found by its relevance search. the `Found` line counts papers apart from datasets, as `Found 3 papers` or `Found 1 dataset and 2 papers`, and a search that found nothing says what it looked for, as `found no paper matching "…"`.

at a terminal, each match is one row with its reference, then title, size, download count and access note in columns, and no number, since `add` takes the reference, not a row's place in the list. a hugging face row has no title, since its title is the repository id the reference already shows. from a real run at a 100-column terminal, shortened:

```text
      Found 12 datasets for "iris"
            hf:datasets/scikit-learn/iris                                    18.1 KiB
            kaggle:uciml/iris                            Iris Species         3.6 KiB
            zenodo:23233774                              IRIS: ICESat-2…      1.1 GiB
  hint: each site's best matches are shown, 12 in all; `--limit 20` shows up to 20 from each
```

the reference is never cut. on a narrow terminal the download count gives way first, then the title; the size and the access note move to a line of their own when they do not fit. a fact the site does not give stays blank, as ENA and GEO give no size or download count, and dandi no download count. sizes are as each site lists them: hugging face gives the size of a dataset's files, while kaggle gives the size of its zipped download, so the files `add` reports can be larger. kaggle lists a dataset it does not serve as files, such as one kept in bigquery, with 0 bytes and 0 downloads, which fetchloom shows as unknown.

a paper keeps its title, which is how people know it: its first author and year give way first, and the title goes on a line under the reference when even it has no room beside it. from a real run at a 40-column terminal, shortened:

```text
      Found 4 papers for
            "attention is all you need"
            arxiv:1706.03762
            Attention Is All You Need
            arxiv:2104.04692
            Not All Attention Is All…
```


with `-q`, a redirected stdout, or no interactive terminal, as under CI or with `TERM=dumb`, stdout gets one match per line: its reference, a tab, its title and, when the site gives a size, a tab and the size, so `cut -f1` reads the references. a control character in a title reads as a space, and a bidi control, which could turn the line around on a terminal, as its escape, such as `\u{202e}`. JSON returns `result.candidates`; [JSON output](../reference/json-output.md#search-candidates) defines the fields. figshare, osf, dataverse and the other sources have no keyword search; pass their links or ids to `add` and `get`.

a site that fails gives a warning while the others can still return matches. once one site has answered, the others have 3 s more, so a search ends in about 3 s however slow one site is; sites named with `--source` have 10 s more, and one named alone is waited for. one still silent then is left out with a warning that names the command searching it alone, as `fetchloom search "iris" --source zenodo`. if every site fails, the first error ends the command. no matches fails with `reference.unresolved` and an empty candidate list.

## choosing a dataset

`add` and `get` search when their argument is plain words rather than a link, id or bare DOI:

```sh
fetchloom add iris
fetchloom get "fashion mnist"
```

a search is the only argument: several plain words, quoted or not, are one search, as `fetchloom add fashion mnist`, while words mixed with a link, an id or a DOI are refused, since `add` and `get` then read each as a link. each word is checked on its own, so one that names a file or folder here stops the search before any request.

paths never reach a search site. a path such as `./flowers.csv`, `~/data` or `file:///data` is imported by `add`, refused by `get` with `reference.invalid`, exit 10, and refused by `search` with `usage`, exit 2. a plain word that names a file or folder in the current folder, such as `iris` beside an `iris` folder, makes `add` and `get` exit 2; `fetchloom search iris` still searches for it. so does a word written as a path that names nothing here, as a typo would: one that ends with `/`, holds two `/` or a `\`, or ends with a file type such as `.csv` or `.zip`. `add` names the near file or folder, and both say how to search for the words on purpose, which only `search` does. stderr, beside a `flowers` folder:

```text
error: there is no file or folder "flowerz/" here
  help: did you mean `fetchloom add ./flowers`? or run `fetchloom search flowerz/` to search for these words
```

with interactive stdin and stderr, an arrow-key picker asks which dataset you mean, in the same columns. up/down or j/k moves, enter selects the current row, and 1-9 selects a numbered row. q or esc cancels with exit 2, and ctrl-c with exit 130; either way stderr shows `Cancelled the choice of a dataset, so nothing changed`, not an error. `--yes` approves downloads but never selects a dataset.

with `FETCHLOOM_ACCESSIBLE=1` at a terminal, as for a screen reader, the same rows come once, numbered, and nothing on the screen moves: type the number of a row and press enter. a number not on the list asks again, and q cancels, as above. from a real run at a 100-column terminal, shortened:

```text
  Searching hugging face, kaggle, zenodo, GEO and openneuro for "iris"
  which one?
     1 hf:datasets/scikit-learn/iris                                         18.1 KiB
     2 kaggle:uciml/iris                            Iris Species              3.6 KiB
     3 zenodo:23233774                              IRIS: ICESat-2 River…     1.1 GiB
    more matches: fetchloom search iris --limit 20
  type a number from 1 to 12 and press enter, or q to cancel: 2
      Chose kaggle:uciml/iris
```

without an interactive terminal, or with `--json`, fetchloom refuses to choose, even a single match: it exits 10 with `reference.ambiguous` and keeps the matches in `result.candidates`. stderr lists them first, in rows that fit the terminal, then says why nothing was added; with `-q` or `--json` the error's details list them instead. from a real run at a 100-column terminal with `TERM=dumb`, shortened:

```text
  Searching hugging face, kaggle, zenodo, GEO and openneuro for "iris"
      Found 12 datasets for "iris"; add one by its id:
            hf:datasets/scikit-learn/iris                                    18.1 KiB
            kaggle:uciml/iris                            Iris Species         3.6 KiB
            zenodo:23233774                              IRIS: ICESat-2...    1.1 GiB
error: "iris" is not a link or an id, and fetchloom will not guess which dataset you mean
  help: run it again with one of these, such as `fetchloom add hf:datasets/scikit-learn/iris`
```

run it again with the reference you mean; `data.toml` records that reference, not the search words.

## offline and failures

search needs the internet: `--offline` fails with `offline.missing`, exit 20, and help to search again online. knowing an id does not make a new dataset available offline; see [offline](../concepts/offline-and-recovery.md#offline).

[exit codes](../reference/exit-codes.md) covers failures of the search sites and the network.

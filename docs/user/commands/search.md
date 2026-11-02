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

words may be quoted or separate arguments. empty or whitespace-only words are a usage error, exit 2, before any catalog request.

| option | meaning |
|---|---|
| `--source <source>` | ask only this catalog: `hf`, `kaggle` or `zenodo`. repeat it or separate names with commas. any other name is a usage error |
| `--limit <n>` | keep up to n matches from each catalog, 1 to 20. the default is 3 |

[global options](README.md#global-options) apply.

## results

fetchloom asks hugging face, kaggle and zenodo concurrently, or only the `--source` catalogs, and keeps up to `--limit` matches from each, three by default. it interleaves the catalogs by rank: each catalog's first match, then each second match, and so on. matches whose title or last id part equals the search words, ignoring case and treating spaces, `-` and `_` alike, then move to the front in that same order. this is catalog order, not a common relevance score.

at a terminal, the story shows canonical references, titles and any known size, download count or access note. quiet or redirected stdout prints one match per line: its reference, a tab, its title and, when the catalog gives a size, a tab and the size. the reference stays the first column, so `cut -f1` or `awk '{print $1}'` still reads it. hugging face search gives no sizes, so its lines have none; when stderr is not a terminal either, the story keeps only the count, so captured output lists each match once. JSON returns `result.candidates`; [JSON output](../reference/json-output.md#search-candidates) defines the fields. figshare, osf, dataverse and the other sources have no keyword search; pass their links or ids to `add` and `get`.

one failed catalog produces a warning while the others can still return matches. if every catalog fails, the first catalog error ends the command. no matches fails with `reference.unresolved` and an empty candidate list. search writes no project files.

## choosing a dataset

`add` and `get` search when their argument is plain words rather than a link, id or bare DOI:

```sh
fetchloom add iris
fetchloom get "fashion mnist"
```

`add` and `get` take one reference argument, so quote a multi-word search. extra bare words produce a usage error with a quote hint.

with interactive stdin and stderr, an arrow-key picker asks which dataset you mean. up/down or j/k moves; enter selects the current row; 1-9 selects a numbered row. q or esc cancels with exit 2 and `usage`; ctrl-c exits 130. `--yes` approves downloads but never selects a dataset.

without an interactive terminal, or with `--json`, fetchloom refuses to choose even a single match. it exits 10 with `reference.ambiguous`, lists candidates in error details and preserves them in `result.candidates`. rerun with the intended canonical reference. a successful `add` stores that reference in `data.toml`, not the search words.

## offline and failures

search needs the internet. `--offline` fails with `offline.missing`, exit 20, and a message explaining that search needs a connection. the recovery help says: "search again online. offline, `fetchloom sync --offline` places what a project already pinned". knowing an id does not make a new dataset available offline; follow [offline preparation](../concepts.md#offline).

[exit codes](../reference/exit-codes.md) covers catalog and network failures.

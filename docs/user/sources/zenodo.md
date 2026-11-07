# zenodo

zenodo references resolve to a concrete record whose selected files are pinned by size and SHA-256.

```sh
fetchloom add zenodo:3608990
fetchloom get doi:10.5281/zenodo.3608990
fetchloom add zenodo:3608990 --as flowers --select "*.csv"
```

## references and selection

| reference | behavior |
|---|---|
| `zenodo:3608990` | list the concrete record |
| `https://zenodo.org/records/3608990` | accept a record page |
| a zenodo file-page link | select that file unless you supply `--select` |
| a DOI routed to zenodo | use the same record resolver |

patterns match listed paths. listing limits and access checks apply before filtering.

## search

`fetchloom search` asks zenodo for datasets whose titles hold every search word, most viewed first. whitespace, including pasted nonbreaking spaces, separates words. when no title holds them all, it asks again for titles with any of the words.

## pins

`resolved` is `zenodo:` followed by the concrete record id. the answer must name the requested record, or identify the requested concept as the parent of that record. unrelated record ids are refused. download URLs use the record's API file-content endpoint. fetchloom checks stated MD5 while learning SHA-256 and size.

zenodo permits [minor file corrections](https://help.zenodo.org/docs/deposit/manage-files/#modify) within 30 days after publication, with republication within 45 days, without changing the DOI. resolving the same record again, including after a filter change, can therefore pin corrected bytes. review the lock diff before committing it. an unchanged locked sync still requires the original size and hashes.

source metadata keeps its casing. the iris record supplies `license = "cc-by-4.0"`. hashes and sizes identify the selected bytes; they do not guarantee that the host will keep serving them.

## credentials and limits

public records need no account. for a restricted, embargoed or closed record, request access on its zenodo page, then sign in with `fetchloom auth login zenodo`, or set `ZENODO_TOKEN`. zenodo lists such a record's files only to an account allowed to read them, so fetchloom downloads them when the key can, and otherwise stops with `auth.required` and the record's page. selection filters do not bypass that check. keys are kept out of the manifest, lock, logs and JSON output.

use [lock upgrades](../commands/lock.md#upgrading) deliberately. changing a pinned input requires a new lock and a sync, followed by committing the lock.

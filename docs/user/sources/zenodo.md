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

## pins

`resolved` is `zenodo:` followed by the concrete record id. download URLs use the record's API file-content endpoint. fetchloom checks stated MD5 while learning SHA-256 and size.

source metadata keeps its casing. the iris record supplies `license = "cc-by-4.0"`. hashes and sizes identify the selected bytes; they do not guarantee that the host will keep serving them.

## credentials and limits

`ZENODO_TOKEN` supplies the source credential. tokens are kept out of the manifest, lock, logs and JSON output.

restricted, embargoed and closed records are refused. selection filters do not bypass that check. request a public accessible copy from the owner when needed.

use [lock upgrades](../commands/lock.md#upgrading) deliberately. changing a pinned input requires a new lock and a sync, followed by committing the lock.

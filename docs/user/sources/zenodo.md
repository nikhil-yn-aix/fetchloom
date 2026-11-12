# zenodo

a zenodo reference names a record, and its selected files are pinned by size and SHA-256.

```sh
fetchloom add zenodo:3608990
fetchloom get doi:10.5281/zenodo.3608990
fetchloom add zenodo:3608990 --as flowers --select "*.csv"
```

## references and selection

| reference | behavior |
|---|---|
| `zenodo:3608990` | list the record |
| `https://zenodo.org/records/3608990` | accept a record's page |
| a link to a file's page on zenodo | select that file, unless `--select` is given |
| a DOI zenodo registered | list the record it names |

a record may hold one large archive cut into parts, such as `scans.zip.001` to `scans.zip.003`. they unpack as that one archive when the dataset lists every part; a `--select` that keeps only some of them leaves them packed with a warning, as [split archives](../concepts.md#split-archives) explains.

## search

`fetchloom search` asks zenodo for datasets whose titles hold every search word, most viewed first. whitespace, including pasted nonbreaking spaces, separates words. when no title holds them all, it asks again for titles with any of the words.

## pins

`resolved` is `zenodo:` and the record's id. zenodo's answer must name the record asked for, or name the concept asked for as that record's parent; any other record is refused. files download from the record's API, and the MD5 zenodo states is checked while SHA-256 and size are learned.

zenodo lets owners make [minor file corrections](https://help.zenodo.org/docs/deposit/manage-files/#modify) within 30 days of publication, and republish within 45, without a new DOI. so resolving a record again, also after a filter change, can pin corrected bytes: read the lock's diff before you commit it. a sync of an unchanged lock still wants the pinned size and hashes.

metadata keeps zenodo's casing, so the iris record gives `license = "cc-by-4.0"`.

## credentials and limits

public records need no account. for a restricted, embargoed or closed record, request access on its zenodo page, then sign in with `fetchloom auth login zenodo`, or set `ZENODO_TOKEN`. zenodo lists such a record's files only to an account allowed to read them, so fetchloom downloads them when the key can, and otherwise stops with `auth.required` and the record's page. selection filters do not bypass that check.

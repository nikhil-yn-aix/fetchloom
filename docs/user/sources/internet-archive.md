# internet archive

an internet archive reference names an item, and its files are pinned by size and SHA-256, each checked against the MD5 archive.org states.

```sh
fetchloom add ia:iris_20240626
fetchloom add https://archive.org/details/iris_20240626 --as iris --select "*.csv"
fetchloom get ia:iris_20240626
```

## references

| reference | behavior |
|---|---|
| `ia:iris_20240626` | list the item's files |
| `https://archive.org/details/iris_20240626` | accept an item's page |
| `https://archive.org/download/iris_20240626/` | accept an item's folder of downloads |

an identifier is written as archive.org writes it, since it tells capitals apart. a link to one file, as `https://archive.org/download/iris_20240626/iris.csv`, stays an ordinary [URL](urls-and-dois.md), since such a link may also name a file inside one of the item's zips. another page of an item, as `https://archive.org/details/iris_20240626/iris.csv`, is refused with `reference.invalid`; so is a collection, which gathers items and holds no data of its own: add its items one by one.

## which files

the files come from archive.org's metadata api, `https://archive.org/metadata/<identifier>`, each at its path in the item, with `--select` and `--exclude` as for any dataset. two kinds are left out:

| left out | why |
|---|---|
| a file the item keeps private, such as a lending library book's scans | archive.org serves it to nobody without its permission; a `source.partial` warning names it |
| archive.org's own records of the item: its `_meta.xml`, `_files.xml`, `_meta.sqlite`, `_reviews.xml`, its torrent and its tile | archive.org remakes them whenever the item changes, so a pin of them would stop matching |

an item whose every file is private, and one archive.org has taken down, stop with `reference.unresolved`, exit 10.

## pins

`resolved` is `ia:` and the identifier; each file's `url` is its link under `https://archive.org/download/`, which sends a download on to a server that holds the item. archive.org states an MD5 and a size for each file, so a download is checked against both while its SHA-256 is learned. the item itself has no versions: an uploader may replace a file, so resolving the item again can pin other bytes, while a sync of an unchanged lock still wants the pinned ones. `access` is `public`, since only files anyone may download are listed.

| source | `resolved` | file `url` |
|---|---|---|
| internet archive | `ia:<identifier>` | `https://archive.org/download/<identifier>/<path>` |

## limits

fetchloom asks archive.org without an account, so files an item lends or keeps private are out of reach, and so is the item's search: give the identifier from the item's page.

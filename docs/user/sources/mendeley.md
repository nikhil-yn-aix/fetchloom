# mendeley data

mendeley data references pin one published version of a dataset, and check each file against the SHA-256 mendeley states for it. published versions need no account.

```sh
fetchloom add mendeley:rscbjbr9sj@2 --select "code2017.zip"
fetchloom add https://data.mendeley.com/datasets/rscbjbr9sj/2 --as oct-code --select "code2017.zip"
fetchloom get doi:10.17632/rscbjbr9sj.2 --select "code2017.zip"
```

## references

| spelling | behavior |
|---|---|
| `mendeley:rscbjbr9sj` | list the newest published version |
| `mendeley:rscbjbr9sj@2` | list version 2 |
| `https://data.mendeley.com/datasets/rscbjbr9sj/2` | accept a dataset page, with or without its version |
| `doi:10.17632/rscbjbr9sj.2` | follow doi.org to the version's page, then list it the same way |

the id is the part of the dataset's link after `/datasets/`, letters and digits, and its case does not matter. a version mendeley does not have is refused with `reference.unresolved`.

a file's page on data.mendeley.com and other site pages are refused with `reference.invalid`; paste the dataset's page and choose files with `--select`. a file's own download link, under `/public-files/`, stays an ordinary [URL](urls-and-dois.md).

## pins

`resolved` is `mendeley:<id>@<version>`: the newest published version when the reference names none. such a reference moves to a newer version only through [`lock --upgrade`](../commands/lock.md#upgrading); one that names a version keeps naming it. each file's `url` is its public download link, `https://data.mendeley.com/public-files/datasets/<id>/files/<file id>/file_downloaded`, which sends the download on to mendeley's storage.

downloads check the SHA-256 mendeley states, then the lock records it with each file's size; bytes that differ stop with `integrity.mismatch` and are never placed. the license is mendeley's short name for it, such as `license = "CC BY 4.0"`, the DOI is the version's own, such as `10.17632/rscbjbr9sj.2`, and `access` is `public`.

files keep the folders mendeley shows them in, below the dataset's folder. each folder is listed in parts until the listing holds as many files as mendeley counts for it; a listing that ends sooner, or names a file twice, is refused with `source.invalid` rather than shortened. a file mendeley gives no download link for is listed as one fetchloom cannot download, so `--exclude` can leave it out.

## access and limits

fetchloom reads the public API that mendeley's own site uses, which lists any published version to anyone. the API mendeley documents asks every caller to register an OAuth client with mendeley first, so fetchloom does not use it, and it has no mendeley key to send. the public API's routes and fields match the documented API's, but mendeley does not promise to keep them; a [weekly live check](../../dev/engineering/ci.md#live-source-checks) notices when they change.

a version under an embargo stops with `auth.required`, exit 40, naming the dataset's page, which says when the embargo ends; no sign-in lifts it. mendeley is not in [search](../commands/search.md).

# DANDI

DANDI references list the assets of a dandiset, a neurophysiology dataset in the DANDI archive, at one version through dandi's API, and check each against the SHA-256 dandi states for it. public dandisets need no account; embargoed ones are refused.

```sh
fetchloom add dandi:000027
fetchloom add dandi:000027@0.210831.2033
fetchloom get "https://dandiarchive.org/dandiset/001512/0.250624.1820/files?location=sub-mouse9%2F"
```

## references

| spelling | behavior |
|---|---|
| `dandi:000027` | the newest published version, else the draft of a dandiset never published |
| `dandi:000027@0.210831.2033` | that published version |
| `dandi:000027@draft` | the draft, which its owners can still change |
| `https://dandiarchive.org/dandiset/000027`, with `/<version>` after it | the dandiset's page, at the version it names |
| `https://dandiarchive.org/dandiset/<id>/<version>/files?location=<folder>` | the page of a folder in its file browser: the dandiset at that version, with `--select "<folder>/**"` |
| `doi:10.48324/dandi.000027/0.210831.2033` | a published version's DOI, which leads to its page |

a dandiset's number has six digits, and `DANDI:000027` reads as `dandi:000027`. a version is `draft` or a published one, as `0.210831.2033`.

```console
$ fetchloom add dandi:000027 --as rat
  Resolving dandi:000027
   Resolved dandi:000027@0.210831.2033 · Test dataset for testing dandi-cli. · 1 file · 18.4 KiB · CC0-1.0
Downloading rat/sub-RAT123/sub-RAT123.nwb · 18.4 KiB
 Downloaded 1 file · 18.4 KiB in 224 ms
      Added rat, its files are in data/rat
```

## pins

`data.lock` keeps the version it read, as `resolved = "dandi:000027@0.210831.2033"`, with the dandiset's title, license and DOI, and each asset's path, size, SHA-256 and link. dandi states a SHA-256, `dandi:sha2-256`, for nearly every asset; the downloaded bytes must match it, and an asset without one is pinned by the SHA-256 of its bytes. dandi's other digest, `dandi:dandi-etag`, is S3's tag for a file uploaded in parts and no MD5, so fetchloom never reads it as one.

an asset's link is its blob on dandi's S3 bucket, `https://dandiarchive.s3.amazonaws.com/blobs/...`, which holds the same bytes for as long as it exists; dandi's own download link, which leads there, is used for an asset that names no blob link. a published version never changes. a draft can, so `dandi:<id>` of a dandiset never published warns that its draft is read, and the lock's SHA-256 names the bytes.

## listing

the assets come from `api.dandiarchive.org`, 1,000 to a page, each page by the link the one before gives, which must be this version's listing; a page that names an earlier page, or leads elsewhere, is refused. a version that lists more assets than `most_files` stops before its pages are read. a dandiset is asked about once, for its newest version, unless the reference names one, then its version once, for its title, license and DOI.

## access and limits

a dandiset under embargo is shown only to the people its owners let in. dandi turns everyone else away with 401, which fetchloom reports as an embargo, `auth.required`, exit 40, since it does not sign in to dandi: no key goes to dandi, and none is read. a dandiset leaving its embargo is refused until dandi opens it.

an asset stored as zarr, a folder of many small chunks such as a large microscopy image, has no single file to download, so it is listed as unavailable with why, and selecting one stops `add`. dandi's search is not part of `fetchloom search`.

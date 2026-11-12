# dropbox

a dropbox shared link downloads a file, or a folder as one zip.

```sh
fetchloom get "https://www.dropbox.com/s/3y96pnutxum3p4v/0569.pdf?dl=1"
```

## references

| shared-link path | kind |
|---|---|
| `/s/` | shared file |
| `/sh/` | shared folder |
| `/scl/fi/` | shared file |
| `/scl/fo/` | shared folder |

quote a link with a query string, as above, so your shell passes it unchanged. fetchloom downloads with `dl=1`.

## pins and extraction

`resolved` and the file's link are the shared link with `dl=1`, and SHA-256 and size pin the downloaded bytes.

a folder arrives as one zip, named by its `Content-Disposition`. the lock pins the zip, and unpacking also records its tree hash and caches its members; `--no-extract` keeps it packed. patterns match the zip, not the paths inside it, and [archives](../concepts.md#archives) says which folders remain once it is unpacked.

## access and limits

a link works when anyone who has it may download; fetchloom cannot sign in to dropbox or ask for access. an answer that is a web page without a file name fails instead of entering the cache, so check that the link is shared.

what a shared link holds can change. a sync checks the pinned hashes, while another `get` takes the new bytes, so pin inputs with [add](../commands/add.md).

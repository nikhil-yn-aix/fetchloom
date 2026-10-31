# dropbox

dropbox shared links download a file or a folder zip through the shared cache.

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

quote links containing query strings so the shell passes them unchanged. fetchloom uses `dl=1` for the download.

## pins and extraction

`resolved` and the file download URL keep the shared URL with `dl=1`. SHA-256 and size pin the downloaded file.

a folder arrives as one zip named by `Content-Disposition`. the lock pins the zip. extraction also records its tree hash and caches its members. `--no-extract` with `add` or `get` keeps it packed.

selection patterns match the listed zip, not paths inside it. [archive placement](../concepts.md#archives) determines which folders remain after unpacking.

## access and limits

use a shared link whose access allows the download. fetchloom does not provide a browser login or access-approval workflow.

an unnamed HTML answer fails instead of entering the cache as dataset bytes. check that the link names accessible data when that happens.

shared-link contents can change. normal project sync checks the original hashes, while another `get` can accept new bytes. use [add](../commands/add.md) for reproducible project inputs.

# URLs and DOIs

HTTP and HTTPS links name one file; DOIs route a persistent handle to a known dataset source.

```sh
fetchloom add https://raw.githubusercontent.com/mwaskom/seaborn-data/master/iris.csv
fetchloom add doi:10.5281/zenodo.3608990
fetchloom get https://doi.org/10.5281/zenodo.3608990
```

## accepted references

| spelling | behavior |
|---|---|
| HTTP or HTTPS URL | fetch one file, including direct release-asset URLs |
| `doi:10.5281/zenodo.3608990` | resolve the DOI handle and route its URL |
| `10.5281/zenodo.3608990` | accept a bare DOI |
| `https://doi.org/10.5281/zenodo.3608990` | accept the DOI link |

`github.com` `/blob/` file links and `gitlab.com` `/-/blob/` file links are converted to their raw download URLs. `add` stores the raw reference.

for direct downloads, a safe `Content-Disposition` filename, including one received after redirects, can replace the name guessed from the URL. selection patterns match the source path before that replacement. recognized archive names [unpack by default](../concepts.md#archives).

a response with HTML content type warns when its filename looks like data rather than HTML. check the saved file and use a download or raw link if the server returned a page.

## pins

a plain URL records the parsed URL as both `resolved` and the file's download URL. SHA-256 and size pin its bytes, even though the URL can later change. DOI pins follow the source to which the handle routes.

`get` can reuse a URL record with `ETag` or `Last-Modified`. a 304 response avoids a file-body download; without validators, bytes may download again. `lock --upgrade` revalidates unchecksummed URLs. a normal locked sync requires the original bytes.

## credentials

project commands reject login details in plain URLs and recognized secret query keys. keep credentials out of `data.toml` and `data.lock`.

`get` accepts secret query values for one-off downloads. its JSON file URLs and errors mask recognized keys, including malformed-link errors. recognized names include `token`, `signature` and `sig`; arbitrary secrets cannot be identified reliably.

local URL records keep raw links. do not share those records or commit them. there is no general browser login or access-approval workflow.

## limits

DOIs must route to a known source. a DOI alone does not provide a download permission or a permanent copy of the bytes.

follow [offline preparation](../concepts.md#offline) for direct-URL reuse and locked projects.

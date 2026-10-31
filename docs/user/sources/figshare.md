# figshare

figshare references pin an article version and the bytes of its selected files.

```sh
fetchloom add figshare:34744689@1
fetchloom get figshare:34744689@1
```

## references

| spelling | behavior |
|---|---|
| `figshare:34744689@1` | request article version 1 |
| a figshare article link | resolve the article and pin its version |
| an institutional figshare article link | use the article resolver for that host |

use `--select` or `--exclude` with quoted [patterns](../reference/data-toml.md#patterns) to choose listed files. filters do not bypass access checks or listing-size limits.

## pins

`resolved` records the article id and version in `figshare:` form. each file keeps the API's download URL. downloads check stated MD5 and record SHA-256 and size.

an explicitly versioned reference keeps naming that version under `lock --upgrade`. edit the reference if you intend to use another version, then lock and sync.

## credentials and limits

`FIGSHARE_TOKEN` supplies the source credential. tokens are not written to project files, logs or JSON.

embargoed and download-disabled articles are refused. obtain an accessible publication from the owner instead of trying to filter around the restriction.

follow [offline preparation](../concepts.md#offline) before disconnecting.

# figshare

a figshare reference pins an article's version and the bytes of its selected files.

```sh
fetchloom add figshare:34744689@1
fetchloom get figshare:34744689@1
```

## references

| spelling | behavior |
|---|---|
| `figshare:34744689@1` | list version 1 of the article |
| `figshare:34744689` | list the article's newest version |
| an article's link, on figshare.com or an institution's figshare site | read the article's id, and its version when the link has one |

an article link holds its title, id and version at fixed places in its path, so a title made of digits never passes for an id. links of the newer type/title/id form and the older title/id form both work. other figshare pages are refused, on institutions' sites too. a file's download link, such as one under `/ndownloader/`, stays an ordinary [URL](urls-and-dois.md).

a file figshare lists as a link to another site has no bytes on figshare. it is listed as one fetchloom cannot download, so `--exclude` its path to fetch the rest.

## pins

`resolved` is `figshare:<id>@<version>`, and each file keeps the download link figshare's API gives. downloads check the MD5 figshare states, and the lock records each file's SHA-256 and size.

a reference that names a version keeps it under `lock --upgrade`; to move to another, edit the reference, then lock and sync.

## credentials and limits

public articles need no account; [signing in](../signing-in.md) says how to give fetchloom a figshare key. an embargoed article, or one whose owner turned downloads off, is refused with `auth.required`, exit 40, and no filter gets around that; ask the owner for a copy you may download.

follow [offline preparation](../concepts.md#offline) before disconnecting.

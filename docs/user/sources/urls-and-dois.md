# URLs and DOIs

an HTTP or HTTPS link names one file. a DOI leads, through doi.org, to a dataset at one of the [sources](README.md) fetchloom knows.

```sh
fetchloom add https://raw.githubusercontent.com/mwaskom/seaborn-data/master/iris.csv
fetchloom add doi:10.5281/zenodo.3608990
fetchloom get https://doi.org/10.5281/zenodo.3608990
```

## accepted references

| spelling | behavior |
|---|---|
| HTTP or HTTPS URL | fetch one file, a release file's own link included |
| `doi:10.5281/zenodo.3608990` | ask doi.org where it points, and list the dataset there |
| `10.5281/zenodo.3608990` | accept a bare DOI |
| `https://doi.org/10.5281/zenodo.3608990` | accept the DOI link |
| `doi:10.34740/kaggle/dsv/2736749` | follow kaggle's version link to that dataset version |

`github.com` `/blob/` file links and `gitlab.com` `/-/blob/` file links are converted to their raw download URLs. `add` stores the raw reference. a link to a release's page lists its files, as [github and gitlab releases](releases.md) explains.

a raw link to a file git-lfs keeps answers with git-lfs's pointer, a few lines git keeps in the file's place, rather than the file. a download under 1 KiB that starts as a pointer does is refused with `source.invalid`, exit 10, before anything is pinned or placed, and its help names the link that serves the file itself: `https://media.githubusercontent.com/media/...` for a github raw link, or gitlab's api with `lfs=true` for a gitlab one.

a `Content-Disposition` file name the server sends, also after redirects, replaces the name guessed from the link; selection patterns match the name from the link. archives [unpack](../concepts.md#archives) as from any source.

a link whose name has no extension, such as a folder link ending in `/`, is refused with `source.invalid`, exit 10, when it answers with a web page and nothing says the bytes are a file: such an answer is a folder listing or a site's page, so nothing enters the cache or data.lock. open the page in a browser and give fetchloom the link of each file instead, or publish the folder packed, as `tar czf many.tar.gz many`, and add the archive's link, which unpacks into the dataset's folder. a web page served under a name that looks like data gets a warning; check the file and use a download or raw link. a server that answers with something HTTP does not allow, as a broken server does, a port that serves something else, or an `http://` link to a port that wants `https://`, fails with `<host> sent a reply that is not valid HTTP`, and no other try is made, since none would get another answer.

## names

a link's file is named after the last part of its path, decoded. a character windows cannot hold in a name, such as `?` or `:`, becomes `_`, and a `\` counts as a folder, so only what follows it stays. the dataset takes that name without its last extension, in lowercase letters, digits, `-` and `_`. from real runs of `fetchloom add` with links to a server on this machine:

| link ends in | dataset | file |
|---|---|---|
| `corpus-a.jsonl.gz` | `corpus-a-jsonl` | `data/corpus-a-jsonl/corpus-a.jsonl.gz` |
| `q%3Fmark.txt` | `q_mark` | `data/q_mark/q_mark.txt` |
| `back%5Cslash.txt` | `slash` | `data/slash/slash.txt` |
| `Data%20Set.CSV` | `data-set` | `data/data-set/Data Set.CSV` |

`--as` names the dataset; with `add -i`, rename the table in data.toml afterwards, and the next sync moves its pin to the new name. a link to a folder is refused as [above](#accepted-references), while its archive makes one dataset. stderr from real runs against a server on this machine that `allowed_origins` names:

```sh
$ fetchloom add http://127.0.0.1:19353/many/
error: http://127.0.0.1:19353/many/ answered with a web page, such as a folder listing, not a file
  help: open it in a browser, copy the link of each file you want, and give fetchloom those links
$ fetchloom add http://127.0.0.1:19353/many.tar.gz
 Downloaded many.tar.gz · 215 B in 8 ms
   Unpacked many/many.tar.gz · 3 files · 9 B in 94 ms
      Added many, its files are in data/many
  hint: scripts can find it with `fetchloom path many`
```

## dry runs

`add --dry-run` with a link asks its server nothing, so it says `size unknown`, and what only an answer tells waits for the real add: a web page in place of a file, a host at a private address that `allowed_origins` does not name, where a redirect leads, and what an archive holds. so a dry run can end `Ok dry run` for a link the add then refuses, as the folder link above is refused. from a real run against a server on this machine, where `--json` counted 0 requests:

```sh
$ fetchloom add --dry-run http://127.0.0.1:19354/many/
      Would pin many, new in data.lock: http://127.0.0.1:19354/many/ · 1 file · size unknown
      Would fetch many/many · size unknown
      Would place data/many · 1 file
      Would add many to data.toml, its files in data/many
         Ok dry run, nothing was changed
```

## pins

a plain link is recorded as both `resolved` and the file's download link, and its SHA-256 and size pin its bytes, even though what the link serves can change. a DOI pins what the source it routes to records. `lock --upgrade` checks the link again, while a sync needs the pinned bytes.

## credentials

project commands refuse a link holding a login or a known secret, such as `token=`, since data.toml and data.lock are shared. `get` takes such a link for a one-off download. what it prints masks the login and each query or fragment value whose name is not known to be harmless, such as `dl`, `version` or `rlkey`, so a secret under a name fetchloom does not know stays out of its output; a project command cannot tell that secret apart, so check a link's query before you add it. the cache's [records of links](../reference/cache-folder.md#url-and-partial-records) keep a fingerprint of each link, not its text. a server's own login goes in `.netrc`, which `fetchloom auth login <host>` writes.

## limits

a DOI must lead to a dataset at a known source. one that leads to another site, or to a page of a known source that is no dataset, such as a kaggle notebook, is refused with `reference.unresolved`, exit 10. a DOI gives no right to download, nor a lasting copy of the bytes.

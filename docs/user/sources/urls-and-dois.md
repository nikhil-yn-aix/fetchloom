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
| `doi:10.34740/kaggle/dsv/2736749` | follow kaggle's version link to that dataset version |

`github.com` `/blob/` file links and `gitlab.com` `/-/blob/` file links are converted to their raw download URLs. `add` stores the raw reference.

for direct downloads, a safe `Content-Disposition` filename, including one received after redirects, can replace the name guessed from the URL. selection patterns match the source path before that replacement. recognized archive names [unpack by default](../concepts.md#archives).

a link whose name has no extension, such as a folder link ending in `/`, is refused with `source.invalid`, exit 10, when it answers with an HTML page and the server names no file and no pinned hash proves the bytes: such an answer is a folder listing or a site's page, so nothing enters the cache or data.lock. open the page in a browser and give fetchloom the link of each file instead. to share a folder of files as one dataset, publish it packed, as `tar czf many.tar.gz many`, and add the archive's link: fetchloom unpacks it into the dataset's folder. a response with HTML content type warns when its filename looks like data rather than HTML. check the saved file and use a download or raw link if the server returned a page.

## names

a link's file is named after the last part of its path, decoded. a character windows cannot hold in a name, such as `?` or `:`, becomes `_`, and a `\` counts as a folder, so only what follows it stays. the dataset takes that name without its last extension, in lowercase letters, digits, `-` and `_`. from real runs of `fetchloom add` with links to a server on this machine:

| link ends in | dataset | file |
|---|---|---|
| `corpus-a.jsonl.gz` | `corpus-a-jsonl` | `data/corpus-a-jsonl/corpus-a.jsonl.gz` |
| `q%3Fmark.txt` | `q_mark` | `data/q_mark/q_mark.txt` |
| `back%5Cslash.txt` | `slash` | `data/slash/slash.txt` |
| `Data%20Set.CSV` | `data-set` | `data/data-set/Data Set.CSV` |

`--as` names the dataset. `add -i` cannot, so rename a table in data.toml afterwards: the next sync moves its pin to the new name. a link to a folder answers with a web page, which is refused as [above](#accepted-references); its archive is one dataset. stderr from real runs against a server on this machine that `allowed_origins` names:

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

`add --dry-run` with a link asks its server nothing: it checks the link's spelling and what data.toml would hold, and says `size unknown`. what only an answer tells waits for the real add: a web page in place of a file, a host at a private address that `allowed_origins` does not name, where a redirect leads, and what an archive holds. so a dry run can end `Ok dry run` for a link the add then refuses. from real runs against a server on this machine, where `--json` counted 0 requests for the dry run:

```sh
$ fetchloom add --dry-run http://127.0.0.1:19354/many/
  Would pin many, new in data.lock: http://127.0.0.1:19354/many/ · 1 file · size unknown
Would fetch many/many · size unknown
Would place data/many · 1 file
  Would add many to data.toml, its files in data/many
         Ok dry run, nothing was changed
$ fetchloom add http://127.0.0.1:19354/many/
error: http://127.0.0.1:19354/many/ answered with a web page, such as a folder listing, not a file
  help: open it in a browser, copy the link of each file you want, and give fetchloom those links
```

## pins

a plain URL records the parsed URL as both `resolved` and the file's download URL. SHA-256 and size pin its bytes, even though the URL can later change. DOI pins follow the source to which the handle routes.

`get` can reuse a URL record with `ETag` or `Last-Modified`. a 304 response avoids a file-body download; without validators, bytes may download again. `lock --upgrade` revalidates unchecksummed URLs. a normal locked sync requires the original bytes.

## credentials

project commands reject login details in plain URLs and recognized secret query keys. keep credentials out of `data.toml` and `data.lock`.

`get` accepts secret query values for one-off downloads. its JSON file URLs and errors mask logins and recognized keys, including malformed-link errors. recognized names include `token`, `signature` and `sig`; arbitrary secrets cannot be identified reliably.

[local URL and partial records](../reference/cache-folder.md#url-and-partial-records) store a fingerprint of the URL, not its raw text. cache payloads and other metadata can still contain private data. there is no general browser login or access-approval workflow.

## limits

DOIs must route to a known source. a DOI that leads to a page of a known source that is not a dataset, such as a kaggle notebook, is refused as unresolved. a DOI alone does not provide a download permission or a permanent copy of the bytes.

follow [offline preparation](../concepts.md#offline) for direct-URL reuse and locked projects.

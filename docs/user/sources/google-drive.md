# google drive

google drive references fetch shared files or walk shared folders while pinning fixed file bytes. a file or folder shared with your google account or a service account, rather than with anyone who has the link, is read through drive's api once you list it: see [private files and folders](../guides/private-cloud-storage.md#private-google-drive-files).

```sh
fetchloom get drive:15bpMH3clXwBJf9cdGpXIOgwKwHrg4T--
fetchloom add drive:15bpMH3clXwBJf9cdGpXIOgwKwHrg4T--
```

## references

| spelling | behavior |
|---|---|
| `drive:` followed by a file id | fetch the shared file |
| `drive:folders/` followed by a folder id | walk the shared folder |
| a shared file or folder link | preserve its id and resource key when supplied |
| a shared `open?id=` link | resolve it to a file or folder reference |

## pins

resolved references retain the id and any `resourcekey`. file URLs use `https://drive.google.com/uc` with the id and resource key. confirmation-form fields are not written to the lock.

transfers start at `https://drive.usercontent.google.com/download` with `id`, `export=download` and `confirm=t`, plus the resource key when needed. a confirmation page can supply the next request without changing the lock URL.

folder listings give no file sizes. downloads supply sizes for the lock. [approval](../concepts/questions-and-interruptions.md#approval) checks a server's stated length, or received bytes when no length is available.

| source | `resolved` | file `url` |
|---|---|---|
| drive file or folder | `drive:<id>` or `drive:folders/<id>`, with `?resourcekey=<key>` when supplied; `open?id=` resolves to one of these | `https://drive.google.com/uc?id=<id>`, with `resourcekey` when supplied |

## access and limits

use links that grant access to the shared files. fetchloom follows download confirmations and reports quota, access and missing-file errors. a resource key is retained when the link supplies one. a file or folder shared only with some people fails with `auth.required`, unless `drive_folders` lists it, as [private files and folders](../guides/private-cloud-storage.md#private-google-drive-files) explains.

google docs exports can change between downloads. select and exclude patterns also match these entries. a doc that remains selected stops the command; the error names the selected docs and explains how to exclude them.

exclude those paths, or ask the owner to upload fixed files. `add` saves exclusions in `data.toml`; an existing project needs that setting edited there. every select pattern must match before exclusions, and some fixed files must remain. empty folders and empty selections are refused.

shared links can change their contents. file hashes pin bytes after resolution, and a later `get` can accept changed bytes. use the project lock when the input must stay fixed.

## private files and folders

see [private Google Drive files](../guides/private-cloud-storage.md#private-google-drive-files).

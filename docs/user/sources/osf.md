# OSF

an OSF reference walks a project's osfstorage folders and pins each file at its version.

```sh
fetchloom add osf:3eunr
fetchloom get osf:3eunr
```

## references

| spelling | behavior |
|---|---|
| `osf:3eunr` | list a project, or any OSF node, or one file, by its id |
| a project's link | walk its osfstorage folders |
| a file's link by its id, as `https://osf.io/<id>/` | list that file |
| an OSF download link with `?version=N` | download that version as a plain link, pinned by SHA-256; any other OSF link with `?version=` is refused, since its listing describes only the latest version |
| an OSF link to one file or folder in a project's storage, such as `https://osf.io/ezcuj/files/osfstorage/553e69248c5e4a219919ea54` | refused, since its last part is a storage id rather than a path; paste a file's download link, or add the project with `--select "<folder>/**"` |

## pins

`resolved` keeps the node's or file's id. each file's link names its version, and SHA-256 and size pin its bytes. when OSF leaves out a size, the SHA-256 it states still pins the bytes; an MD5 remembered from an earlier download cannot stand in for it.

a node is no fixed snapshot: resolving it again, also after a filter change, can pick newer file versions, so read the lock's diff before you commit it.

a storage link OSF says has expired is asked for again. a pinned download that answers 404 or 410 can use [recovery](../concepts/offline-and-recovery.md#recovery), checked against the pinned size and hashes.

| source | `resolved` | file `url` |
|---|---|---|
| OSF | `osf:<node or file id>` | the download link with a file version |

## credentials and limits

public projects need no account. a private one needs a key whose account may read it, from `fetchloom auth login osf` or `OSF_TOKEN`.

the walk reads osfstorage only, and refuses an unsafe name before it enters a folder, with control characters escaped in the error and help to ask the dataset's owner to rename it. a file kept in storage another service runs, linked to the project, has no OSF version to pin, so it is refused with help to ask the project's owner to move it into OSF storage.

it does not enter a project's components, which keep their files in storage of their own. a project with components gets a warning giving their number, also in `--json`'s `warnings`; add each component you need as its own dataset, as `osf:<id>`.

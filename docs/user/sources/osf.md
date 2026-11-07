# OSF

OSF references walk osfstorage folders and pin versioned file downloads.

```sh
fetchloom add osf:3eunr
fetchloom get osf:3eunr
```

## references

| spelling | behavior |
|---|---|
| `osf:3eunr` | resolve an OSF node or file id |
| an OSF project link | walk its osfstorage folders |
| an OSF file GUID link | resolve that file |
| an OSF download link with `?version=N` | download that version as a plain link, pinned by SHA-256; any other OSF link with `?version=` is refused, since its listing describes only the latest version |
| an OSF link to one file or folder in a project's storage, such as `https://osf.io/ezcuj/files/osfstorage/553e69248c5e4a219919ea54` | refused, since its last part is a storage id rather than a path; paste a file's download link, or add the project with `--select "<folder>/**"` |

selection patterns match the listed paths before downloading. [pattern rules](../reference/data-toml.md#patterns) apply at every folder depth.

## pins

`resolved` keeps the OSF node or file id. each file URL includes its version, and SHA-256 and size pin its bytes. when OSF omits a size, its stated SHA-256 still pins the bytes; a remembered MD5 mapping cannot replace that hash.

an OSF node is not an immutable snapshot. resolving the node again, including after a filter change, can select newer file versions. review the lock diff before committing it.

explicitly expired storage redirects can be refreshed. locked 404 or 410 downloads can also use [source URL recovery](../concepts.md#recovery), while checking against the original size and hashes.

## credentials and limits

`fetchloom auth login osf` or `OSF_TOKEN` supplies the source credential. it is not stored in the manifest, lock, logs or JSON output.

the folder walk covers osfstorage. source access rules and listing-size limits still apply. fetchloom does not grant account access or guarantee permanent hosting of the locked bytes.

the walk does not enter a project's components, which keep their files in storage of their own. when a project has components, fetchloom warns once the project resolves, giving their number, and `--json` lists the warning in `warnings`; add each component you need as its own dataset, by its id, as in `osf:<id>`.

follow [offline preparation](../concepts.md#offline) before disconnecting.

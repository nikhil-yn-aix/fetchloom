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

selection patterns match the listed paths before downloading. [pattern rules](../reference/data-toml.md#patterns) apply at every folder depth.

## pins

`resolved` keeps the OSF node or file id. each file URL includes its version, and SHA-256 and size pin its bytes.

an OSF node is not an immutable snapshot. resolving the node again, including after a filter change, can select newer file versions. review the lock diff before committing it.

explicitly expired storage redirects can be refreshed. locked 404 or 410 downloads can also use [source URL recovery](../concepts.md#recovery), while checking against the original size and hashes.

## credentials and limits

`OSF_TOKEN` supplies the source credential. it is not stored in the manifest, lock, logs or JSON output.

the folder walk covers osfstorage. source access rules and listing-size limits still apply. fetchloom does not grant account access or guarantee permanent hosting of the locked bytes.

prepare the project online before [offline sync](../concepts.md#offline); repository listings are not cached for offline resolution.

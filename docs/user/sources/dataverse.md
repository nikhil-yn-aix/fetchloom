# dataverse

dataverse references pin published dataset or file versions from the named host.

```sh
fetchloom add dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP
fetchloom get dataverse:dataverse.harvard.edu/file:6867336
```

## references

| spelling | behavior |
|---|---|
| `dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP` | resolve a dataset's persistent id |
| `dataverse:dataverse.harvard.edu/file:6867336` | resolve a numeric file id |
| a dataset or file page | read its dataset or file reference |
| a `doi:` or `hdl:` persistent id on a dataverse host | resolve the published dataset or file version |

## pins

resolved dataset references contain the host, persistent id and `@major.minor` version. resolved file references contain the host, numeric file id and version. a file PID is converted to its numeric id.

file URLs use `/api/access/datafile/` followed by the numeric id and `?format=original`. fetchloom checks stated MD5 or SHA-256. other checksum algorithms are ignored; SHA-256 is learned from the download instead.

[lock records](../reference/data-lock.md#what-each-source-records) defines the stored forms. changing filters keeps the resolved reference unless the manifest reference changes or an upgrade is requested.

## credentials and limits

use public dataverse links. `DATAVERSE_TOKEN` is not read. access refusal requires a public copy from the dataset's owner.

a dataset is refused if any listed file is restricted. a restricted file page is also refused. selection cannot bypass these checks or the listing-size limit.

follow [offline preparation](../concepts.md#offline) before disconnecting.

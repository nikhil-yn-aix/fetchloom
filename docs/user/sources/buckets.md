# public cloud buckets

fetchloom lists and downloads public S3 and google cloud storage buckets anonymously over HTTPS, without an AWS or google account, SDK or command line tool.

```sh
fetchloom add s3://openneuro.org/ds000001/
fetchloom get gs://cloud-samples-data/bigquery/us-states/
```

## references

| written form | stored as |
|---|---|
| `s3://<bucket>/<prefix>/` | the same |
| `gs://<bucket>/<prefix>/` | the same |
| `https://<bucket>.s3.amazonaws.com/<prefix>/`, also with `s3.<region>` or `s3-<region>` in the host | `s3://<bucket>/<prefix>/` |
| `https://s3.amazonaws.com/<bucket>/<prefix>/` or `https://s3.<region>.amazonaws.com/<bucket>/<prefix>/` | `s3://<bucket>/<prefix>/` |
| `https://storage.googleapis.com/<bucket>/<prefix>/` or `https://<bucket>.storage.googleapis.com/<prefix>/` | `gs://<bucket>/<prefix>/` |

a pasted HTTPS link names a prefix only when its path ends in `/`, or names the bucket alone. a link to one object stays a plain [URL](urls-and-dois.md) and downloads that object. a link with a query string, such as a presigned link, also stays a plain URL, as do website, dualstack and other S3 endpoint hosts.

an `s3://` or `gs://` reference without a final `/` names the object with exactly that key, or else the folder of that name. keys are case sensitive. bucket names follow each service's rules: lowercase letters, digits, `-` and `.`, from 3 to 63 characters on S3, and also `_` and up to 222 characters on cloud storage.

`data.toml` keeps the `s3://` or `gs://` form, even for a pasted link. without `--as`, a dataset is named after the last segment of its prefix, so `s3://openneuro.org/ds000001/` becomes `ds000001`. a selection that keeps one file is named after that file, as with other sources.

## listing

S3 buckets are listed with ListObjectsV2, up to 1,000 keys a page, following continuation tokens. a bucket whose name contains a dot is addressed by path on `s3.amazonaws.com`, because the service's certificate does not cover dotted host names; other buckets use their own host. an object whose whole key is `soap` downloads by path on the service's host, as [S3 requires](https://docs.aws.amazon.com/AmazonS3/latest/userguide/object-keys.html). when S3 answers that a bucket lives in another region, through its `x-amz-bucket-region` header or the region named in its error, fetchloom asks that region's endpoint once.

cloud storage buckets are listed through the JSON API at `storage.googleapis.com/storage/v1/b/<bucket>/o`, following page tokens.

zero-byte objects whose keys end in `/` mark folders and are skipped. a nonempty object ending in `/` cannot name a file on disk, so the listing fails with `source.invalid` before selection or downloads. ask the owner to rename it, or choose a prefix that leaves it out. the 100,000-object listing limit applies before `--select` and `--exclude`, so name a narrower prefix inside a large bucket. a prefix that holds no objects fails with `reference.unresolved`.

## pins

| store | `resolved` | file `url` | `md5` |
|---|---|---|---|
| S3 | the `s3://` reference | the object's HTTPS URL on the endpoint that listed it | the ETag, when it holds no `-` |
| cloud storage | the `gs://` reference | `https://storage.googleapis.com/<bucket>/<key>?generation=<generation>` | the listed `md5Hash` |

an ETag with a `-` belongs to an object uploaded in parts and is not an MD5 of its bytes, so such files are pinned by SHA-256 and size alone. a stated MD5 is checked against the downloaded bytes.

a cloud storage URL keeps the object's generation. a listing with a missing, empty or nonnumeric generation fails with `source.invalid`; fetchloom does not replace it with an unversioned URL. when someone replaces the object, that URL still names the old bytes while the bucket keeps old generations, and fails to download when it does not. ListObjectsV2 states no object versions, so an S3 URL serves whatever the key holds now; sync refuses bytes that differ from the locked SHA-256. `lock --upgrade` lists the prefix again.

## access and limits

only buckets that allow anonymous listing work. fetchloom reads no AWS or google credentials, such as `AWS_ACCESS_KEY_ID`, `~/.aws` or `GOOGLE_APPLICATION_CREDENTIALS`, and sends no credential or signature to these hosts.

a refused listing (401 or 403, such as S3's `AccessDenied`) fails with `auth.required` and exit code 40, saying that fetchloom reads public buckets only. use the provider's own tools for private data. a bucket that allows reading objects but not listing them still works one object at a time through plain object links. requester-pays buckets need credentials and are refused the same way. a missing bucket fails with `reference.unresolved`.

S3-compatible services on other hosts, AWS regions outside `amazonaws.com` and azure blob storage are not recognized as buckets; their object links work as plain URLs.

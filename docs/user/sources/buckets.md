# cloud buckets

fetchloom lists and downloads S3 and google cloud storage buckets over HTTPS, without an AWS or google SDK or command line tool. a public bucket needs no account; a private S3 bucket you list in the `s3_buckets` setting is read with your AWS keys.

```sh
fetchloom add s3://openneuro.org/ds000001/
fetchloom get gs://cloud-samples-data/bigquery/us-states/
FETCHLOOM_S3_BUCKETS=s3://lab-raw fetchloom add s3://lab-raw/2026/
fetchloom add s3:https://minio.lab.example:9000/scans/raw/
```

## references

| written form | stored as |
|---|---|
| `s3://<bucket>/<prefix>/` | the same |
| `gs://<bucket>/<prefix>/` | the same |
| `https://<bucket>.s3.amazonaws.com/<prefix>/`, also with `s3.<region>` or `s3-<region>` in the host | `s3://<bucket>/<prefix>/` |
| `https://s3.amazonaws.com/<bucket>/<prefix>/` or `https://s3.<region>.amazonaws.com/<bucket>/<prefix>/` | `s3://<bucket>/<prefix>/` |
| `https://storage.googleapis.com/<bucket>/<prefix>/` or `https://<bucket>.storage.googleapis.com/<prefix>/` | `gs://<bucket>/<prefix>/` |
| `s3:https://<host>[:<port>]/<bucket>/<prefix>/`, or `s3:http://…` | the same, with the host in lowercase and a usual port left out |

a bucket at another service that speaks S3's API, such as minio, cloudflare r2, backblaze b2, wasabi or ceph, is written `s3:` followed by the service's own link, its host and port, then the bucket and prefix. it may hold no login, query or fragment. its buckets are addressed by path on that host. a host written on the command line may be on this machine or your network for that run; one only data.toml or data.lock names reaches such a server when [`allowed_origins`](../reference/environment.md) lists its origin, as for any link.

a pasted HTTPS link names a prefix only when its path ends in `/`, or names the bucket alone. a link to one object stays a plain [URL](urls-and-dois.md) and downloads that object. a link with a query string, such as a presigned link, also stays a plain URL, as do website, dualstack and other S3 endpoint hosts.

an `s3://` or `gs://` reference without a final `/` names the object with exactly that key, or else the folder of that name. keys are case sensitive. bucket names follow each service's rules: lowercase letters, digits, `-` and `.`, from 3 to 63 characters on S3, and also `_` and up to 222 characters on cloud storage.

`data.toml` keeps the `s3://`, `s3:https://` or `gs://` form, even for a pasted link. without `--as`, `add` names a one-object listing after that file. otherwise it uses the last segment of the prefix, so `s3://openneuro.org/ds000001/` becomes `ds000001`. it chooses from the unfiltered listing, so `--select` keeping one of several objects does not change that prefix-derived name; use `--as` to choose it. `get` chooses its folder from the selected files and uses the filename stem when one remains.

## listing

S3 buckets are listed with ListObjectsV2, up to 1,000 keys a page, following continuation tokens. a bucket whose name contains a dot is addressed by path on `s3.amazonaws.com`, because the service's certificate does not cover dotted host names; other buckets use their own host. an object whose whole key is `soap` downloads by path on the service's host, as [S3 requires](https://docs.aws.amazon.com/AmazonS3/latest/userguide/object-keys.html). when S3 answers that a bucket lives in another region, through its `x-amz-bucket-region` header or the region named in its error, fetchloom asks that region's endpoint once.

a bucket at another service is listed the same way, by path on its host. no region move applies there.

cloud storage buckets are listed through the JSON API at `storage.googleapis.com/storage/v1/b/<bucket>/o`, following page tokens.

zero-byte objects whose keys end in `/` mark folders and are skipped. a nonempty object ending in `/` cannot name a file on disk, so the listing fails with `source.invalid` before selection or downloads. ask the owner to rename it, or choose a prefix that leaves it out. the 100,000-object listing limit applies before `--select` and `--exclude`, so name a narrower prefix inside a large bucket. a prefix that holds no objects fails with `reference.unresolved`.

## pins

| store | `resolved` | file `url` | `md5` |
|---|---|---|---|
| S3 | the `s3://` reference | the object's HTTPS URL on the endpoint that listed it, on its region's endpoint after a signed listing, with `?versionId=<id>` when the bucket keeps versions | the ETag, when it holds no `-` and the listing went without a key |
| another S3 service | the `s3:https://` reference | the object's link on that service, `https://<host>[:<port>]/<bucket>/<key>`, with `?versionId=<id>` when the bucket keeps versions | none |
| cloud storage | the `gs://` reference | `https://storage.googleapis.com/<bucket>/<key>?generation=<generation>` | the listed `md5Hash` |

an ETag with a `-` belongs to an object uploaded in parts and is not an MD5 of its bytes, so such files are pinned by SHA-256 and size alone. a bucket that needs a key may encrypt objects with keys of its own, whose ETags are no MD5 either, and other services promise nothing about theirs, so those files are pinned the same way. a stated MD5 is checked against the downloaded bytes.

an S3 bucket that keeps versions names the version of each object it sends, and the lock URL pins it, so `sync` gets the same bytes after someone replaces the object. the version is kept when the bytes come from the cache instead, through what an earlier download of that link recorded. a pinned version that is gone, or that the key may not read, is fetched from the object there now, and the locked SHA-256 decides: the same bytes are used, other bytes fail with `integrity.mismatch`, exit 30, saying the version could not be read. a bucket that refuses old versions to anyone without a key, as many public ones do, refuses them all, so once one file of a run came that way, its later files ask for the object there now first, and for their pinned version only when those bytes differ. a bucket that keeps no versions serves whatever the key holds now; sync refuses bytes that differ from the locked SHA-256. `lock --upgrade` lists the prefix again.

a cloud storage URL keeps the object's generation. a listing with a missing, empty or nonnumeric generation fails with `source.invalid`; fetchloom does not replace it with an unversioned URL. object names in generation errors have control characters escaped. when someone replaces the object, that URL still names the old bytes while the bucket keeps old generations, and fails to download when it does not.

## private S3 buckets

the `s3_buckets` setting lists the buckets fetchloom may sign for with your AWS keys, in the [settings file](../reference/environment.md#settings-file) or `FETCHLOOM_S3_BUCKETS`, separated by commas. an AWS bucket is `s3://<bucket>`, or the bare name; one at another service is `s3:https://<host>[:<port>]/<bucket>`. a bucket nobody listed never gets a key, so a cloned project cannot make fetchloom present your key to a bucket of its choosing.

```toml
s3_buckets = ["s3://lab-raw", "s3:https://minio.lab.example:9000/scans"]
```

each run asks a bucket without a key first, so a public bucket never sees your key. when S3 refuses a listed bucket, with 401 or 403, fetchloom asks again once, signed with AWS signature version 4, and signs that bucket's later requests from the start. this costs one extra request for each such bucket in each run, or one for each download that was already on its way when the first refusal came. a link that carries its own signature, as a presigned link does, is never signed again, and a signature never follows a redirect.

keys come from where the AWS tools keep them, in this order:

1. `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, with `AWS_SESSION_TOKEN` for temporary keys.
2. the profile `AWS_PROFILE` names, else `default`: its `aws_access_key_id` and `aws_secret_access_key` in `~/.aws/credentials`, else in `~/.aws/config`. `AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE` name other files.

a profile that gets its keys by taking on a role, by signing in to IAM identity center, from a web identity token or from a `credential_process` program is refused with `auth.required`, never passed over for another identity. set the variables `aws configure export-credentials --profile <name> --format env` prints instead. an `AWS_PROFILE` neither file has fails with `usage`, exit 2.

the region a request is signed for is the one its host names, as `s3.eu-west-1.amazonaws.com` does, else the one S3 named for the bucket, else `AWS_REGION`, `AWS_DEFAULT_REGION` or the profile's `region`, else `us-east-1`. a signed listing names the bucket's region in its object links, so a later run signs them for it. cloudflare r2 signs for `auto`, and backblaze and wasabi take the region in their host.

keys reach another service only where your AWS settings send them: `AWS_ENDPOINT_URL_S3` or `AWS_ENDPOINT_URL` sends the keys above to that endpoint, and so does a profile's `endpoint_url`, set in the profile or for `s3` in the `services` section it names. a bucket there takes the keys of the one profile whose endpoint names it, so `AWS_PROFILE` can stay on your AWS profile. keys sent to an endpoint are never sent to AWS. `AWS_IGNORE_CONFIGURED_ENDPOINT_URLS=true` turns endpoint settings off. a plain `http` service is signed only when your settings name its `http` link.

the secret never leaves the machine: a request carries the key id and a signature made with the secret. messages show the key id by its last four characters, such as `…WXYZ`. no key or session token reaches data.toml, data.lock, logs or `--json` output.

## access and limits

a refused listing (401 or 403, such as S3's `AccessDenied`) fails with `auth.required` and exit code 40, saying whether a key went with the request and what to do. a bucket `s3_buckets` does not list:

```text
error: s3://lab-raw/2026/ is private: S3 refused to list it without a key
  help: if you trust s3://lab-raw with your AWS keys, add it to s3_buckets in the settings file or FETCHLOOM_S3_BUCKETS
```

a listed bucket with no keys on this machine:

```text
error: s3://lab-raw/2026/ is private: S3 refused to list it without a key, and no AWS keys were found
  help: set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or AWS_PROFILE to a profile in ~/.aws/credentials
```

a signed request S3 still refuses says `S3 refused the AWS key …WXYZ from AWS_ACCESS_KEY_ID for s3://lab-raw/2026/` and asks you to have the owner let that key list and read the bucket. a line under each error repeats what S3 said, such as `AccessDenied: Access Denied`. a download refused the same way fails with the same kind and says which link it was. a service that wants another region than the request was signed for names it, and a missing bucket fails with `reference.unresolved`.

requester-pays buckets bill the reader for each request, and fetchloom never agrees to pay: they fail with `auth.required`, with or without a key. use the AWS CLI with `--request-payer requester` for them. a bucket that allows reading objects but not listing them still works one object at a time through plain object links.

google cloud storage buckets must allow anonymous reads: `GOOGLE_APPLICATION_CREDENTIALS` is not read, and a refused listing says fetchloom reads public cloud storage buckets only. AWS keys that come from a role, IAM identity center, `credential_process` or an instance's role are not read, and azure blob storage is not a bucket source; an object link that needs no key still works as a plain URL.

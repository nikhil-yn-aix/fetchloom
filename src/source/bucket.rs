//! cloud object storage: an S3, cloud storage or azure bucket, a prefix inside one, or a bucket
//! at a service that speaks S3's API. requests go without a key until a bucket the settings list
//! refuses one, as [`crate::net::s3`] decides for S3.

mod azure;
mod gcs;
mod s3;

use std::fmt;

use percent_encoding::{percent_decode_str, utf8_percent_encode};
use url::Url;

use super::{Access, Listing, Reference, Remote, invalid, malformed};
use crate::auth::azure::{BLOB_HOST, account_of, is_account, is_container};
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::s3::{Bucket, Keyed, endpoint, s3_host};
use crate::net::sign::UNRESERVED;
use crate::net::{self, Api, Response};

/// the help for a cloud storage bucket that bills the reader, which fetchloom never agrees to.
const REQUESTER_PAYS: &str = "use `gcloud storage cp` with --billing-project for this bucket";

/// the longest key S3 and cloud storage accept, in bytes.
const LONGEST_KEY: usize = 1024;

/// the page tokens a listing followed, as [`super::Pages`] guards them.
struct Pages(super::Pages<String>);

impl Pages {
    /// the pages of a listing of at most `most` objects.
    fn new(most: usize) -> Pages {
        Pages(super::Pages::after_first(most))
    }

    /// follows `token`, the listing holding `objects` so far.
    fn follow(&mut self, prefix: &Prefix, token: &str, objects: usize) -> Result<(), Error> {
        self.0
            .next(token.to_owned(), objects)
            .map_err(|reason| invalid(prefix, &reason))
    }
}

/// which object store a bucket lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Store {
    S3,
    Gcs,
    /// azure blob storage, whose containers are a bucket's part, inside a storage account.
    Azure,
}

impl Store {
    fn scheme(self) -> &'static str {
        match self {
            Store::S3 => "s3",
            Store::Gcs => "gs",
            Store::Azure => "azure",
        }
    }

    fn example(self) -> &'static str {
        match self {
            Store::S3 => "s3://openneuro.org/ds000001/",
            Store::Gcs => "gs://cloud-samples-data/bigquery/us-states/",
            Store::Azure => "azure:azureopendatastorage/mnist/",
        }
    }
}

/// a bucket and the key prefix inside it, as written. a prefix ending in `/`, or none, names a
/// folder. any other prefix names one object, or the folder of that name when no object has it.
/// an azure container is the bucket, with its account's blob host as the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prefix {
    store: Store,
    /// for a bucket at another service that speaks S3's API, or an azure container, where it is.
    endpoint: Option<url::Origin>,
    bucket: String,
    key: String,
}

/// the objects a store listed under a prefix, and whether it listed them only once credentials
/// went with the request.
struct Listed {
    objects: Vec<Object>,
    credentials: bool,
}

/// whether `response` answers a request that carried credentials, which every store gets only
/// once it refused the same request without them: an S3 signature, or anything but an
/// anonymous request to cloud storage or azure.
fn with_credentials(response: &Response) -> bool {
    matches!(response.keyed, Some(Keyed::Signed { .. }))
        || response.authorized.as_ref().is_some_and(|authorized| {
            !matches!(
                authorized,
                net::Authorized::Anonymous { .. }
                    | net::Authorized::Azure(net::Sent::Anonymous { .. })
            )
        })
}

/// an object a bucket listed, before its key becomes a dataset path.
struct Object {
    key: String,
    size: u64,
    md5: Option<Md5>,
    /// the `ETag` the listing gives, when GETs of the object send the same one.
    etag: Option<String>,
    url: Url,
}

impl Prefix {
    /// whether it names a folder, its key ending in `/`, or a whole bucket or container, rather
    /// than one object.
    pub(crate) fn names_a_folder(&self) -> bool {
        self.key.is_empty() || self.key.ends_with('/')
    }

    /// reads the part after `s3:` or `gs:`, such as `//openneuro.org/ds000001/`, or for a
    /// bucket at another service, `https://minio.lab.example:9000/scans/raw/`, or after
    /// `azure:`, an account, a container and a prefix, such as `azureopendatastorage/mnist/`.
    pub(super) fn parse(store: Store, text: &str) -> Result<Prefix, Error> {
        let text = text.trim();
        if store == Store::Azure {
            let (account, rest) = text.split_once('/').unwrap_or((text, ""));
            let (container, key) = rest.split_once('/').unwrap_or((rest, ""));
            return Prefix::azure(&account.to_ascii_lowercase(), container, key)
                .ok_or_else(|| malformed(&format!("azure:{text}"), store.example()));
        }
        if let Some(rest) = text.strip_prefix("//") {
            let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
            return Prefix::new(store, None, bucket, key)
                .ok_or_else(|| malformed(&format!("{}:{text}", store.scheme()), store.example()));
        }
        let bad = || {
            malformed(
                &format!("{}:{}", store.scheme(), crate::shown::mask_query(text)),
                "s3:https://minio.lab.example:9000/scans/raw/, for a bucket at another service that speaks S3's API",
            )
        };
        if store != Store::S3 {
            return Err(malformed(
                &format!("{}:{text}", store.scheme()),
                store.example(),
            ));
        }
        let (origin, path) = endpoint(text).ok_or_else(bad)?;
        let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
        Prefix::new(store, Some(origin), bucket, key).ok_or_else(bad)
    }

    fn new(store: Store, endpoint: Option<url::Origin>, bucket: &str, key: &str) -> Option<Prefix> {
        let usable_key =
            key.len() <= LONGEST_KEY && !key.chars().any(char::is_control) && !dotted(key);
        (is_bucket(store, bucket) && usable_key).then(|| Prefix {
            store,
            endpoint,
            bucket: bucket.to_owned(),
            key: key.to_owned(),
        })
    }

    /// the prefix `key` in the azure container `container` of the storage account `account`.
    fn azure(account: &str, container: &str, key: &str) -> Option<Prefix> {
        if !is_account(account) {
            return None;
        }
        let host = Url::parse(&format!("https://{account}{BLOB_HOST}/")).ok()?;
        Prefix::new(Store::Azure, Some(host.origin()), container, key)
    }

    /// the storage account of an azure container, as its blob host names it.
    fn account(&self) -> &str {
        match &self.endpoint {
            Some(url::Origin::Tuple(_, url::Host::Domain(host), _)) => {
                host.strip_suffix(BLOB_HOST).unwrap_or(host)
            }
            _ => "",
        }
    }

    /// whether the links of this listing may pin object versions, as S3's do.
    pub(super) fn keeps_versions(&self) -> bool {
        self.store == Store::S3
    }

    /// the bucket, as the `s3_buckets` setting names it.
    pub(crate) fn bucket(&self) -> Bucket {
        Bucket::new(self.endpoint.clone(), self.bucket.clone())
    }

    /// the origin of the service a person names by writing this prefix, when it is not AWS,
    /// google's or azure's. it may be on a private network.
    pub(super) fn written_origin(&self) -> Option<url::Origin> {
        self.endpoint.clone().filter(|_| self.store != Store::Azure)
    }

    /// a pasted HTTPS link to a bucket or a folder in one, ending in `/`, or to an azure
    /// container. a link to a single object is left alone, so it stays a plain link.
    pub(super) fn from_link(url: &Url) -> Option<Prefix> {
        if !matches!(url.scheme(), "http" | "https") || url.query().is_some() {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let path = percent_decode_str(url.path().strip_prefix('/')?)
            .decode_utf8()
            .ok()?;
        if let Some(account) = account_of(&host) {
            let (container, key) = path.split_once('/').unwrap_or((&path, ""));
            if !(key.is_empty() || key.ends_with('/')) || url.scheme() != "https" {
                return None;
            }
            return Prefix::azure(account, container, key);
        }
        let (store, bucket, key) = if host == "storage.googleapis.com" {
            let (bucket, key) = path.split_once('/').unwrap_or((&path, ""));
            if matches!(bucket, "storage" | "download" | "upload" | "batch") {
                return None;
            }
            (Store::Gcs, bucket.to_owned(), key)
        } else if let Some(bucket) = host.strip_suffix(".storage.googleapis.com") {
            (Store::Gcs, bucket.to_owned(), &*path)
        } else if let (Some(bucket), _) = s3_host(&host)? {
            (Store::S3, bucket.to_owned(), &*path)
        } else {
            let (bucket, key) = path.split_once('/').unwrap_or((&path, ""));
            (Store::S3, bucket.to_owned(), key)
        };
        if !(key.is_empty() || key.ends_with('/')) {
            return None;
        }
        Prefix::new(store, None, &bucket, key)
    }

    /// lists every object under the prefix, then names each by its key below the prefix.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let listed = match self.store {
            Store::S3 => s3::list(self, api).await?,
            Store::Gcs => gcs::list(self, api).await?,
            Store::Azure => azure::list(self, api).await?,
        };
        let files = self.files(listed.objects)?;
        if files.is_empty() {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{self} holds no objects"),
            )
            .help("check the prefix: keys are case sensitive, and a folder's prefix ends with /"));
        }
        Ok(Listing {
            within: Vec::new(),
            // a teammate needs credentials too for a bucket listed only to them
            access: if listed.credentials {
                Access::Login
            } else {
                Access::Unknown
            },
            resolved: Reference::Bucket(self.clone()),
            title: None,
            license: None,
            doi: None,
            files,
            unavailable: Vec::new(),
            warnings: Vec::new(),
        })
    }

    /// the dataset's files. zero-byte keys ending in `/` mark folders; nonempty ones cannot
    /// name files. keys that merely start with a bare prefix belong to other folders.
    fn files(&self, mut objects: Vec<Object>) -> Result<Vec<Remote>, Error> {
        let remote = |object: Object, path: &str| Remote {
            path: path.to_owned(),
            size: Some(object.size),
            sha256: None,
            md5: object.md5,
            url: object.url,
            named_by_server: false,
            etag: object.etag,
        };
        let folder = if self.key.is_empty() || self.key.ends_with('/') {
            self.key.clone()
        } else {
            if let Some(at) = objects.iter().position(|object| object.key == self.key) {
                let name = self.key.rsplit('/').next().unwrap_or(&self.key);
                return Ok(vec![remote(objects.swap_remove(at), name)]);
            }
            format!("{}/", self.key)
        };
        objects
            .into_iter()
            .filter_map(|object| {
                let path = object.key.strip_prefix(&folder)?.to_owned();
                if object.key.ends_with('/') {
                    return (object.size != 0).then(|| {
                        Err(Error::new(
                            Kind::SourceInvalid,
                            format!(
                                "{self} holds the nonempty object \"{}\", whose trailing / cannot name a file",
                                object.key.escape_debug()
                            ),
                        )
                        .help("ask the bucket's owner to rename the object without a final /, or name a prefix that leaves it out"))
                    });
                }
                (!path.is_empty()).then(|| Ok(remote(object, &path)))
            })
            .collect()
    }

    /// refuses a key with a `.` or `..` part: a link resolves those away, so it would fetch
    /// another object than the one listed.
    fn check_key(&self, key: &str) -> Result<(), Error> {
        if !dotted(key) {
            return Ok(());
        }
        Err(Error::new(
            Kind::SourceInvalid,
            format!("{self} holds \"{key}\", whose . or .. part no link can name"),
        )
        .help("name a prefix, ending in /, for a folder that leaves it out"))
    }

    /// the error for a listing that counted more than the `most` objects one dataset may hold.
    fn too_many(&self, most: usize) -> Error {
        super::too_many(
            self,
            most,
            "name a narrower prefix, ending in /, for the folder you need",
        )
    }

    /// the error for an answer other than a listing. `code`, `message` and `region` are what the
    /// service said, when it said them.
    fn refused(
        &self,
        response: &Response,
        code: Option<&str>,
        message: Option<&str>,
        region: Option<&str>,
    ) -> Error {
        let host = net::host(&response.url);
        let said = match (code, message) {
            (Some(code), Some(message)) => Some(format!("{host} said {code}: {message}")),
            (Some(said), None) | (None, Some(said)) => Some(format!("{host} said {said}")),
            (None, None) => None,
        };
        let requester_pays =
            message.is_some_and(|message| message.to_ascii_lowercase().contains("requester pays"));
        let error = match response.status {
            400 | 401 | 403 if requester_pays && self.store == Store::S3 => Error::new(
                Kind::AuthRequired,
                format!(
                    "{self} bills the reader for each request, and fetchloom never agrees to pay"
                ),
            )
            .help("use the AWS CLI with --request-payer requester for this bucket"),
            401 | 403 if self.store == Store::S3 => self.private(response.keyed.as_ref()),
            401 | 403 if self.store == Store::Azure => response
                .authorized
                .clone()
                .unwrap_or_else(|| {
                    net::Authorized::Azure(net::Sent::Anonymous {
                        container: format!("azure:{}/{}", self.account(), self.bucket),
                        listed: false,
                    })
                })
                .listing_refusal(&self.to_string(), message),
            404 | 410 if self.store == Store::Azure => Error::new(
                Kind::ReferenceUnresolved,
                format!(
                    "the container {} does not exist in the storage account {}",
                    self.bucket,
                    self.account()
                ),
            )
            .help(format!(
                "check the account and container in {self}. both are lowercase and exact"
            )),
            401 | 403 => response
                .authorized
                .clone()
                .unwrap_or(net::Authorized::Anonymous {
                    bucket: self.bucket.clone(),
                    listed: false,
                })
                .listing_refusal(&self.to_string(), message),
            400 if requester_pays => Error::new(
                Kind::AuthRequired,
                format!(
                    "{self} bills the reader for each request, and fetchloom never agrees to pay"
                ),
            )
            .help(REQUESTER_PAYS),
            400 if code == Some("AuthorizationHeaderMalformed")
                && let Some(region) = region =>
            {
                let bucket = self.bucket();
                let help = match bucket.endpoint() {
                    Some(origin) => format!(
                        "set region = {region} in the AWS profile for {}, or AWS_REGION={region}",
                        origin.ascii_serialization()
                    ),
                    None => {
                        format!("set AWS_REGION={region}, or region = {region} in your AWS profile")
                    }
                };
                Error::new(
                    Kind::AuthRequired,
                    format!(
                        "{} wants the requests for {self} signed for the region {region}",
                        bucket.service()
                    ),
                )
                .help(help)
            }
            404 | 410 => Error::new(
                Kind::ReferenceUnresolved,
                format!("the bucket {} does not exist", self.bucket),
            )
            .help(format!(
                "check the bucket name in {self}. bucket names are lowercase and exact"
            )),
            status => Error::new(
                Kind::NetworkStatus,
                format!("{host} answered {status} for {self}"),
            ),
        };
        match said {
            Some(said) => error.detail(said),
            None => error,
        }
    }

    /// the error for an S3 listing refused, as `keyed` says it went: without a key, as for a
    /// bucket `s3_buckets` does not list, or signed with one.
    fn private(&self, keyed: Option<&Keyed>) -> Error {
        let bucket = self.bucket();
        let service = bucket.service();
        match keyed {
            Some(Keyed::Signed { key, .. }) => Error::new(
                Kind::AuthRequired,
                format!("{service} refused the AWS key {key} for {self}"),
            )
            .help("ask the bucket's owner to let that key list and read it, or set AWS_PROFILE to a profile whose key can"),
            Some(Keyed::Unsigned { listed: true, .. }) => Error::new(
                Kind::AuthRequired,
                format!(
                    "{self} is private: {service} refused to list it without a key, and {}",
                    bucket.keyless()
                ),
            )
            .help(bucket.keys_help()),
            _ => Error::new(
                Kind::AuthRequired,
                format!("{self} is private: {service} refused to list it without a key"),
            )
            .help(bucket.listing_help()),
        }
    }
}

impl fmt::Display for Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.endpoint {
            Some(_) if self.store == Store::Azure => {
                write!(f, "azure:{}/{}/{}", self.account(), self.bucket, self.key)
            }
            Some(endpoint) => write!(
                f,
                "{}:{}/{}/{}",
                self.store.scheme(),
                endpoint.ascii_serialization(),
                self.bucket,
                self.key
            ),
            None => write!(f, "{}://{}/{}", self.store.scheme(), self.bucket, self.key),
        }
    }
}

/// whether `name` follows `store`'s bucket naming rules.
fn is_bucket(store: Store, name: &str) -> bool {
    match store {
        Store::S3 => crate::net::s3::is_bucket(name),
        Store::Gcs => crate::net::google::is_bucket(name),
        Store::Azure => is_container(name),
    }
}

/// a query string with every value escaped the same strict way.
fn query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", utf8_percent_encode(value, UNRESERVED)))
        .collect::<Vec<_>>()
        .join("&")
}

fn dotted(key: &str) -> bool {
    key.split('/').any(|part| matches!(part, "." | ".."))
}

/// `base` with the object key appended to its path, each segment escaped.
fn object_url(base: &Url, key: &str) -> Url {
    let mut url = base.clone();
    let path = key
        .split('/')
        .map(|segment| utf8_percent_encode(segment, UNRESERVED).to_string())
        .collect::<Vec<_>>()
        .join("/");
    url.set_path(&format!("{}{path}", base.path()));
    url
}

#[cfg(test)]
mod tests;

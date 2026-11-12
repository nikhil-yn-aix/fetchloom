//! cloud object storage: an S3 or google cloud storage bucket, or a prefix inside one, or a
//! bucket at another service that speaks S3's API. listings and downloads go without a key, and
//! only an S3 bucket the `s3_buckets` setting lists is asked again with one, as
//! [`signing`] decides.

mod gcs;
mod s3;
mod signing;
mod xml;

use std::fmt;

use percent_encoding::{percent_decode_str, utf8_percent_encode};
use url::Url;

pub(crate) use self::gcs::GcsBucket;
pub(crate) use self::signing::{Keyed, Signing};
use super::{Access, Listing, Reference, Remote, malformed};
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::sign::UNRESERVED;
use crate::net::{self, Api, Response};

/// the help for a cloud storage bucket that bills the reader, which fetchloom never agrees to.
const REQUESTER_PAYS: &str = "use `gcloud storage cp` with --billing-project for this bucket";

/// the longest key S3 and cloud storage accept, in bytes.
const LONGEST_KEY: usize = 1024;
/// the most listing pages followed. both stores send up to 1,000 objects a page, so the file
/// limit fits in a tenth of these, and a server that keeps sending pages stops here.
const MOST_PAGES: usize = 1_000;

/// the page tokens a listing followed, so a server that sends one again, or pages without end,
/// cannot keep fetchloom asking.
#[derive(Default)]
struct Pages(std::collections::HashSet<String>);

impl Pages {
    fn follow(&mut self, prefix: &Prefix, token: &str) -> Result<(), Error> {
        if self.0.len() >= MOST_PAGES {
            return Err(prefix.invalid(&format!("the listing ran past {MOST_PAGES} pages")));
        }
        if !self.0.insert(token.to_owned()) {
            return Err(prefix.invalid("a page repeated an earlier page's token"));
        }
        Ok(())
    }
}

/// which object store a bucket lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Store {
    S3,
    Gcs,
}

impl Store {
    fn scheme(self) -> &'static str {
        match self {
            Store::S3 => "s3",
            Store::Gcs => "gs",
        }
    }

    fn example(self) -> &'static str {
        match self {
            Store::S3 => "s3://openneuro.org/ds000001/",
            Store::Gcs => "gs://cloud-samples-data/bigquery/us-states/",
        }
    }
}

/// a bucket and the key prefix inside it, as written. a prefix ending in `/`, or none, names a
/// folder. any other prefix names one object, or the folder of that name when no object has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prefix {
    store: Store,
    /// for a bucket at another service that speaks S3's API, where it is.
    endpoint: Option<url::Origin>,
    bucket: String,
    key: String,
}

/// an S3 bucket as the `s3_buckets` setting names it: one on AWS, or one at another service that
/// speaks S3's API, known by its origin.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Bucket {
    endpoint: Option<url::Origin>,
    name: String,
}

impl Bucket {
    /// reads `s3://name`, or a bare name, for a bucket on AWS, and `s3:https://host:port/name`
    /// for one at another service, each with or without a final `/`.
    pub(crate) fn parse(text: &str) -> Option<Bucket> {
        let text = text.trim();
        let text = text.strip_suffix('/').unwrap_or(text);
        let (endpoint, name) = match text.split_once(':') {
            None => (None, text),
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("s3") => {
                if let Some(name) = rest.strip_prefix("//") {
                    (None, name)
                } else {
                    let (origin, name) = endpoint(rest)?;
                    (Some(origin), name)
                }
            }
            Some(_) => return None,
        };
        is_bucket(Store::S3, name).then(|| Bucket {
            endpoint,
            name: name.to_owned(),
        })
    }

    /// the bucket a link to one of its objects names: on AWS by its host, or by its first
    /// segment on the service's own host, and elsewhere by its origin and first segment, as a
    /// service that speaks S3's API serves every bucket. none when no bucket name is there.
    pub(crate) fn holding(url: &Url) -> Option<Bucket> {
        let host = url.host_str()?.to_ascii_lowercase();
        let mut segments = url.path_segments()?;
        let first = segments.next().unwrap_or_default();
        // a bucket named in the path still needs an object key after it
        let keyed = segments.next().is_some_and(|key| !key.is_empty());
        let (endpoint, name) = match s3_host(&host) {
            Some((Some(named), _)) => (None, named),
            Some((None, _)) if keyed => (None, first),
            None if keyed => (Some(url.origin()), first),
            _ => return None,
        };
        is_bucket(Store::S3, name).then(|| Bucket {
            endpoint,
            name: name.to_owned(),
        })
    }

    /// the service that holds the bucket, as messages name it: S3, or the host of another.
    fn service(&self) -> String {
        let Some(origin) = &self.endpoint else {
            return "S3".to_owned();
        };
        let origin = origin.ascii_serialization();
        origin
            .split_once("://")
            .map_or_else(|| origin.clone(), |(_, host)| host.to_owned())
    }

    /// what was missing when no keys were found to sign for the bucket.
    fn keyless(&self) -> String {
        match &self.endpoint {
            Some(origin) => format!(
                "no AWS settings send keys to {}",
                origin.ascii_serialization()
            ),
            None => "no AWS keys were found".to_owned(),
        }
    }

    /// how to give fetchloom keys for the bucket.
    fn keys_help(&self) -> String {
        match &self.endpoint {
            Some(origin) => {
                let origin = origin.ascii_serialization();
                format!(
                    "put endpoint_url = {origin} in a profile in ~/.aws/config, with its keys in ~/.aws/credentials, or set AWS_ENDPOINT_URL_S3={origin} with AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY"
                )
            }
            None => "set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or AWS_PROFILE to a profile in ~/.aws/credentials".to_owned(),
        }
    }

    /// how to let fetchloom sign for the bucket.
    fn listing_help(&self) -> String {
        format!(
            "if you trust {self} with your AWS keys, add it to s3_buckets in the settings file or FETCHLOOM_S3_BUCKETS"
        )
    }

    /// `text` as a message may repeat it, with any login in it masked.
    pub(crate) fn shown(text: &str) -> String {
        match text.split_once(':') {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("s3") => {
                format!("{scheme}:{}", net::mask_query(rest))
            }
            _ => net::mask_query(text).into_owned(),
        }
    }
}

impl Keyed {
    /// the error for a download S3 refused, as this says the request for it went.
    pub(crate) fn refusal(&self, url: &Url) -> Error {
        let shown = net::Shown(url);
        match self {
            Keyed::Signed { bucket, key, .. } => Error::new(
                Kind::AuthRequired,
                format!("{} refused the AWS key {key} for {shown}", bucket.service()),
            )
            .help(format!(
                "ask the owner of {bucket} to let that key read it, or set AWS_PROFILE to a profile whose key can"
            )),
            Keyed::Unsigned {
                bucket,
                listed: true,
            } => Error::new(
                Kind::AuthRequired,
                format!(
                    "{} refused {shown} without a key, and {}",
                    bucket.service(),
                    bucket.keyless()
                ),
            )
            .help(bucket.keys_help()),
            Keyed::Unsigned {
                bucket,
                listed: false,
            } => Error::new(
                Kind::AuthRequired,
                format!("{} refused {shown} without a key", bucket.service()),
            )
            .help(bucket.listing_help()),
        }
    }
}

impl fmt::Display for Bucket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.endpoint {
            Some(endpoint) => write!(f, "s3:{}/{}", endpoint.ascii_serialization(), self.name),
            None => write!(f, "s3://{}", self.name),
        }
    }
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
    /// reads the part after `s3:` or `gs:`, such as `//openneuro.org/ds000001/`, or for a
    /// bucket at another service, `https://minio.lab.example:9000/scans/raw/`.
    pub(super) fn parse(store: Store, text: &str) -> Result<Prefix, Error> {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix("//") {
            let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
            return Prefix::new(store, None, bucket, key)
                .ok_or_else(|| malformed(&format!("{}:{text}", store.scheme()), store.example()));
        }
        let bad = || {
            malformed(
                &format!("{}:{}", store.scheme(), net::mask_query(text)),
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

    /// whether the links of this listing may pin object versions, as S3's do.
    pub(super) fn keeps_versions(&self) -> bool {
        self.store == Store::S3
    }

    /// the bucket, as the `s3_buckets` setting names it.
    pub(crate) fn bucket(&self) -> Bucket {
        Bucket {
            endpoint: self.endpoint.clone(),
            name: self.bucket.clone(),
        }
    }

    /// the origin of the service a person names by writing this prefix, when it is not AWS or
    /// google's. it may be on a private network.
    pub(super) fn written_origin(&self) -> Option<url::Origin> {
        self.endpoint.clone()
    }

    /// a pasted HTTPS link to a bucket or a folder in one, ending in `/`. a link to a single
    /// object is left alone, so it stays a plain link.
    pub(super) fn from_link(url: &Url) -> Option<Prefix> {
        if !matches!(url.scheme(), "http" | "https") || url.query().is_some() {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let path = percent_decode_str(url.path().strip_prefix('/')?)
            .decode_utf8()
            .ok()?;
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
        let objects = match self.store {
            Store::S3 => s3::list(self, api).await?,
            Store::Gcs => gcs::list(self, api).await?,
        };
        let files = self.files(objects)?;
        if files.is_empty() {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{self} holds no objects"),
            )
            .help("check the prefix: keys are case sensitive, and a folder's prefix ends with /"));
        }
        Ok(Listing {
            within: Vec::new(),
            access: Access::Unknown,
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

    /// the error for a listing that counted more objects than one dataset may hold.
    fn too_many(&self) -> Error {
        super::too_many(
            self,
            "name a narrower prefix, ending in /, for the folder you need",
        )
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("this usually means the service changed. please report it at https://github.com/nikhil-yn-aix/fetchloom/issues")
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
                let help = match &bucket.endpoint {
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

/// `url` pinned to the S3 object version `version`.
pub(crate) fn with_version(url: &Url, version: &str) -> Url {
    let mut pinned = url.clone();
    let pair = query(&[("versionId", version)]);
    let joined = match url.query().filter(|query| !query.is_empty()) {
        Some(query) => format!("{query}&{pair}"),
        None => pair,
    };
    pinned.set_query(Some(&joined));
    pinned
}

/// `url` without the S3 object version it pins, and that version, when it pins one.
pub(crate) fn unversioned(url: &Url) -> Option<(Url, String)> {
    let pairs: Vec<&str> = url.query()?.split('&').collect();
    let value = pairs
        .iter()
        .find_map(|pair| pair.strip_prefix("versionId="))?;
    let version = percent_decode_str(value).decode_utf8().ok()?.into_owned();
    let kept: Vec<&str> = pairs
        .into_iter()
        .filter(|pair| !pair.starts_with("versionId="))
        .collect();
    let mut current = url.clone();
    current.set_query((!kept.is_empty()).then(|| kept.join("&")).as_deref());
    Some((current, version))
}

/// reads `https://host[:port]/rest` as the origin of a service that speaks S3's API and the path
/// after it. none for a login, a query, a fragment or anything but a plain web origin.
fn endpoint(text: &str) -> Option<(url::Origin, &str)> {
    let (scheme, rest) = text.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") || text.contains(['?', '#']) {
        return None;
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.is_empty() {
        return None;
    }
    let url = Url::parse(&format!("{scheme}://{authority}/")).ok()?;
    let plain = url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/";
    plain.then(|| (url.origin(), path))
}

/// whether `name` follows the bucket naming rules: lowercase letters, digits, `-` and `.`, and
/// `_` in cloud storage, starting and ending with a letter or digit.
fn is_bucket(store: Store, name: &str) -> bool {
    let longest = match store {
        Store::S3 => 63,
        Store::Gcs => 222,
    };
    let bytes = name.as_bytes();
    let ends = |byte: Option<&u8>| byte.is_some_and(u8::is_ascii_alphanumeric);
    (3..=longest).contains(&name.len())
        && ends(bytes.first())
        && ends(bytes.last())
        && !name.contains("..")
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'.')
                || (byte == b'_' && store == Store::Gcs)
        })
}

/// reads an S3 host: the bucket when the host names one, and the region when it names one.
/// `None` when the host is not an S3 endpoint fetchloom lists, such as a website endpoint.
fn s3_host(host: &str) -> Option<(Option<&str>, Option<&str>)> {
    let rest = host.strip_suffix(".amazonaws.com")?;
    let (bucket, endpoint) = if rest == "s3" || rest.starts_with("s3.") || rest.starts_with("s3-") {
        (None, rest)
    } else {
        let at = rest.rfind(".s3")?;
        (Some(&rest[..at]), &rest[at + 1..])
    };
    let region = match endpoint {
        "s3" => None,
        other => {
            let region = other
                .strip_prefix("s3.")
                .or_else(|| other.strip_prefix("s3-"))?;
            if !is_region(region) {
                return None;
            }
            Some(region)
        }
    };
    Some((bucket, region))
}

/// whether `text` reads as an AWS region, such as `us-west-2`.
fn is_region(text: &str) -> bool {
    text.contains('-')
        && !text.starts_with("website")
        && text.ends_with(|c: char| c.is_ascii_digit())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// a query string with every value escaped the same strict way.
fn query(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", utf8_percent_encode(value, UNRESERVED)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `base` with the object key appended to its path, each segment escaped.
fn dotted(key: &str) -> bool {
    key.split('/').any(|part| matches!(part, "." | ".."))
}

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

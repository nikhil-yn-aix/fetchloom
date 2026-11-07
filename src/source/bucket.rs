//! public cloud object storage: an S3 or google cloud storage bucket, or a prefix inside one,
//! listed anonymously over HTTPS. no credential is ever sent to these hosts.

mod gcs;
mod s3;
mod xml;

use std::fmt;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use url::Url;

use super::{Listing, MOST_FILES, Reference, Remote, malformed};
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::{self, Api, Response};

/// the help for a bucket that refuses anonymous reads.
const PRIVATE: &str =
    "fetchloom reads public buckets only; use the provider's tools for private data";

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

/// what stays unescaped in a query value or path segment: the unreserved characters, as both
/// services sign and decode them. S3 reads a `+` in a path as a space, so it is escaped too.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

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

    fn service(self) -> &'static str {
        match self {
            Store::S3 => "S3",
            Store::Gcs => "google cloud storage",
        }
    }
}

/// a bucket and the key prefix inside it, as written. a prefix ending in `/`, or none, names a
/// folder. any other prefix names one object, or the folder of that name when no object has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prefix {
    store: Store,
    bucket: String,
    key: String,
}

/// an object a bucket listed, before its key becomes a dataset path.
struct Object {
    key: String,
    size: u64,
    md5: Option<Md5>,
    url: Url,
}

impl Prefix {
    /// reads the part after `s3:` or `gs:`, such as `//openneuro.org/ds000001/`.
    pub(super) fn parse(store: Store, text: &str) -> Result<Prefix, Error> {
        let bad = || malformed(&format!("{}:{text}", store.scheme()), store.example());
        let rest = text.trim().strip_prefix("//").ok_or_else(bad)?;
        let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
        Prefix::new(store, bucket, key).ok_or_else(bad)
    }

    fn new(store: Store, bucket: &str, key: &str) -> Option<Prefix> {
        let usable_key =
            key.len() <= LONGEST_KEY && !key.chars().any(char::is_control) && !dotted(key);
        (is_bucket(store, bucket) && usable_key).then(|| Prefix {
            store,
            bucket: bucket.to_owned(),
            key: key.to_owned(),
        })
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
        Prefix::new(store, &bucket, key)
    }

    /// lists every object under the prefix, then names each by its key below the prefix.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let objects = match self.store {
            Store::S3 => s3::list(self, api).await?,
            Store::Gcs => gcs::list(self, api).await?,
        };
        let files = self.files(objects);
        if files.is_empty() {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{self} holds no objects"),
            )
            .help("check the prefix: keys are case sensitive, and a folder's prefix ends with /"));
        }
        Ok(Listing {
            resolved: Reference::Bucket(self.clone()),
            title: None,
            license: None,
            doi: None,
            files,
            unavailable: Vec::new(),
            warnings: Vec::new(),
        })
    }

    /// the dataset's files. an object whose key ends in `/` only marks a folder, and keys
    /// that merely start with a prefix not ending in `/` belong to other folders.
    fn files(&self, mut objects: Vec<Object>) -> Vec<Remote> {
        let remote = |object: Object, path: &str| Remote {
            path: path.to_owned(),
            size: Some(object.size),
            sha256: None,
            md5: object.md5,
            url: object.url,
            named_by_server: false,
        };
        let folder = if self.key.is_empty() || self.key.ends_with('/') {
            self.key.clone()
        } else {
            if let Some(at) = objects.iter().position(|object| object.key == self.key) {
                let name = self.key.rsplit('/').next().unwrap_or(&self.key);
                return vec![remote(objects.swap_remove(at), name)];
            }
            format!("{}/", self.key)
        };
        objects
            .into_iter()
            .filter_map(|object| {
                let path = object.key.strip_prefix(&folder)?.to_owned();
                (!path.is_empty() && !path.ends_with('/')).then(|| remote(object, &path))
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
        Error::new(
            Kind::SourceInvalid,
            format!(
                "{self} holds more than the {MOST_FILES} objects fetchloom handles in one dataset"
            ),
        )
        .help("name a narrower prefix, ending in /, for the folder you need")
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("this usually means the service changed. please report it at https://github.com/nikhil-yn-aix/fetchloom/issues")
    }

    /// the error for an answer other than a listing. `code` and `message` are what the service
    /// said, when it said anything.
    fn refused(&self, response: &Response, code: Option<&str>, message: Option<&str>) -> Error {
        let host = net::host(&response.url);
        let said = match (code, message) {
            (Some(code), Some(message)) => Some(format!("{host} said {code}: {message}")),
            (Some(said), None) | (None, Some(said)) => Some(format!("{host} said {said}")),
            (None, None) => None,
        };
        let requester_pays =
            message.is_some_and(|message| message.to_ascii_lowercase().contains("requester pays"));
        let error = match response.status {
            401 | 403 => Error::new(
                Kind::AuthRequired,
                format!(
                    "{self} is not a public bucket: {} refused to list it",
                    self.store.service()
                ),
            )
            .help(PRIVATE),
            400 if requester_pays => Error::new(
                Kind::AuthRequired,
                format!("{self} bills the reader for each request, which needs an account"),
            )
            .help(PRIVATE),
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
}

impl fmt::Display for Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}/{}", self.store.scheme(), self.bucket, self.key)
    }
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

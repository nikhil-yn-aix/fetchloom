//! S3 buckets as the settings and links name them, and which requests to one go signed: a listed
//! bucket's, once S3 refused one without a key this run. a bucket nobody listed is never signed
//! for, whatever a project names, and one whose keys could not be had fails rather than going
//! without them.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use percent_encoding::{percent_decode_str, utf8_percent_encode};
use url::Url;

use super::sign::UNRESERVED;
use crate::auth::aws::{self, Identity, Keys};
use crate::error::{Error, Kind};
use crate::net::sign::{self, Request, Scope};
use crate::shown::Shown;
use crate::stop::Stop;

/// the region a signature names when nothing says where a bucket is: the one S3's global
/// endpoint serves, and the one most other services take.
const DEFAULT_REGION: &str = "us-east-1";

/// reads the environment for the AWS settings, as [`crate::machine::env`] does.
pub(crate) type Environment = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// how a request for an S3 bucket went out, kept on its answer so a refusal can say why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Keyed {
    /// without a key: `s3_buckets` does not list the bucket, or lists it and no keys were found.
    Unsigned { bucket: Bucket, listed: bool },
    /// with the key `key`, as `…WXYZ from AWS_ACCESS_KEY_ID`, for `region`.
    Signed {
        bucket: Bucket,
        key: String,
        region: String,
    },
}

/// what one run knows about signing: the buckets listed, and what it learned on the way.
pub(crate) struct Signing {
    listed: Vec<Bucket>,
    environment: Environment,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// buckets S3 refused without a key this run, asked with one from then on.
    refused: HashSet<Bucket>,
    /// the region S3 named for a bucket in its `x-amz-bucket-region` header.
    regions: HashMap<Bucket, String>,
    /// the keys found for each endpoint, AWS itself under none, read when first needed.
    found: HashMap<Option<url::Origin>, Result<Option<Arc<Identity>>, Error>>,
}

/// the bucket a request is for, as its link names it.
pub(crate) struct Located {
    bucket: Bucket,
    listed: bool,
    /// the region the host itself names, as `s3.us-west-2.amazonaws.com` does.
    region: Option<String>,
}

/// keys and a region that sign one bucket's requests.
pub(crate) struct Signer {
    keys: Arc<Keys>,
    identity: Arc<Identity>,
    region: String,
}

impl Default for Signing {
    fn default() -> Signing {
        Signing::new(Vec::new(), Arc::new(crate::machine::env))
    }
}

impl Signing {
    /// signs for the `listed` buckets, with keys from the settings `environment` reads.
    pub(crate) fn new(listed: Vec<Bucket>, environment: Environment) -> Signing {
        Signing {
            listed,
            environment,
            state: Mutex::default(),
        }
    }

    /// the bucket `url` is for: on AWS by its host and path over https, elsewhere when
    /// `s3_buckets` lists a bucket there. none for a link with a signature or another secret.
    pub(crate) fn locate(&self, url: &Url) -> Option<Located> {
        if crate::shown::secret(url).is_some() {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        let first = url
            .path_segments()
            .and_then(|mut segments| segments.next())
            .unwrap_or_default();
        if let Some((named, region)) = s3_host(&host) {
            let name = named.unwrap_or(first);
            if url.scheme() != "https" || !is_bucket(name) {
                return None;
            }
            let bucket = Bucket {
                endpoint: None,
                name: name.to_owned(),
            };
            return Some(Located {
                listed: self.listed.contains(&bucket),
                bucket,
                region: region.map(str::to_owned),
            });
        }
        let origin = url.origin();
        let bucket = self
            .listed
            .iter()
            .find(|bucket| bucket.endpoint.as_ref() == Some(&origin) && bucket.name == first)?;
        Some(Located {
            bucket: bucket.clone(),
            listed: true,
            region: service_region(&host),
        })
    }

    /// the keys and region that sign a request made at `now` for `located`, once S3 refused its
    /// bucket without a key this run and keys were found; else none, and the request goes unsigned.
    ///
    /// # Errors
    ///
    /// what reading the AWS settings or getting the keys found wrong, such as a
    /// `credential_process` that failed or gave keys that expired, or `interrupted` once `stop`
    /// asks; the request is then not sent at all.
    pub(crate) async fn signer(
        &self,
        located: Option<&Located>,
        now: SystemTime,
        stop: &Stop,
    ) -> Result<Option<Signer>, Error> {
        let Some(located) = located.filter(|located| located.listed) else {
            return Ok(None);
        };
        let (identity, region) = {
            let state = self.state();
            if !state.refused.contains(&located.bucket) {
                return Ok(None);
            }
            let identity = match state.found.get(&located.bucket.endpoint) {
                Some(Ok(Some(identity))) => identity.clone(),
                Some(Err(error)) => return Err(error.clone()),
                Some(Ok(None)) | None => return Ok(None),
            };
            let region = located
                .region
                .clone()
                .or_else(|| state.regions.get(&located.bucket).cloned())
                .or_else(|| identity.region.clone().filter(|region| plausible(region)))
                .unwrap_or_else(|| DEFAULT_REGION.to_owned());
            (identity, region)
        };
        // a program may run a while for the keys, so the state is not held meanwhile
        let keys = identity.keys(now, stop).await?;
        Ok(Some(Signer {
            keys,
            identity,
            region,
        }))
    }

    /// learns from `answer`, S3's 401 or 403 to an unsigned request for `located`, as
    /// [`Signing::refused`] does, in the region it names. says whether keys were found to ask with.
    ///
    /// # Errors
    ///
    /// what reading the AWS settings found wrong.
    pub(crate) fn refused_by(
        &self,
        located: &Located,
        answer: &reqwest::Response,
    ) -> Result<bool, Error> {
        if !matches!(answer.status().as_u16(), 401 | 403) {
            return Ok(false);
        }
        let region = answer
            .headers()
            .get("x-amz-bucket-region")
            .and_then(|value| value.to_str().ok());
        self.refused(located, region)
    }

    /// learns from S3 refusing an unsigned request for `located`: a listed bucket is asked with a
    /// key from now on, in the `region` the refusal named. says whether keys were found.
    ///
    /// # Errors
    ///
    /// what reading the AWS settings found wrong, such as a profile that gets its keys another
    /// way.
    pub(crate) fn refused(&self, located: &Located, region: Option<&str>) -> Result<bool, Error> {
        if !located.listed {
            return Ok(false);
        }
        let mut state = self.state();
        if let Some(region) = region.filter(|region| plausible(region)) {
            state
                .regions
                .insert(located.bucket.clone(), region.to_owned());
        }
        state.refused.insert(located.bucket.clone());
        let endpoint = located.bucket.endpoint.clone();
        let found = state.found.entry(endpoint.clone()).or_insert_with(|| {
            aws::find(&*self.environment, endpoint.as_ref()).map(|found| found.map(Arc::new))
        });
        match found {
            Ok(found) => Ok(found.is_some()),
            Err(error) => Err(error.clone()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Located {
    /// how the request went out, signed by `signer` or not.
    pub(crate) fn keyed(&self, signer: Option<&Signer>) -> Keyed {
        match signer {
            Some(signer) => Keyed::Signed {
                bucket: self.bucket.clone(),
                key: format!("{} from {}", signer.keys.shown(), signer.identity.origin),
                region: signer.region.clone(),
            },
            None => Keyed::Unsigned {
                bucket: self.bucket.clone(),
                listed: self.listed,
            },
        }
    }
}

impl Signer {
    /// whether the keys come from a program, a `credential_process` or the AWS CLI, which may
    /// give new ones when S3 says these expired.
    pub(crate) fn renews(&self) -> bool {
        self.identity.renews()
    }

    /// forgets these keys, which S3 said expired, so the bucket's next request asks the program
    /// for new ones.
    pub(crate) async fn expired(&self) {
        self.identity.forget(&self.keys).await;
    }

    /// whether S3's answer, of `status` with `body`, refused the request because its keys
    /// expired: `ExpiredToken`, or `TokenRefreshRequired`, in its error document.
    pub(crate) fn says_expired(status: u16, body: &[u8]) -> bool {
        let code = std::str::from_utf8(body)
            .ok()
            .and_then(|text| crate::xml::parse(text).ok())
            .filter(|said| said.name == "Error")
            .and_then(|said| said.text_of("Code").map(str::to_owned));
        matches!(status, 400 | 403)
            && matches!(
                code.as_deref(),
                Some("ExpiredToken" | "TokenRefreshRequired")
            )
    }

    /// the headers that sign a GET of `url` made at `now`: the date, the hash of the empty body,
    /// the session of temporary keys, and the authorization, which holds the signature.
    pub(crate) fn headers(&self, url: &Url, now: SystemTime) -> Vec<(&'static str, String)> {
        let keys = &self.keys;
        let time = sign::amz_date(now);
        let host = match (url.host_str(), url.port()) {
            (Some(host), Some(port)) => format!("{host}:{port}"),
            (host, None) => host.unwrap_or_default().to_owned(),
            (None, Some(_)) => String::new(),
        };
        let mut headers = vec![
            ("x-amz-content-sha256", sign::EMPTY.to_owned()),
            ("x-amz-date", time.clone()),
        ];
        if let Some(token) = &keys.token {
            headers.push(("x-amz-security-token", token.clone()));
        }
        let mut signed: Vec<(&str, &str)> = vec![("host", host.as_str())];
        signed.extend(headers.iter().map(|(name, value)| (*name, value.as_str())));
        let scope = Scope {
            time: &time,
            region: &self.region,
            service: "s3",
        };
        let request = Request {
            method: "GET",
            url,
            headers: &signed,
            payload: sign::EMPTY,
        };
        let authorization = sign::authorization(keys, &scope, &request);
        headers.push(("authorization", authorization));
        headers
    }
}

/// the region a service that speaks S3's API names in its host: cloudflare r2 signs for `auto`,
/// backblaze and wasabi put the region after `s3.`.
fn service_region(host: &str) -> Option<String> {
    if host.ends_with(".r2.cloudflarestorage.com") {
        return Some("auto".to_owned());
    }
    [".backblazeb2.com", ".wasabisys.com"]
        .iter()
        .find_map(|service| host.strip_suffix(service)?.strip_prefix("s3."))
        .filter(|region| plausible(region))
        .map(str::to_owned)
}

/// whether `text` can stand as a region in a signature: lowercase letters, digits and `-`.
fn plausible(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// an S3 bucket as the `s3_buckets` setting names it: one on AWS, or one at another service that
/// speaks S3's API, known by its origin.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Bucket {
    endpoint: Option<url::Origin>,
    name: String,
}

impl Bucket {
    /// the bucket `name` on AWS, or at the service at `endpoint`.
    pub(crate) fn new(endpoint: Option<url::Origin>, name: String) -> Bucket {
        Bucket { endpoint, name }
    }

    /// the origin of the service that holds the bucket, none for AWS.
    pub(crate) fn endpoint(&self) -> Option<&url::Origin> {
        self.endpoint.as_ref()
    }

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
        is_bucket(name).then(|| Bucket {
            endpoint,
            name: name.to_owned(),
        })
    }

    /// the bucket a link to one of its objects names: on AWS by its host or first segment,
    /// elsewhere by its origin and first segment, as an S3-like service serves every bucket.
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
        is_bucket(name).then(|| Bucket {
            endpoint,
            name: name.to_owned(),
        })
    }

    /// the service that holds the bucket, as messages name it: S3, or the host of another.
    pub(crate) fn service(&self) -> String {
        let Some(origin) = &self.endpoint else {
            return "S3".to_owned();
        };
        let origin = origin.ascii_serialization();
        origin
            .split_once("://")
            .map_or_else(|| origin.clone(), |(_, host)| host.to_owned())
    }

    /// what was missing when no keys were found to sign for the bucket.
    pub(crate) fn keyless(&self) -> String {
        match &self.endpoint {
            Some(origin) => format!(
                "no AWS settings send keys to {}",
                origin.ascii_serialization()
            ),
            None => "no AWS keys were found".to_owned(),
        }
    }

    /// how to give fetchloom keys for the bucket.
    pub(crate) fn keys_help(&self) -> String {
        match &self.endpoint {
            Some(origin) => {
                let origin = origin.ascii_serialization();
                format!(
                    "put endpoint_url = {origin} in a profile in ~/.aws/config, with its keys in ~/.aws/credentials, or set AWS_ENDPOINT_URL_S3={origin} with AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY"
                )
            }
            None => "set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or AWS_PROFILE to a profile with keys in ~/.aws/credentials, a credential_process, or sso or a role through the AWS CLI. see https://nikhil-yn-aix.github.io/fetchloom/user/guides/private-cloud-storage/#private-s3-buckets".to_owned(),
        }
    }

    /// how to let fetchloom sign for the bucket.
    pub(crate) fn listing_help(&self) -> String {
        format!(
            "if you trust {self} with your AWS keys, add it to s3_buckets in the settings file or FETCHLOOM_S3_BUCKETS"
        )
    }

    /// `text` as a message may repeat it, with any login in it masked.
    pub(crate) fn shown(text: &str) -> String {
        match text.split_once(':') {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("s3") => {
                format!("{scheme}:{}", crate::shown::mask_query(rest))
            }
            _ => crate::shown::mask_query(text).into_owned(),
        }
    }
}

impl Keyed {
    /// the error for a download S3 refused, as this says the request for it went.
    pub(crate) fn refusal(&self, url: &Url) -> Error {
        let shown = Shown(url);
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

/// `url` pinned to the S3 object version `version`.
pub(crate) fn with_version(url: &Url, version: &str) -> Url {
    let mut pinned = url.clone();
    let pair = format!("versionId={}", utf8_percent_encode(version, UNRESERVED));
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
pub(crate) fn endpoint(text: &str) -> Option<(url::Origin, &str)> {
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

/// whether `name` follows S3's bucket naming rules.
pub(crate) fn is_bucket(name: &str) -> bool {
    named(name, 63, false)
}

/// whether `name` is 3 to `longest` lowercase letters, digits, `-` and `.`, and `_` when
/// `underscore`, starting and ending with a letter or digit, as S3 and cloud storage name buckets.
pub(crate) fn named(name: &str, longest: usize, underscore: bool) -> bool {
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
                || (byte == b'_' && underscore)
        })
}

/// reads an S3 host: the bucket when the host names one, and the region when it names one.
/// `None` when the host is not an S3 endpoint fetchloom lists, such as a website endpoint.
pub(crate) fn s3_host(host: &str) -> Option<(Option<&str>, Option<&str>)> {
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
pub(crate) fn is_region(text: &str) -> bool {
    text.contains('-')
        && !text.starts_with("website")
        && text.ends_with(|c: char| c.is_ascii_digit())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::source::recorded::block_on;

    const KEYS: [(&str, &str); 3] = [
        ("AWS_ACCESS_KEY_ID", "AKIAEXAMPLEWXYZ"),
        ("AWS_SECRET_ACCESS_KEY", "do-not-send"),
        ("AWS_SESSION_TOKEN", "session-secret"),
    ];

    const KEYS_HALF: [(&str, &str); 1] = [("AWS_ACCESS_KEY_ID", "AKIAEXAMPLEWXYZ")];

    fn signing(listed: &[&str], pairs: &'static [(&'static str, &'static str)]) -> Signing {
        let listed = listed
            .iter()
            .map(|text| Bucket::parse(text).unwrap())
            .collect();
        Signing::new(
            listed,
            Arc::new(move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }),
        )
    }

    fn url(link: &str) -> Url {
        Url::parse(link).unwrap()
    }

    /// what signs a request for `located`, whose written keys hold at any time.
    fn signer(signing: &Signing, located: &Located) -> Option<Signer> {
        block_on(signing.signer(Some(located), UNIX_EPOCH, &Stop::default())).unwrap()
    }

    #[test]
    fn a_request_names_its_bucket_by_its_host_or_its_path() {
        let signing = signing(&["s3://lab-raw", "s3:http://127.0.0.1:9000/scans"], &[]);
        for (link, bucket, listed, region) in [
            (
                "https://lab-raw.s3.amazonaws.com/a.csv",
                "s3://lab-raw",
                true,
                None,
            ),
            (
                "https://lab-raw.s3.us-west-2.amazonaws.com/?list-type=2",
                "s3://lab-raw",
                true,
                Some("us-west-2"),
            ),
            (
                "https://s3.eu-west-1.amazonaws.com/open.data/x.csv",
                "s3://open.data",
                false,
                Some("eu-west-1"),
            ),
            (
                "https://other.s3.amazonaws.com/a.csv",
                "s3://other",
                false,
                None,
            ),
            (
                "http://127.0.0.1:9000/scans/raw/a.csv",
                "s3:http://127.0.0.1:9000/scans",
                true,
                None,
            ),
        ] {
            let located = signing.locate(&url(link)).unwrap();
            assert_eq!(located.bucket.to_string(), bucket, "{link}");
            assert_eq!(located.listed, listed, "{link}");
            assert_eq!(located.region.as_deref(), region, "{link}");
        }
        for link in [
            "https://lab-raw.s3.amazonaws.com/a.csv?X-Amz-Signature=abc&X-Amz-Credential=x",
            "http://lab-raw.s3.amazonaws.com/a.csv",
            "http://127.0.0.1:9000/other/a.csv",
            "https://127.0.0.1:9000/scans/a.csv",
            "https://storage.googleapis.com/lab-raw/a.csv",
            "https://s3.amazonaws.com/",
        ] {
            assert!(signing.locate(&url(link)).is_none(), "{link}");
        }
    }

    #[test]
    fn a_listed_bucket_is_signed_for_once_s3_refused_it_and_keys_were_found() {
        let signing = signing(&["s3://lab-raw"], &KEYS);
        let located = signing
            .locate(&url("https://lab-raw.s3.amazonaws.com/a.csv"))
            .unwrap();
        assert!(signer(&signing, &located).is_none());
        assert!(signing.refused(&located, Some("eu-west-1")).unwrap());
        let signed = signer(&signing, &located).unwrap();
        assert_eq!(
            located.keyed(Some(&signed)),
            Keyed::Signed {
                bucket: Bucket::parse("s3://lab-raw").unwrap(),
                key: "\u{2026}WXYZ from AWS_ACCESS_KEY_ID".to_owned(),
                region: "eu-west-1".to_owned(),
            }
        );
        let other = signing
            .locate(&url("https://other.s3.amazonaws.com/a.csv"))
            .unwrap();
        assert!(!signing.refused(&other, None).unwrap());
        assert!(signer(&signing, &other).is_none());
        assert_eq!(
            other.keyed(None),
            Keyed::Unsigned {
                bucket: Bucket::parse("s3://other").unwrap(),
                listed: false,
            }
        );
        let keyless = self::signing(&["s3://lab-raw"], &[]);
        let located = keyless
            .locate(&url("https://lab-raw.s3.amazonaws.com/a.csv"))
            .unwrap();
        assert!(!keyless.refused(&located, None).unwrap());
        assert!(signer(&keyless, &located).is_none());
        // keys that could not be read fail each later request too, which never goes without one
        let half = self::signing(&["s3://lab-raw"], &KEYS_HALF);
        let located = half
            .locate(&url("https://lab-raw.s3.amazonaws.com/a.csv"))
            .unwrap();
        let refused = half.refused(&located, None).unwrap_err();
        let later = block_on(half.signer(Some(&located), UNIX_EPOCH, &Stop::default()));
        assert_eq!(
            later.err().map(|error| error.message),
            Some(refused.message)
        );
    }

    #[test]
    fn a_signature_names_the_region_of_the_host_then_of_s3s_answer_then_of_the_settings() {
        const REGIONAL: [(&str, &str); 3] = [
            ("AWS_ACCESS_KEY_ID", "AKIAEXAMPLEWXYZ"),
            ("AWS_SECRET_ACCESS_KEY", "do-not-send"),
            ("AWS_REGION", "ap-south-1"),
        ];
        let region = |signing: &Signing, link: &str, said: Option<&str>| {
            let located = signing.locate(&url(link)).unwrap();
            signing.refused(&located, said).unwrap();
            signer(signing, &located).unwrap().region
        };
        let set = signing(&["s3://lab-raw", "s3://lab-old"], &REGIONAL);
        let plain = signing(&["s3://lab-raw"], &KEYS);
        let host = "https://lab-raw.s3.us-west-2.amazonaws.com/a.csv";
        assert_eq!(region(&set, host, Some("eu-west-1")), "us-west-2");
        let global = "https://lab-raw.s3.amazonaws.com/a.csv";
        assert_eq!(region(&set, global, Some("eu-west-1")), "eu-west-1");
        let old = "https://lab-old.s3.amazonaws.com/a.csv";
        assert_eq!(region(&set, old, Some("Bad Region")), "ap-south-1");
        assert_eq!(region(&plain, global, None), "us-east-1");
        for (host, said) in [
            ("abc123.r2.cloudflarestorage.com", Some("auto")),
            ("s3.us-west-004.backblazeb2.com", Some("us-west-004")),
            ("s3.eu-central-1.wasabisys.com", Some("eu-central-1")),
            ("s3.wasabisys.com", None),
            ("minio.lab.example", None),
        ] {
            assert_eq!(service_region(host).as_deref(), said, "{host}");
        }
    }

    #[test]
    fn signed_headers_carry_the_session_and_never_the_secret() {
        let signing = signing(&["s3://lab-raw"], &KEYS);
        let link = url("https://lab-raw.s3.amazonaws.com/raw/a%20b.csv?versionId=v1");
        let located = signing.locate(&link).unwrap();
        signing.refused(&located, None).unwrap();
        let signed = signer(&signing, &located).unwrap();
        let now = UNIX_EPOCH + Duration::from_hours(380_376);
        let headers = signed.headers(&link, now);
        let names: Vec<_> = headers.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [
                "x-amz-content-sha256",
                "x-amz-date",
                "x-amz-security-token",
                "authorization"
            ]
        );
        assert_eq!(headers[1].1, "20130524T000000Z");
        assert_eq!(headers[2].1, "session-secret");
        assert!(headers[3].1.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIAEXAMPLEWXYZ/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token, Signature="
        ));
        assert!(
            headers
                .iter()
                .all(|(_, value)| !value.contains("do-not-send"))
        );
    }

    /// only S3's word that a request's keys expired asks a program for new ones: not another
    /// refusal, nor those words in another status or outside an error document.
    #[test]
    fn only_an_expired_token_asks_for_new_keys() {
        let error = |code: &str| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{code}</Code><Message>m</Message></Error>"
            )
        };
        for (status, body, expired) in [
            (400, error("ExpiredToken"), true),
            (403, error("ExpiredToken"), true),
            (400, error("TokenRefreshRequired"), true),
            (403, error("AccessDenied"), false),
            (400, error("InvalidToken"), false),
            (500, error("ExpiredToken"), false),
            (400, "ExpiredToken".to_owned(), false),
            (
                400,
                "<Other><Code>ExpiredToken</Code></Other>".to_owned(),
                false,
            ),
        ] {
            assert_eq!(
                Signer::says_expired(status, body.as_bytes()),
                expired,
                "{status} {body}"
            );
        }
    }
}

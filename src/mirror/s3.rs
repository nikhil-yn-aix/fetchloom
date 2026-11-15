//! a mirror in an S3 bucket, or in a bucket at another service that speaks S3's API, below a key
//! prefix. it is read as a web mirror is, each request signed with the keys fetchloom finds for S3
//! as for a bucket `s3_buckets` lists, since a person named it. `fetchloom push` writes one: a HEAD
//! for each file first, then a PUT that S3 checks against the file's SHA-256, or for a file past
//! 5 GiB an upload in parts, taken back when a part fails or a stop comes.

use std::fmt::{self, Write as _};
use std::io::{self, Read as _, Seek as _};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::time::SystemTime;

use bytes::Bytes;
use percent_encoding::utf8_percent_encode;
use reqwest::Method;
use url::Url;

use super::folder::{Item, Put, State, There};
use super::web::joined;
use super::{MARKER, MARKER_LINE, Refusal, below};
use crate::digest::{Hasher, Sha256};
use crate::error::{Error, Kind};
use crate::net::Net;
use crate::net::s3::{Bucket, Keyed, Located, Signer, Signing, region_named};
use crate::net::sign::{EMPTY, UNRESERVED};
use crate::stop::Stop;

/// the longest prefix a mirror's keys go below, so a tree's key stays within the 1,024 bytes S3
/// allows one.
const LONGEST_PREFIX: usize = 900;
/// the largest file one PUT carries, and the largest part: what S3 takes.
const LARGEST_PUT: u64 = 5 * 1024 * 1024 * 1024;
/// the most parts S3 joins into one object.
const MOST_PARTS: u64 = 10_000;
/// the least size of a part but the last, so a file just past 5 GiB goes in few parts.
const LEAST_PART: u64 = 64 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
/// the most of an answer a push reads: S3's error documents and its answers about uploads in parts
/// are a few KiB.
const LARGEST_ANSWER: usize = 1024 * 1024;
/// files a push sends at once, as many as a folder push copies.
const AT_ONCE: usize = 8;
/// a file goes to S3 in pieces of this size, read as they are sent.
const PIECE: u64 = 1024 * 1024;

/// `base`, a bucket's link, with the folders of `prefix` below it, each escaped, ending in `/`.
fn below_prefix(mut base: Url, prefix: &str) -> Url {
    if !prefix.is_empty() {
        let escaped: Vec<String> = prefix
            .split('/')
            .map(|part| utf8_percent_encode(part, UNRESERVED).to_string())
            .collect();
        let path = format!("{}{}/", base.path(), escaped.join("/"));
        base.set_path(&path);
    }
    base
}

/// a mirror's top in a bucket: the bucket, and the prefix its keys go below.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Top {
    bucket: Bucket,
    /// the prefix without a final `/`, empty for the bucket's top.
    prefix: String,
    /// the link to the mirror's top where S3 serves every region, ending in `/`.
    link: Url,
}

impl Top {
    /// reads `s3://bucket/prefix`, or `s3:https://host:port/bucket/prefix` for a bucket at another
    /// service that speaks S3's API, as the `s3_buckets` setting writes a bucket.
    pub(crate) fn parse(text: &str) -> Result<Top, Refusal> {
        let Some((bucket, rest)) = Bucket::split(text.trim()) else {
            let login = text.split_once("://").is_some_and(|(_, rest)| {
                rest.split('/')
                    .next()
                    .is_some_and(|authority| authority.contains('@'))
            });
            if login {
                return Err(Refusal {
                    reason: "it holds a login".to_owned(),
                    instead: "write the address without it: fetchloom signs with the keys your AWS settings give for that service"
                        .to_owned(),
                });
            }
            return Err(Refusal {
                reason: "it names no S3 bucket".to_owned(),
                instead: "write s3://bucket/prefix, with the bucket's name in lowercase, or s3:https://host:port/bucket/prefix for a bucket at another service that speaks S3's API"
                    .to_owned(),
            });
        };
        let prefix = rest.trim_end_matches('/');
        let usable = prefix.len() <= LONGEST_PREFIX
            && !prefix.contains(['?', '#'])
            && !prefix.chars().any(char::is_control)
            && (prefix.is_empty()
                || prefix
                    .split('/')
                    .all(|part| !matches!(part, "" | "." | "..")));
        if !usable {
            return Err(Refusal {
                reason: "its prefix names no folder of keys".to_owned(),
                instead:
                    "write the prefix as names separated by single slashes, without . or .., ? or #"
                        .to_owned(),
            });
        }
        let Some(link) = bucket.base(None).map(|base| below_prefix(base, prefix)) else {
            return Err(Refusal {
                reason: "it makes no valid link".to_owned(),
                instead: "check the bucket's name and the service's address".to_owned(),
            });
        };
        Ok(Top {
            bucket,
            prefix: prefix.to_owned(),
            link,
        })
    }

    pub(crate) fn bucket(&self) -> &Bucket {
        &self.bucket
    }

    /// the link to the mirror's top in `region`, ending in `/`: where S3 serves every region
    /// when none.
    pub(crate) fn link(&self, region: Option<&str>) -> Url {
        region
            .and_then(|region| self.bucket.base(Some(region)))
            .map_or_else(
                || self.link.clone(),
                |base| below_prefix(base, &self.prefix),
            )
    }

    /// the keys a push writes, as a bucket policy names them.
    fn keys(&self) -> String {
        if self.prefix.is_empty() {
            "every object".to_owned()
        } else {
            format!("{}/*", self.prefix)
        }
    }
}

impl fmt::Display for Top {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.prefix.is_empty() {
            write!(f, "{}", self.bucket)
        } else {
            write!(f, "{}/{}", self.bucket, self.prefix)
        }
    }
}

/// what an S3 mirror's marker showed.
pub(crate) enum Look {
    /// the marker, below this link: the mirror's top, in the region S3 keeps the bucket.
    There(Url),
    /// no marker, so the prefix is not a mirror's top.
    NoMarker,
    /// no bucket of that name.
    NoBucket,
    /// a refusal, and how the request went: with a key or without.
    Refused(Option<Keyed>),
    /// another answer, such as 500 after the network's retries.
    Answered(u16),
    /// no answer, after the network's retries.
    Failed(Error),
}

/// asks the S3 mirror at `top` for its marker, following the bucket once to the region S3 names,
/// as a listing does. fails only when a stop is asked.
pub(crate) async fn probe(net: &Net, top: &Top) -> Result<Look, Error> {
    let mut region: Option<String> = None;
    loop {
        let base = top.link(region.as_deref());
        let answer = match net.mirror_get(&joined(&base, &[MARKER])).await {
            Ok(answer) => answer,
            Err(error) if error.kind == Kind::Interrupted => return Err(error),
            Err(error) => return Ok(Look::Failed(error)),
        };
        let said = crate::xml::parse(&answer.text()).ok();
        if matches!(answer.status, 301 | 400)
            && region.is_none()
            && top.bucket.endpoint().is_none()
            && let Some(named) = region_named(answer.region.as_deref(), said.as_ref())
        {
            region = Some(named);
            continue;
        }
        let code = said.as_ref().and_then(|said| said.text_of("Code"));
        return Ok(match answer.status {
            200 if answer.text().lines().next().map(str::trim) == Some(MARKER_LINE) => {
                Look::There(base)
            }
            404 | 410 if code == Some("NoSuchBucket") => Look::NoBucket,
            200 | 404 | 410 => Look::NoMarker,
            401 | 403 => Look::Refused(answer.keyed),
            status => Look::Answered(status),
        });
    }
}

impl Look {
    /// what to do about the S3 mirror at `top` refusing a run's request, as `keyed` says it went.
    pub(crate) fn refused_help(top: &Top, keyed: Option<&Keyed>) -> String {
        match keyed {
            Some(Keyed::Signed { key, .. }) => format!(
                "ask the owner of {} to let the AWS key {key} read it: its policy must grant s3:GetObject on {}",
                top.bucket,
                top.keys()
            ),
            Some(Keyed::Unsigned { bucket, .. }) => {
                format!("{}: {}", bucket.keyless(), bucket.keys_help())
            }
            None => "check the mirror's address".to_owned(),
        }
    }
}

/// where the bytes of a file a push sends come from.
pub(crate) enum Source {
    /// a file in the cache.
    Cached(PathBuf),
    /// bytes made on the spot, as an archive's list of files.
    Made(Vec<u8>),
}

/// a file a push sends: its name on the mirror, what the bucket held there, and where its bytes
/// come from.
pub(crate) struct Upload {
    pub(crate) item: Item,
    pub(crate) there: There,
    pub(crate) source: Source,
}

/// why a file was not pushed.
#[derive(Debug)]
pub(crate) enum Fault {
    /// its bytes could not be read.
    Source(io::Error),
    /// its bytes are not the ones its name says.
    OtherBytes,
    /// the bucket refused it or could not be reached, or a stop came: `interrupted`.
    Bucket(Error),
}

/// the body of a request: none, bytes in memory, or part of a file in the cache, each with the
/// SHA-256 S3 checks it against.
enum Body {
    Empty,
    Bytes(Bytes, Sha256),
    File {
        path: PathBuf,
        /// where the part starts, its length, and the whole file's length.
        offset: u64,
        len: u64,
        whole: u64,
        sha256: Sha256,
    },
}

impl Body {
    fn sha256(&self) -> String {
        match self {
            Body::Empty => EMPTY.to_owned(),
            Body::Bytes(_, sha256) | Body::File { sha256, .. } => sha256.hex(),
        }
    }
}

/// one request as [`Pusher::send`] makes it each time it goes.
struct Sending<'a> {
    method: &'a Method,
    body: &'a Body,
    /// the body's SHA-256 in hex, which the signature names.
    payload: &'a str,
    signer: Option<&'a Signer>,
    /// set once the body proved to hold other bytes than its name says.
    other_bytes: &'a Arc<AtomicBool>,
    /// the bytes of the body sent so far.
    moved: &'a Arc<AtomicU64>,
}

/// what the bucket answered.
struct Answer {
    status: u16,
    /// the `ETag` header, which names a part, and `Content-Length`, which a HEAD gives as the
    /// object's size.
    etag: Option<String>,
    length: Option<u64>,
    body: Vec<u8>,
    keyed: Keyed,
}

impl Answer {
    /// `answered`, which went as `keyed` says, and the region its `x-amz-bucket-region` header
    /// names.
    fn read(answered: crate::net::Answered, keyed: Keyed) -> (Answer, Option<String>) {
        let header = |name: &str| {
            answered
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let elsewhere = header("x-amz-bucket-region");
        let answer = Answer {
            status: answered.status,
            etag: header("etag"),
            length: header("content-length").and_then(|length| length.parse().ok()),
            keyed,
            body: answered.body,
        };
        (answer, elsewhere)
    }

    fn said(&self) -> Option<crate::xml::Element> {
        crate::xml::parse(&String::from_utf8_lossy(&self.body)).ok()
    }

    fn code(&self) -> Option<String> {
        self.said()
            .and_then(|said| said.text_of("Code").map(str::to_owned))
    }

    fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// writes a mirror into an S3 bucket for `fetchloom push`. every request is signed from the first
/// with the keys found for S3, when any are, and goes without a key otherwise.
pub(crate) struct Pusher {
    net: Arc<Net>,
    top: Top,
    signing: Signing,
    /// the region S3 named for the bucket, once a request reached another.
    region: Mutex<Option<String>>,
    stop: Arc<Stop>,
    /// a file bigger than this goes up in parts: 5 GiB, less in tests.
    multipart_above: u64,
    /// the least size of a part but the last.
    least_part: u64,
}

impl Pusher {
    /// a push to `top`, sent through `net`, with the keys the AWS settings give for its bucket.
    ///
    /// # Errors
    ///
    /// what reading the AWS settings found wrong, such as a profile that gets its keys another
    /// way.
    pub(crate) fn new(net: Arc<Net>, top: Top, stop: Arc<Stop>) -> Result<Pusher, Error> {
        let signing = Signing::new(vec![top.bucket.clone()], Arc::new(crate::machine::env));
        // a push signs from its first request, so its writes never go out without a key it has
        signing.refused(&Located::named(top.bucket.clone(), None), None)?;
        net.trust(top.bucket.endpoint().cloned());
        let (multipart_above, least_part) = parts_from_settings();
        Ok(Pusher {
            net,
            top,
            signing,
            region: Mutex::new(None),
            stop,
            multipart_above,
            least_part,
        })
    }

    pub(crate) fn top(&self) -> &Top {
        &self.top
    }

    fn region(&self) -> Option<String> {
        self.region
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// whether the prefix holds a mirror's marker, or none, so a push starts one there.
    ///
    /// # Errors
    ///
    /// a refusal, a bucket that is not there, a marker another program wrote, no answer, or a
    /// stop.
    pub(crate) async fn look(&self) -> Result<State, Error> {
        let answer = self
            .send(Method::GET, &[MARKER], None, Body::Empty, true)
            .await?;
        match answer.status {
            200 if String::from_utf8_lossy(&answer.body)
                .lines()
                .next()
                .map(str::trim)
                == Some(MARKER_LINE) =>
            {
                Ok(State::Mirror)
            }
            200 => Err(Error::new(
                Kind::SourceInvalid,
                format!(
                    "{} holds a fetchloom-mirror file this fetchloom cannot read",
                    self.top
                ),
            )
            .help("name another prefix for the mirror, or update fetchloom")),
            404 | 410 if answer.code().as_deref() != Some("NoSuchBucket") => Ok(State::New),
            _ => Err(self.refusal(&answer)),
        }
    }

    /// looks once at the name of each of `items` in the bucket, as many at once as a push sends,
    /// and says what is there, in order. a new mirror holds nothing.
    ///
    /// # Errors
    ///
    /// the first refusal, failure or stop.
    pub(crate) async fn survey(
        self: &Arc<Self>,
        state: State,
        items: &[Item],
    ) -> Result<Vec<There>, Error> {
        if state == State::New {
            return Ok(vec![There::Absent; items.len()]);
        }
        let looked = at_once(
            items.to_vec(),
            Arc::clone(self),
            |pusher, item| async move { pusher.there(&item).await },
            Result::is_err,
        )
        .await;
        looked.into_iter().collect()
    }

    async fn there(&self, item: &Item) -> Result<There, Error> {
        let answer = self
            .send(
                Method::HEAD,
                &below(item.folder, &item.digest),
                None,
                Body::Empty,
                true,
            )
            .await?;
        match answer.status {
            200 if answer.length == Some(item.size) => Ok(There::Present),
            200 => Ok(There::Damaged),
            404 | 410 => Ok(There::Absent),
            _ => Err(self.refusal(&answer)),
        }
    }

    /// writes the marker, which comes before every file, so a mirror never holds files without it.
    ///
    /// # Errors
    ///
    /// a refusal, no answer, or a stop.
    pub(crate) async fn start(&self) -> Result<(), Error> {
        let line = Bytes::from(format!("{MARKER_LINE}\n"));
        let sha256 = crate::digest::sha256(&line);
        let answer = self
            .send(
                Method::PUT,
                &[MARKER],
                None,
                Body::Bytes(line, sha256),
                true,
            )
            .await?;
        if answer.is_success() {
            Ok(())
        } else {
            Err(self.refusal(&answer))
        }
    }

    /// writes each of `uploads`, as many at once as a folder push copies, and says what each
    /// gave, in order. `sent` hears the size of each file, or part, once the bucket kept it.
    ///
    /// # Errors
    ///
    /// the first file that failed, by its place among `uploads`, once the files on their way
    /// went; those the bucket kept stay.
    pub(crate) async fn put_all(
        self: &Arc<Self>,
        uploads: Vec<Upload>,
        sent: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Result<Vec<Put>, Box<(usize, Fault)>> {
        let uploads: Vec<(usize, Upload)> = uploads.into_iter().enumerate().collect();
        let done = at_once(
            uploads,
            Arc::clone(self),
            move |pusher, (at, upload)| {
                let sent = Arc::clone(&sent);
                async move {
                    pusher
                        .put(&upload, &*sent)
                        .await
                        .map_err(|fault| (at, fault))
                }
            },
            Result::is_err,
        )
        .await;
        let mut puts = Vec::with_capacity(done.len());
        let mut failed: Option<(usize, Fault)> = None;
        for result in done {
            match result {
                Ok(put) => puts.push(put),
                // a stop is told only when nothing else failed
                Err(fault) => match &failed {
                    Some((_, Fault::Bucket(error))) if error.kind == Kind::Interrupted => {
                        failed = Some(fault);
                    }
                    Some(_) => {}
                    None => failed = Some(fault),
                },
            }
        }
        match failed {
            Some(failed) => Err(Box::new(failed)),
            None => Ok(puts),
        }
    }

    async fn put(&self, upload: &Upload, sent: &(dyn Fn(u64) + Send + Sync)) -> Result<Put, Fault> {
        let item = upload.item;
        let put = if upload.there == There::Damaged {
            Put::Replaced
        } else {
            Put::Copied
        };
        let key = below(item.folder, &item.digest);
        let body = match &upload.source {
            Source::Cached(path) if item.size > self.multipart_above => {
                self.put_parts(&key, path, item, sent).await?;
                return Ok(put);
            }
            Source::Cached(path) => Body::File {
                path: path.clone(),
                offset: 0,
                len: item.size,
                whole: item.size,
                sha256: item.digest,
            },
            Source::Made(bytes) => {
                if bytes.len() as u64 != item.size || crate::digest::sha256(bytes) != item.digest {
                    return Err(Fault::OtherBytes);
                }
                Body::Bytes(Bytes::from(bytes.clone()), item.digest)
            }
        };
        let answer = self.send(Method::PUT, &key, None, body, true).await?;
        if !answer.is_success() {
            return Err(Fault::Bucket(self.refusal(&answer)));
        }
        sent(item.size);
        Ok(put)
    }

    /// sends the file at `path` in parts, each checked by S3 against its SHA-256, after the whole
    /// file was read once and matched its pin. a part that fails, or a stop, takes the upload
    /// back, so the bucket keeps none of it.
    async fn put_parts(
        &self,
        key: &[String; 3],
        path: &std::path::Path,
        item: Item,
        sent: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<(), Fault> {
        let part = part_size(item.size, self.least_part).ok_or_else(|| {
            Fault::Bucket(
                Error::new(
                    Kind::Usage,
                    format!(
                        "{} is larger than S3 keeps in one object",
                        crate::units::size(item.size)
                    ),
                )
                .help("split the file, or push the dataset to a folder mirror"),
            )
        })?;
        let hashes = {
            let (path, stop) = (path.to_path_buf(), Arc::clone(&self.stop));
            tokio::task::spawn_blocking(move || hash_parts(&path, item.size, part, &stop))
                .await
                .unwrap_or_else(|err| Err(io::Error::other(err)))
        };
        let (hashes, whole) = match hashes {
            Ok(hashes) => hashes,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {
                return Err(Fault::Bucket(Stop::stopped()));
            }
            Err(err) if err.kind() == io::ErrorKind::InvalidData => return Err(Fault::OtherBytes),
            Err(err) => return Err(Fault::Source(err)),
        };
        if whole != item.digest {
            return Err(Fault::OtherBytes);
        }
        let started = self
            .send(Method::POST, key, Some("uploads"), Body::Empty, true)
            .await?;
        let upload = started
            .said()
            .filter(|said| started.is_success() && said.name == "InitiateMultipartUploadResult")
            .and_then(|said| said.text_of("UploadId").map(str::to_owned))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Fault::Bucket(self.refusal(&started)))?;
        let sent_parts = self
            .send_parts(key, path, item, part, &hashes, &upload, sent)
            .await;
        let finished = match sent_parts {
            Ok(tags) => self.finish_parts(key, &upload, &tags).await,
            Err(fault) => Err(fault),
        };
        if finished.is_err() {
            self.abort(key, &upload).await;
        }
        finished
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "each says which part of which upload"
    )]
    async fn send_parts(
        &self,
        key: &[String; 3],
        path: &std::path::Path,
        item: Item,
        part: u64,
        hashes: &[Sha256],
        upload: &str,
        sent: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<Vec<String>, Fault> {
        let mut tags = Vec::with_capacity(hashes.len());
        for (index, sha256) in hashes.iter().enumerate() {
            let number = index as u64 + 1;
            let offset = index as u64 * part;
            let len = part.min(item.size - offset);
            let query = format!(
                "partNumber={number}&uploadId={}",
                utf8_percent_encode(upload, UNRESERVED)
            );
            let body = Body::File {
                path: path.to_path_buf(),
                offset,
                len,
                whole: item.size,
                sha256: *sha256,
            };
            let answer = self
                .send(Method::PUT, key, Some(&query), body, true)
                .await?;
            let tag = answer.etag.clone().filter(|_| answer.is_success());
            let Some(tag) = tag else {
                return Err(Fault::Bucket(self.refusal(&answer)));
            };
            tags.push(tag);
            sent(len);
        }
        Ok(tags)
    }

    /// asks S3 to join the parts `tags` names into the object.
    async fn finish_parts(
        &self,
        key: &[String; 3],
        upload: &str,
        tags: &[String],
    ) -> Result<(), Fault> {
        let mut document = String::from("<CompleteMultipartUpload>");
        for (index, tag) in tags.iter().enumerate() {
            let _ = write!(
                document,
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                index + 1,
                escaped_xml(tag)
            );
        }
        document.push_str("</CompleteMultipartUpload>");
        let sha256 = crate::digest::sha256(document.as_bytes());
        let query = format!("uploadId={}", utf8_percent_encode(upload, UNRESERVED));
        let answer = self
            .send(
                Method::POST,
                key,
                Some(&query),
                Body::Bytes(Bytes::from(document), sha256),
                true,
            )
            .await?;
        // S3 may answer 200 and still say in the body that joining the parts failed
        let joined = answer
            .said()
            .is_some_and(|said| said.name == "CompleteMultipartUploadResult");
        if answer.is_success() && joined {
            Ok(())
        } else {
            Err(Fault::Bucket(self.refusal(&answer)))
        }
    }

    /// takes back the upload `upload` of `key`, once, even after a stop, so the bucket does not
    /// keep and bill its parts. a failure here leaves them to the bucket's lifecycle rules.
    async fn abort(&self, key: &[String; 3], upload: &str) {
        let query = format!("uploadId={}", utf8_percent_encode(upload, UNRESERVED));
        match self
            .send(Method::DELETE, key, Some(&query), Body::Empty, false)
            .await
        {
            Ok(answer) if answer.is_success() => {}
            Ok(answer) => {
                tracing::debug!(status = answer.status, "S3 kept an upload in parts");
            }
            Err(error) => {
                tracing::debug!(error = %error.message, "could not take back an upload in parts");
            }
        }
    }

    /// sends one request for the mirror file `parts` names, with `query`, signed when keys were
    /// found; follows the bucket once to the region S3 names and asks a program once for new keys
    /// when S3 says they expired. a request not `heeding` a stop goes once, as
    /// [`Net::send_to_bucket`] says.
    async fn send(
        &self,
        method: Method,
        parts: &[impl AsRef<str>],
        query: Option<&str>,
        body: Body,
        heeding: bool,
    ) -> Result<Answer, Error> {
        let payload = body.sha256();
        let other_bytes = Arc::new(AtomicBool::new(false));
        let moved = Arc::new(AtomicU64::new(0));
        let (mut renewed, mut followed) = (false, false);
        loop {
            let region = self.region();
            let located = Located::named(self.top.bucket.clone(), region.clone());
            let signer = self
                .signing
                .signer(Some(&located), SystemTime::now(), &self.stop)
                .await?;
            let mut url = joined(&self.top.link(region.as_deref()), parts);
            url.set_query(query);
            let sending = Sending {
                method: &method,
                body: &body,
                payload: &payload,
                signer: signer.as_ref(),
                other_bytes: &other_bytes,
                moved: &moved,
            };
            let answered = self
                .net
                .send_to_bucket(&url, heeding, LARGEST_ANSWER, &moved, |client, sent| {
                    self.request(client, sent, &sending)
                })
                .await;
            let answered = match answered {
                Ok(answered) => answered,
                Err(_) if other_bytes.load(Ordering::Relaxed) => return Err(other_bytes_error()),
                Err(error) => return Err(error),
            };
            let (answer, elsewhere) = Answer::read(answered, located.keyed(signer.as_ref()));
            if matches!(answer.status, 301 | 400)
                && !followed
                && self.top.bucket.endpoint().is_none()
                && let Some(named) = region_named(elsewhere.as_deref(), answer.said().as_ref())
                && region.as_deref() != Some(named.as_str())
            {
                tracing::debug!(region = named, "S3 keeps the bucket in another region");
                *self.region.lock().unwrap_or_else(PoisonError::into_inner) = Some(named);
                followed = true;
                continue;
            }
            if let Some(signer) = &signer
                && !renewed
                && signer.renews()
                && Signer::says_expired(answer.status, &answer.body)
            {
                signer.expired().await;
                renewed = true;
                continue;
            }
            return Ok(answer);
        }
    }

    /// the request `sending` describes, on `client` for `sent`, the link it goes to, signed when
    /// a signer is there; none once its body proved to hold other bytes than its name says.
    fn request(
        &self,
        client: &reqwest::Client,
        sent: &Url,
        sending: &Sending<'_>,
    ) -> Result<reqwest::RequestBuilder, Error> {
        if sending.other_bytes.load(Ordering::Relaxed) {
            return Err(other_bytes_error());
        }
        let method = sending.method;
        let mut request = client.request(method.clone(), sent.clone());
        if let Some(signer) = sending.signer {
            for (name, value) in
                signer.headers_for(method.as_str(), sent, sending.payload, SystemTime::now())
            {
                request = request.header(name, value);
            }
        }
        Ok(match sending.body {
            Body::Empty if *method == Method::POST => {
                request.header(reqwest::header::CONTENT_LENGTH, 0)
            }
            Body::Empty => request,
            Body::Bytes(bytes, _) => request
                .header(reqwest::header::CONTENT_LENGTH, bytes.len())
                .body(bytes.clone()),
            Body::File {
                path,
                offset,
                len,
                whole,
                sha256,
            } => {
                let part = Part {
                    path: path.clone(),
                    offset: *offset,
                    len: *len,
                    whole: *whole,
                    sha256: *sha256,
                };
                let reading = Reading::start(
                    part,
                    Arc::clone(&self.stop),
                    Arc::clone(sending.other_bytes),
                    Arc::clone(sending.moved),
                );
                request
                    .header(reqwest::header::CONTENT_LENGTH, *len)
                    .body(reqwest::Body::wrap(reading))
            }
        })
    }

    /// the error for `answer`, a refusal or another answer than the push asked for.
    fn refusal(&self, answer: &Answer) -> Error {
        let top = &self.top;
        let bucket = &top.bucket;
        let service = bucket.service();
        let said = answer.said();
        let code = said.as_ref().and_then(|said| said.text_of("Code"));
        let message = said.as_ref().and_then(|said| said.text_of("Message"));
        let error = match (answer.status, code) {
            (401 | 403, _) => match &answer.keyed {
                Keyed::Signed { key, .. } => Error::new(
                    Kind::AuthRequired,
                    format!("{service} refused the AWS key {key} for {top}"),
                )
                .help(format!(
                    "ask the owner of {bucket} to let that key push there: its policy must grant s3:ListBucket on the bucket, and s3:GetObject, s3:PutObject and s3:AbortMultipartUpload on {}. or set AWS_PROFILE to a profile whose key may",
                    top.keys()
                )),
                Keyed::Unsigned { .. } => Error::new(
                    Kind::AuthRequired,
                    format!(
                        "{service} refused the push to {top} without a key, and {}",
                        bucket.keyless()
                    ),
                )
                .help(bucket.keys_help()),
            },
            (404 | 410, Some("NoSuchBucket")) => Error::new(
                Kind::ReferenceUnresolved,
                format!("the bucket {} does not exist on {service}", bucket.name()),
            )
            .help(format!(
                "check the bucket's name in {top}: names are lowercase and exact. push never makes a bucket"
            )),
            (400, Some("AuthorizationHeaderMalformed")) => {
                let region = said
                    .as_ref()
                    .and_then(|said| region_named(None, Some(said)));
                Error::new(
                    Kind::AuthRequired,
                    format!(
                        "{service} wants the requests for {top} signed for {}",
                        region.as_deref().unwrap_or("another region")
                    ),
                )
                .help(match (bucket.endpoint(), region) {
                    (Some(origin), Some(region)) => format!(
                        "set region = {region} in the AWS profile for {}, or AWS_REGION={region}",
                        origin.ascii_serialization()
                    ),
                    (_, Some(region)) => {
                        format!("set AWS_REGION={region}, or region = {region} in your AWS profile")
                    }
                    (_, None) => "set AWS_REGION to the bucket's region".to_owned(),
                })
            }
            (status, _) => Error::new(
                Kind::NetworkStatus,
                format!("{service} answered {status} to the push to {top}"),
            ),
        };
        match (code, message) {
            (Some(code), Some(message)) => {
                error.detail(format!("{service} said {code}: {message}"))
            }
            (Some(said), None) | (None, Some(said)) => {
                error.detail(format!("{service} said {said}"))
            }
            (None, None) => error,
        }
    }
}

/// the error a request ends with when the bytes it sends are not the ones it names.
fn other_bytes_error() -> Error {
    Error::new(
        Kind::IntegrityMismatch,
        "the bytes to push are not the ones their name says",
    )
}

impl From<Error> for Fault {
    fn from(error: Error) -> Fault {
        if error.kind == Kind::IntegrityMismatch {
            Fault::OtherBytes
        } else {
            Fault::Bucket(error)
        }
    }
}

/// the multipart threshold and least part: S3's, or in debug builds the size
/// `FETCHLOOM_TEST_MULTIPART` sets for both, so a test pushes a small file in parts.
fn parts_from_settings() -> (u64, u64) {
    #[cfg(debug_assertions)]
    if let Some(size) = std::env::var("FETCHLOOM_TEST_MULTIPART")
        .ok()
        .and_then(|size| size.parse().ok())
    {
        return (size, size);
    }
    (LARGEST_PUT, LEAST_PART)
}

/// the size of each part but the last of a file of `size` bytes: `least`, or past it the least
/// whole MiB that fits the file in 10,000 parts, as S3 asks; none past what S3 keeps.
fn part_size(size: u64, least: u64) -> Option<u64> {
    let even = size.div_ceil(MOST_PARTS);
    let part = if even <= least {
        least.max(1)
    } else {
        even.next_multiple_of(MIB)
    };
    (part <= LARGEST_PUT && size.div_ceil(part) <= MOST_PARTS).then_some(part)
}

/// the SHA-256 of each `part`-sized piece of the file at `path`, of `size` bytes, and of the whole
/// file, read once.
fn hash_parts(
    path: &std::path::Path,
    size: u64,
    part: u64,
    stop: &Stop,
) -> io::Result<(Vec<Sha256>, Sha256)> {
    let mut file = crate::volume::read_unfollowed(path)?;
    if file.metadata()?.len() != size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "other bytes than pinned",
        ));
    }
    let mut whole = Hasher::default();
    let mut hashes = Vec::new();
    let mut piece = vec![0; usize::try_from(PIECE.min(part)).unwrap_or(usize::MAX)];
    let mut left = size;
    while left > 0 {
        let mut this = Hasher::default();
        let mut in_part = part.min(left);
        left -= in_part;
        while in_part > 0 {
            stop.check_io()?;
            let take = usize::try_from(in_part.min(piece.len() as u64)).unwrap_or(piece.len());
            file.read_exact(&mut piece[..take])?;
            this.update(&piece[..take]);
            whole.update(&piece[..take]);
            in_part -= take as u64;
        }
        hashes.push(this.finish().0);
    }
    Ok((hashes, whole.finish().0))
}

/// `text` as XML holds it between tags.
fn escaped_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// runs `work` on each of `items`, [`AT_ONCE`] at a time, and gives what each gave, in order.
/// once one gives what `ends` says ends the work, no more start, so those on their way finish and
/// the rest are left out.
async fn at_once<T, R, F>(
    items: Vec<T>,
    pusher: Arc<Pusher>,
    work: impl Fn(Arc<Pusher>, T) -> F,
    ends: impl Fn(&R) -> bool,
) -> Vec<R>
where
    T: Send + 'static,
    R: Send + 'static,
    F: Future<Output = R> + Send + 'static,
{
    let count = items.len();
    let mut items = items.into_iter().enumerate();
    let mut done: Vec<Option<R>> = (0..count).map(|_| None).collect();
    let mut running = tokio::task::JoinSet::new();
    let mut ended = false;
    loop {
        while !ended && running.len() < AT_ONCE {
            let Some((at, item)) = items.next() else {
                break;
            };
            let future = work(Arc::clone(&pusher), item);
            running.spawn(async move { (at, future.await) });
        }
        let Some(joined) = running.join_next().await else {
            break;
        };
        let (at, result) = joined.unwrap_or_else(|err| std::panic::resume_unwind(err.into_panic()));
        ended |= ends(&result);
        done[at] = Some(result);
    }
    done.into_iter().flatten().collect()
}

/// a part of a file in the cache, as a request's body sends it.
struct Part {
    path: PathBuf,
    offset: u64,
    len: u64,
    /// the whole file's length, which it must have, and the part's SHA-256.
    whole: u64,
    sha256: Sha256,
}

/// a part of a file as a request's body, read on a thread that may wait on the disk and checked
/// against its SHA-256 as it goes: the last piece waits until they match, so a body that does not
/// hold the part's bytes never arrives whole and the bucket keeps nothing.
struct Reading {
    pieces: tokio::sync::mpsc::Receiver<io::Result<Bytes>>,
    left: u64,
    /// the bytes sent so far, which tells a slow upload from a stalled one.
    moved: Arc<AtomicU64>,
}

impl Reading {
    fn start(
        part: Part,
        stop: Arc<Stop>,
        other_bytes: Arc<AtomicBool>,
        moved: Arc<AtomicU64>,
    ) -> Reading {
        let (sender, pieces) = tokio::sync::mpsc::channel(4);
        let left = part.len;
        tokio::task::spawn_blocking(move || {
            if let Err(err) = read_part(&part, &stop, &sender) {
                if err.kind() == io::ErrorKind::InvalidData {
                    other_bytes.store(true, Ordering::Relaxed);
                }
                let _ = sender.blocking_send(Err(err));
            }
        });
        Reading {
            pieces,
            left,
            moved,
        }
    }
}

/// sends `part` into `sender` piece by piece, holding the last back until the part hashes to its
/// SHA-256. a body nobody reads any more ends it.
fn read_part(
    part: &Part,
    stop: &Stop,
    sender: &tokio::sync::mpsc::Sender<io::Result<Bytes>>,
) -> io::Result<()> {
    let other = || io::Error::new(io::ErrorKind::InvalidData, "other bytes than pinned");
    let mut file = crate::volume::read_unfollowed(&part.path)?;
    if file.metadata()?.len() != part.whole {
        return Err(other());
    }
    file.seek(io::SeekFrom::Start(part.offset))?;
    let mut hasher = Hasher::default();
    let mut left = part.len;
    let mut held: Option<Bytes> = None;
    while left > 0 {
        stop.check_io()?;
        let take = PIECE.min(left);
        let mut piece = vec![0; usize::try_from(take).unwrap_or(usize::MAX)];
        file.read_exact(&mut piece)?;
        hasher.update(&piece);
        left -= take;
        if let Some(earlier) = held.replace(Bytes::from(piece))
            && sender.blocking_send(Ok(earlier)).is_err()
        {
            return Ok(());
        }
    }
    if hasher.finish().0 != part.sha256 {
        return Err(other());
    }
    if let Some(last) = held {
        let _ = sender.blocking_send(Ok(last));
    }
    Ok(())
}

impl hyper::body::Body for Reading {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, io::Error>>> {
        match self.pieces.poll_recv(cx) {
            Poll::Ready(Some(Ok(piece))) => {
                let len = piece.len() as u64;
                self.left = self.left.saturating_sub(len);
                self.moved.fetch_add(len, Ordering::Relaxed);
                Poll::Ready(Some(Ok(hyper::body::Frame::data(piece))))
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) if self.left > 0 => Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the file ended early",
            )))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        hyper::body::SizeHint::with_exact(self.left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_mirror_is_written_as_s3_buckets_writes_a_bucket() {
        for (text, shown, base) in [
            (
                "s3://lab-data/fetchloom",
                "s3://lab-data/fetchloom",
                "https://lab-data.s3.amazonaws.com/fetchloom/",
            ),
            (
                " s3://lab-data/raw/fetchloom/ ",
                "s3://lab-data/raw/fetchloom",
                "https://lab-data.s3.amazonaws.com/raw/fetchloom/",
            ),
            (
                "S3://lab-data",
                "s3://lab-data",
                "https://lab-data.s3.amazonaws.com/",
            ),
            (
                "s3://open.data/a b",
                "s3://open.data/a b",
                "https://s3.amazonaws.com/open.data/a%20b/",
            ),
            (
                "s3:http://127.0.0.1:9000/scans/mirror/",
                "s3:http://127.0.0.1:9000/scans/mirror",
                "http://127.0.0.1:9000/scans/mirror/",
            ),
        ] {
            let top = Top::parse(text).unwrap();
            assert_eq!(top.to_string(), shown, "{text}");
            assert_eq!(top.link(None).as_str(), base, "{text}");
            assert_eq!(Top::parse(shown), Ok(top), "{text}");
        }
        let top = Top::parse("s3://lab-data/fetchloom").unwrap();
        assert_eq!(
            top.link(Some("eu-west-1")).as_str(),
            "https://lab-data.s3.eu-west-1.amazonaws.com/fetchloom/"
        );
        assert_eq!(
            Top::parse("s3:https://ada:secret@minio.lab/scans/m")
                .unwrap_err()
                .reason,
            "it holds a login"
        );
        for text in [
            "s3://Lab-Data/m",
            "s3://ab/m",
            "s3:https://ada:secret@minio.lab/scans/m",
            "s3:ftp://minio.lab/scans",
            "s3:https://minio.lab/scans/m?token=abc",
            "s3://lab-data/a/../b",
            "s3://lab-data/a//b",
            "s3://lab-data/m#top",
            "s3://192.168.1.1/x",
        ] {
            let refusal = Top::parse(text).unwrap_err();
            assert!(!refusal.reason.contains("secret"), "{text}");
        }
    }

    /// a file goes in parts of at least the least size, in whole MiB, and in no more than S3's
    /// 10,000, up to the largest file S3's largest parts make.
    #[test]
    fn a_large_file_fits_in_ten_thousand_parts() {
        const TIB: u64 = 1024 * 1024 * MIB;
        assert_eq!(part_size(6 * 1024 * MIB, LEAST_PART), Some(LEAST_PART));
        let part = part_size(5 * TIB, LEAST_PART).unwrap();
        assert_eq!(part, 525 * MIB);
        assert!((5 * TIB).div_ceil(part) <= MOST_PARTS);
        let largest = LARGEST_PUT * MOST_PARTS;
        assert_eq!(part_size(largest, LEAST_PART), Some(LARGEST_PUT));
        assert_eq!(part_size(largest + 1, LEAST_PART), None);
        assert_eq!(part_size(100, 30), Some(30));
    }

    #[test]
    fn parts_are_hashed_one_by_one_and_whole() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("file");
        let bytes: Vec<u8> = (0..250u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let (hashes, whole) = hash_parts(&path, 250, 100, &Stop::default()).unwrap();
        assert_eq!(
            hashes,
            [
                crate::digest::sha256(&bytes[..100]),
                crate::digest::sha256(&bytes[100..200]),
                crate::digest::sha256(&bytes[200..]),
            ]
        );
        assert_eq!(whole, crate::digest::sha256(&bytes));
    }

    /// a pusher to `top` that finds no keys and sends nothing.
    fn pusher(top: &Top) -> Pusher {
        Pusher {
            net: Arc::new(Net::new(
                crate::net::Options {
                    offline: crate::net::Offline::No,
                    jobs: 1,
                    credentials: Vec::new(),
                    split: crate::net::Splitting::WhenSlow,
                },
                Arc::default(),
                Arc::default(),
                Arc::new(|_| {}),
            )),
            signing: Signing::new(Vec::new(), Arc::new(|_| None)),
            region: Mutex::new(None),
            stop: Arc::default(),
            multipart_above: LARGEST_PUT,
            least_part: LEAST_PART,
            top: top.clone(),
        }
    }

    /// once one file fails, no other file starts, so a refused key costs a few requests rather
    /// than one for each file.
    #[test]
    fn the_first_failure_starts_nothing_more() {
        let top = Top::parse("s3://lab-data/fetchloom").unwrap();
        let started = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&started);
        let done = crate::source::recorded::block_on(at_once(
            (0..100).collect(),
            Arc::new(pusher(&top)),
            move |_, item: u32| {
                counted.fetch_add(1, Ordering::Relaxed);
                async move { if item == 0 { Err(item) } else { Ok(item) } }
            },
            Result::is_err,
        ));
        let started = started.load(Ordering::Relaxed);
        assert!(started < 100, "{started}");
        assert_eq!(done.len() as u64, started);
        assert!(done.contains(&Err(0)));
    }

    /// a refused push names the key, never its secret, and what the bucket's policy must grant.
    #[test]
    fn a_refusal_names_the_key_and_what_the_policy_must_grant() {
        let top = Top::parse("s3://lab-data/fetchloom").unwrap();
        let pusher = pusher(&top);
        let denied = b"<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";
        let answer = |status, body: &[u8], keyed| Answer {
            status,
            etag: None,
            length: None,
            body: body.to_vec(),
            keyed,
        };
        let signed = Keyed::Signed {
            bucket: top.bucket.clone(),
            key: "\u{2026}WXYZ from AWS_ACCESS_KEY_ID".to_owned(),
            region: "us-east-1".to_owned(),
        };
        let error = pusher.refusal(&answer(403, denied, signed.clone()));
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(
            error.message,
            "S3 refused the AWS key \u{2026}WXYZ from AWS_ACCESS_KEY_ID for s3://lab-data/fetchloom"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "ask the owner of s3://lab-data to let that key push there: its policy must grant s3:ListBucket on the bucket, and s3:GetObject, s3:PutObject and s3:AbortMultipartUpload on fetchloom/*. or set AWS_PROFILE to a profile whose key may"
            )
        );
        assert_eq!(error.details, ["S3 said AccessDenied: Access Denied"]);
        let unsigned = Keyed::Unsigned {
            bucket: top.bucket,
            listed: true,
        };
        let error = pusher.refusal(&answer(403, b"", unsigned));
        assert_eq!(
            error.message,
            "S3 refused the push to s3://lab-data/fetchloom without a key, and no AWS keys were found"
        );
        let missing = b"<Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist</Message></Error>";
        let error = pusher.refusal(&answer(404, missing, signed));
        assert_eq!(error.kind, Kind::ReferenceUnresolved);
        assert_eq!(error.message, "the bucket lab-data does not exist on S3");
    }
}

//! small answers read whole, such as the listings and metadata that sources ask for.

use std::borrow::Cow;
use std::future::Future;

use reqwest::header;
use serde::de::DeserializeOwned;
use url::Url;

use super::failure::{self, Failure, Retry};
use super::{Net, host};
use crate::error::{Error, Kind};
use crate::work::Work;

/// the largest answer read whole, so a hostile server cannot fill the memory.
const LARGEST: usize = 64 * 1024 * 1024;

/// the network as a source sees it: one GET at a time. the network layer retries while a server
/// says to try again, and every other answer comes back for the source to read.
pub(crate) trait Api: Sync {
    fn get(&self, url: &Url) -> impl Future<Output = Result<Response, Error>> + Send;

    /// one try at a GET, for a hint a person can do without: an answer a retry or a wait for a
    /// rate limit would get past fails at once instead.
    fn get_once(&self, url: &Url) -> impl Future<Output = Result<Response, Error>> + Send {
        self.get(url)
    }
}

/// what a server answered, with the few headers sources read.
#[derive(Clone, Debug)]
pub(crate) struct Response {
    pub(crate) status: u16,
    /// where the answer came from, after redirects.
    pub(crate) url: Url,
    /// the `rel="next"` target of a `Link` header, for listings sent in pages.
    pub(crate) next: Option<Url>,
    /// how many items a listing sent in parts holds in all, as a `Content-Range` header counts
    /// them: 47 for `items 0-46/47`.
    pub(crate) total: Option<u64>,
    /// the `x-amz-bucket-region` header, which names where an S3 bucket lives when a request
    /// reached the wrong region.
    pub(crate) region: Option<String>,
    /// how a request for an S3 bucket went out: signed with a key or not, and why.
    pub(crate) keyed: Option<crate::source::Keyed>,
    /// how a request for a cloud storage bucket went out: with a google token or not, and why.
    pub(crate) authorized: Option<super::Authorized>,
    pub(crate) body: Vec<u8>,
}

impl Response {
    pub(crate) fn json<T: DeserializeOwned>(&self) -> Result<T, Error> {
        serde_json::from_slice(&self.body).map_err(|err| {
            Error::new(Kind::SourceInvalid, format!("{} sent an answer fetchloom could not read: {err}", host(&self.url)))
                .detail(format!("from {}", super::Shown(&self.url)))
                .help("this usually means the service changed. please report it at https://github.com/nikhil-yn-aix/fetchloom/issues")
        })
    }

    pub(crate) fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

impl Api for Net {
    async fn get(&self, url: &Url) -> Result<Response, Error> {
        self.get_for(url, false).await
    }

    async fn get_once(&self, url: &Url) -> Result<Response, Error> {
        let url = self.asking(url)?;
        if self
            .paces
            .holds(&host(&url).to_ascii_lowercase(), std::time::Instant::now())
        {
            return Err(Error::new(
                Kind::NetworkStatus,
                format!("{} asked for a pause", host(&url)),
            ));
        }
        let _permits = self.permit(&url).await?;
        match self.answer(&url, false).await {
            Ok(response) => Ok(response),
            Err(Failure::Final(err)) => Err(err),
            Err(Failure::Transient { reason, .. } | Failure::Limited { reason, .. }) => {
                Err(Error::new(Kind::NetworkStatus, reason))
            }
        }
    }
}

impl Net {
    /// [`Api::get`] for what a mirror gives, its marker or a tree, which mirror-only mode lets a
    /// request ask a mirror for.
    pub(crate) async fn mirror_get(&self, url: &Url) -> Result<Response, Error> {
        self.get_for(url, true).await
    }

    async fn get_for(&self, url: &Url, mirror: bool) -> Result<Response, Error> {
        let url = &self.asking(url)?;
        let _permits = self.permit(url).await?;
        let mut retry = Retry::new(self, url);
        loop {
            match self.answer(url, mirror).await {
                Ok(response) => return Ok(response),
                Err(Failure::Final(err)) => return Err(err),
                Err(Failure::Limited { reason, wait }) => {
                    retry.limited(reason, wait, false).await?;
                }
                Err(Failure::Transient {
                    reason,
                    retry_after,
                }) => retry.wait(reason, retry_after, false).await?,
            }
        }
    }

    /// where a request for `url` goes, once offline mode and the origins this run may reach let
    /// it: in debug builds, a test server that stands in for a source's API.
    fn asking(&self, url: &Url) -> Result<Url, Error> {
        if self.offline.enabled() {
            return Err(Error::new(
                Kind::OfflineMissing,
                format!("offline, and this needs an answer from {}", host(url)),
            )
            .help(self.offline.retry()));
        }
        #[cfg(debug_assertions)]
        let url = match test_endpoint(url) {
            Some(local) => {
                self.typed(&local);
                local
            }
            None => url.clone(),
        };
        #[cfg(not(debug_assertions))]
        let url = url.clone();
        self.typed
            .check(&url)
            .map_err(|reason| failure::refused(&url, &reason))?;
        Ok(url)
    }

    async fn answer(&self, url: &Url, mirror: bool) -> Result<Response, Failure> {
        let mut response = self
            .send_on(super::Lane::Shared, url, mirror, |request| {
                request.header(
                    header::ACCEPT,
                    "application/json, text/html;q=0.9, */*;q=0.8",
                )
            })
            .await?;
        let status = response.status();
        tracing::debug!(url = %super::Shown(url), status = status.as_u16(), "answer");
        if failure::is_transient(status) {
            return Err(failure::refusal(url, status, response.headers()));
        }
        let text = |name| {
            response
                .headers()
                .get(name)
                .and_then(|value: &header::HeaderValue| value.to_str().ok())
                .map(str::to_owned)
        };
        let next = response
            .headers()
            .get_all(header::LINK)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(next_link)
            .find_map(|link| response.url().join(&link).ok());
        let total = text(header::CONTENT_RANGE).as_deref().and_then(items_total);
        let region = text(header::HeaderName::from_static("x-amz-bucket-region"));
        let keyed = response.extensions().get::<crate::source::Keyed>().cloned();
        let authorized = response.extensions().get::<super::Authorized>().cloned();
        let final_url = response.url().clone();
        let mut body = Vec::new();
        loop {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(self.idle, response.chunk()) => chunk,
                () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(url))),
            };
            match chunk {
                Err(_) => {
                    return Err(failure::transient(format!(
                        "{} sent nothing for {} s",
                        host(url),
                        self.idle.as_secs()
                    )));
                }
                Ok(Err(err)) => {
                    return Err(failure::transient(format!(
                        "the connection to {} broke: {err}",
                        host(url)
                    )));
                }
                Ok(Ok(None)) => break,
                Ok(Ok(Some(chunk))) => {
                    self.throttle(url, chunk.len()).await?;
                    Work::add(&self.work.bytes_down, chunk.len() as u64);
                    if body.len() + chunk.len() > LARGEST {
                        return Err(Failure::Final(Error::new(
                            Kind::SourceInvalid,
                            format!(
                                "{} sent more than 64 MiB where a short answer was expected",
                                host(url)
                            ),
                        )));
                    }
                    body.extend_from_slice(&chunk);
                }
            }
        }
        Ok(Response {
            status: status.as_u16(),
            url: final_url,
            next,
            total,
            region,
            keyed,
            authorized,
            body,
        })
    }
}

/// local catalog replay for CLI tests; release builds never read these overrides.
#[cfg(debug_assertions)]
fn test_endpoint(url: &Url) -> Option<Url> {
    let variable = match url.host_str()? {
        "huggingface.co" => "FETCHLOOM_TEST_HF_API",
        "api.kaggle.com" => "FETCHLOOM_TEST_KAGGLE_API",
        "zenodo.org" => "FETCHLOOM_TEST_ZENODO_API",
        _ => return None,
    };
    let mut local = local_test_server(variable)?;
    local.set_path(url.path());
    local.set_query(url.query());
    local.set_fragment(None);
    Some(local)
}

/// the test server a CLI test stands in with for `url`'s host, a bucket's or a repository's, if
/// it set one.
#[cfg(debug_assertions)]
pub(super) fn stand_in(url: &Url) -> Option<Url> {
    test_bucket(url).or_else(|| test_source(url))
}

/// local object storage for CLI tests, listings and object bytes alike: a request to an S3 or
/// cloud storage host goes to `/<host><path>` on the test server instead. release builds never
/// read this override.
#[cfg(debug_assertions)]
fn test_bucket(url: &Url) -> Option<Url> {
    let host = url.host_str()?;
    let storage = host.ends_with(".amazonaws.com")
        || host == "storage.googleapis.com"
        || host.ends_with(".storage.googleapis.com");
    if !storage {
        return None;
    }
    let mut local = local_test_server("FETCHLOOM_TEST_BUCKETS")?;
    local.set_path(&format!("/{host}{}", url.path()));
    local.set_query(url.query());
    local.set_fragment(None);
    Some(local)
}

/// a local stand-in for the repositories whose CLI tests download as well as list: a request to
/// one of their hosts, API or file, goes to `/<host><path>` on the test server instead. release
/// builds never read this override.
#[cfg(debug_assertions)]
fn test_source(url: &Url) -> Option<Url> {
    let host = url.host_str()?;
    if !matches!(host, "datadryad.org" | "data.mendeley.com") {
        return None;
    }
    let mut local = local_test_server("FETCHLOOM_TEST_SOURCES")?;
    local.set_path(&format!("/{host}{}", url.path()));
    local.set_query(url.query());
    local.set_fragment(None);
    Some(local)
}

#[cfg(debug_assertions)]
pub(super) fn local_test_server(variable: &str) -> Option<Url> {
    let local = Url::parse(&std::env::var(variable).ok()?).ok()?;
    if local.scheme() != "http"
        || local.host_str() != Some("127.0.0.1")
        || !local.username().is_empty()
        || local.password().is_some()
    {
        return None;
    }
    Some(local)
}

/// the count after the slash of a `Content-Range` header that counts items, as a listing in parts
/// sends it.
fn items_total(value: &str) -> Option<u64> {
    let (_, total) = value.trim().strip_prefix("items ")?.rsplit_once('/')?;
    total.trim().parse().ok()
}

/// the target of the `rel="next"` link in a `Link` header.
fn next_link(value: &str) -> Option<String> {
    link_fields(value, ',').find_map(|link| {
        let (target, params) = link.trim().strip_prefix('<')?.split_once('>')?;
        let mut relation = None;
        for param in link_fields(params, ';').skip(1) {
            let Some((name, value)) = param.trim().split_once('=') else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case("anchor") {
                return None;
            }
            // RFC 8288 ignores later rel parameters on the same link.
            if name.trim().eq_ignore_ascii_case("rel") && relation.is_none() {
                relation = Some(link_value(value.trim())?);
            }
        }
        relation?
            .split_ascii_whitespace()
            .any(|value| value.eq_ignore_ascii_case("next"))
            .then(|| target.to_owned())
    })
}

fn link_fields(value: &str, separator: char) -> impl Iterator<Item = &str> {
    let (mut quoted, mut escaped, mut target) = (false, false, false);
    value.split(move |character| {
        if escaped {
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' && !target {
            quoted = !quoted;
        } else if !quoted {
            match character {
                '<' => target = true,
                '>' => target = false,
                _ if character == separator && !target => return true,
                _ => {}
            }
        }
        false
    })
}

fn link_value(value: &str) -> Option<Cow<'_, str>> {
    let Some(quoted) = value.strip_prefix('"') else {
        return Some(Cow::Borrowed(value));
    };
    let quoted = quoted.strip_suffix('"')?;
    if !quoted.contains('\\') {
        return Some(Cow::Borrowed(quoted));
    }
    let mut decoded = String::new();
    let mut characters = quoted.chars();
    while let Some(character) = characters.next() {
        decoded.push(if character == '\\' {
            characters.next()?
        } else {
            character
        });
    }
    Some(Cow::Owned(decoded))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::net::scripted::{reply, serve};
    use crate::net::{Notice, Offline, Options, Splitting};
    use crate::source::recorded::block_on;

    fn net(url: &Url) -> Net {
        let net = Net::new(
            Options {
                offline: Offline::No,
                jobs: 1,
                credentials: Vec::new(),
                split: Splitting::Never,
            },
            Arc::default(),
            Arc::default(),
            Arc::new(|_| {}),
        );
        net.typed(url);
        net
    }

    #[test]
    fn listing_redirects_resolve_next_pages_against_the_final_url() {
        let (url, asked) = serve(vec![
            reply("302 Found", &[("Location", "/listing/page1")], b""),
            reply(
                "200 OK",
                &[(
                    "Link",
                    "<../first>; rel=first, <page2?cursor=abc>; rel=next",
                )],
                br#"[{"path":"data.csv"}]"#,
            ),
        ]);
        let net = net(&url);
        let response = block_on(net.get(&url)).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.url, url.join("listing/page1").unwrap());
        assert_eq!(
            response.next,
            Some(url.join("listing/page2?cursor=abc").unwrap())
        );
        let listing: serde_json::Value = response.json().unwrap();
        assert_eq!(listing[0]["path"], "data.csv");
        let requests = asked.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET / HTTP/1.1"));
        assert!(requests[1].starts_with("GET /listing/page1 HTTP/1.1"));
        assert!(requests[1].contains("accept: application/json, text/html;q=0.9, */*;q=0.8"));
        assert_eq!(net.work.counts().bytes_down, 21);
    }

    #[test]
    fn permanent_listing_statuses_reach_the_source_without_retrying() {
        for (status, kind) in [
            ("401 Unauthorized", Kind::AuthRequired),
            ("403 Forbidden", Kind::AuthRequired),
            ("404 Not Found", Kind::ReferenceUnresolved),
            ("410 Gone", Kind::ReferenceUnresolved),
            ("418 Teapot", Kind::NetworkStatus),
        ] {
            let (url, asked) = serve(vec![reply(status, &[], b"not json")]);
            let net = net(&url);
            let response = block_on(net.get(&url)).unwrap();
            assert_eq!(response.text(), "not json");
            let error = crate::source::json::<serde_json::Value>(&url, &response).unwrap_err();
            assert_eq!(error.kind, kind, "{status}");
            assert_eq!(asked.lock().unwrap().len(), 1);
            assert_eq!(net.work.counts().retries, 0);
        }
    }

    #[test]
    fn invalid_listing_json_reports_the_source_and_masks_query_secrets() {
        for body in [b"{broken".as_slice(), b"[1,2]"] {
            let (mut url, _) = serve(vec![reply("200 OK", &[], body)]);
            url.set_query(Some("token=private-value&view=files"));
            let response = block_on(net(&url).get(&url)).unwrap();
            let error = response
                .json::<std::collections::BTreeMap<String, String>>()
                .unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert!(error.message.contains("127.0.0.1"));
            assert_eq!(error.details.len(), 1);
            assert!(error.details[0].contains("token=***&view=files"));
            assert!(!error.details[0].contains("private-value"));
            assert!(error.help.unwrap().contains("report"));
        }
    }

    #[test]
    fn a_listing_body_at_the_limit_is_accepted_but_one_extra_byte_is_refused() {
        const LIMIT: usize = 64 * 1024 * 1024;
        for size in [LIMIT, LIMIT + 1] {
            // No content length: the limit must be enforced while reading the body.
            let mut answer = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
            answer.resize(answer.len() + size, b'x');
            let (url, _) = serve(vec![answer]);
            let net = net(&url);
            let result = block_on(net.get(&url));
            if size == LIMIT {
                let response = result.unwrap();
                assert_eq!(response.body.len(), LIMIT);
                assert!(response.body.iter().all(|byte| *byte == b'x'));
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.kind, Kind::SourceInvalid);
                assert!(error.message.contains("64 MiB"));
            }
            assert_eq!(net.work.counts().requests, 1);
            assert_eq!(net.work.counts().retries, 0);
        }
    }

    #[test]
    fn a_broken_listing_body_is_retried_without_reusing_its_prefix() {
        let (url, asked) = serve(vec![
            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 100\r\n\r\npartial".to_vec(),
            reply("200 OK", &[], b"[42]"),
        ]);
        let net = net(&url);
        let response = block_on(net.get(&url)).unwrap();
        assert_eq!(response.json::<Vec<u32>>().unwrap(), [42]);
        assert_eq!(asked.lock().unwrap().len(), 2);
        assert_eq!(net.work.counts().retries, 1);
    }

    #[test]
    fn transient_listing_statuses_retry_before_decoding_the_body() {
        let (url, asked) = serve(vec![
            reply("503 Unavailable", &[("Retry-After", "0")], b"not json"),
            reply("200 OK", &[], b"[7]"),
        ]);
        let net = net(&url);
        let response = block_on(net.get(&url)).unwrap();
        assert_eq!(response.json::<Vec<u32>>().unwrap(), [7]);
        assert_eq!(asked.lock().unwrap().len(), 2);
        assert_eq!(net.work.counts().bytes_down, 3);
        assert_eq!(net.work.counts().retries, 1);
    }

    #[test]
    fn listing_rate_limits_get_a_grace_budget_before_spending_retries() {
        let mut responses = vec![reply("429 Too Many Requests", &[("Retry-After", "0")], b""); 11];
        responses.push(reply("200 OK", &[], b"[]"));
        let (url, asked) = serve(responses);
        let mut net = net(&url);
        let retries = Arc::new(Mutex::new(Vec::new()));
        let seen = retries.clone();
        net.notify = Arc::new(move |notice| {
            if let Notice::Retrying { next, of, .. } = notice {
                seen.lock().unwrap().push((next, of));
            }
        });
        let response = block_on(net.get(&url)).unwrap();
        assert_eq!(response.json::<Vec<u32>>().unwrap(), Vec::<u32>::new());
        assert_eq!(asked.lock().unwrap().len(), 12);
        assert_eq!(net.work.counts().retries, 11);
        // the ten waits for the limit count among the tries, so the first retry is the 12th of 15
        assert_eq!(*retries.lock().unwrap(), [(12, 15)]);
    }

    #[test]
    fn listing_waits_beyond_the_budget_fail_without_another_request() {
        for (status, wait) in [("503 Unavailable", "61"), ("429 Too Many Requests", "121")] {
            let (url, asked) = serve(vec![reply(status, &[("Retry-After", wait)], b"")]);
            let net = net(&url);
            let error = block_on(net.get(&url)).unwrap_err();
            assert_eq!(error.kind, Kind::NetworkRateLimited);
            assert!(error.help.unwrap().contains("later"));
            assert_eq!(asked.lock().unwrap().len(), 1);
            assert_eq!(net.work.counts().retries, 0);
        }
    }

    #[test]
    fn offline_listings_never_connect_and_explain_how_to_go_online() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        for mode in [Offline::Flag, Offline::Environment, Offline::Both] {
            let mut net = net(&url);
            net.offline = mode;
            let error = block_on(net.get(&url)).unwrap_err();
            assert_eq!(error.kind, Kind::OfflineMissing);
            let help = error.help.unwrap();
            assert_eq!(help.contains("--offline"), mode != Offline::Environment);
            assert_eq!(help.contains("FETCHLOOM_OFFLINE"), mode != Offline::Flag);
            assert_eq!(net.work.counts().requests, 0);
            assert!(net.wire.get().is_none());
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn a_listing_cannot_contact_an_untrusted_local_address() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let net = net(&Url::parse("https://example.org/").unwrap());
        let error = block_on(net.get(&url)).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkRefused);
        assert_eq!(net.work.counts().requests, 0);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn the_next_page_comes_from_the_link_header() {
        let value = r#"<https://huggingface.co/api/datasets/x/tree/main?cursor=abc>; rel="next", <https://x/first>; rel="first""#;
        assert_eq!(
            next_link(value).as_deref(),
            Some("https://huggingface.co/api/datasets/x/tree/main?cursor=abc")
        );
        assert_eq!(next_link(r#"<https://x/first>; rel="first""#), None);
        assert_eq!(
            next_link("<https://x/2>; rel=next").as_deref(),
            Some("https://x/2")
        );
    }

    #[test]
    fn a_listings_total_comes_from_its_items_range() {
        for (value, wanted) in [
            ("items 0-46/47", Some(47)),
            ("items 2-2/3", Some(3)),
            ("items NaN-NaN/0", Some(0)),
            ("items 0-46/*", None),
            ("bytes 0-46/47", None),
            ("items 0-46", None),
        ] {
            assert_eq!(items_total(value), wanted, "{value}");
        }
    }

    #[test]
    fn pagination_keeps_bracketed_targets_and_relation_lists() {
        for (value, wanted) in [
            (
                r#"<https://example.org/items?cursor=a,b>; rel="next""#,
                Some("https://example.org/items?cursor=a,b"),
            ),
            (r#"<page2>; rel="next last""#, Some("page2")),
            (r#"<page2>; title="a,b;c"; rel="n\ext last""#, Some("page2")),
            (r"<page2>; rel=prev; rel=next", None),
            (r#"<page2>; anchor="/another"; rel=next"#, None),
        ] {
            assert_eq!(next_link(value).as_deref(), wanted, "{value}");
        }
    }

    #[test]
    fn pagination_reads_each_link_header_field() {
        let (url, _) = serve(vec![reply(
            "200 OK",
            &[("Link", "<page0>; rel=prev"), ("Link", "<page2>; rel=next")],
            b"[]",
        )]);
        let response = block_on(net(&url).get(&url)).unwrap();
        assert_eq!(response.next, Some(url.join("page2").unwrap()));
    }

    #[test]
    fn a_listings_total_is_read_from_the_answer() {
        let (url, _) = serve(vec![reply(
            "200 OK",
            &[("Content-Range", "items 0-46/47")],
            b"[]",
        )]);
        let response = block_on(net(&url).get(&url)).unwrap();
        assert_eq!(response.total, Some(47));
    }
}

//! small answers read whole, such as the listings and metadata that sources ask for.

use std::borrow::Cow;
use std::future::Future;

use bytes::BytesMut;
use reqwest::header;
use serde::de::DeserializeOwned;
use url::Url;

use super::failure::{self, Failure, Retry};
use super::{IDLE, Net, host};
use crate::error::{Error, Kind};
use crate::work::Work;

/// the largest answer read whole, so a hostile server cannot fill the memory.
const LARGEST: usize = 64 * 1024 * 1024;

/// the network as a source sees it: one GET at a time. the network layer retries while a server
/// says to try again, and every other answer comes back for the source to read.
pub(crate) trait Api: Sync {
    fn get(&self, url: &Url) -> impl Future<Output = Result<Response, Error>> + Send;
}

/// what a server answered, with the few headers sources read.
#[derive(Clone, Debug)]
pub(crate) struct Response {
    pub(crate) status: u16,
    /// where the answer came from, after redirects.
    pub(crate) url: Url,
    /// the `rel="next"` target of a `Link` header, for listings sent in pages.
    pub(crate) next: Option<Url>,
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
        if self.offline.enabled() {
            return Err(Error::new(
                Kind::OfflineMissing,
                format!("offline, and this needs an answer from {}", host(url)),
            )
            .help(self.offline.retry()));
        }
        self.typed
            .check(url)
            .map_err(|reason| failure::refused(url, &reason))?;
        let _permits = self.permit(url).await?;
        let mut retry = Retry::new(self, url);
        loop {
            match self.answer(url).await {
                Ok(response) => return Ok(response),
                Err(Failure::Final(err)) => return Err(err),
                Err(Failure::Limited { reason }) => retry.limited(reason, false).await?,
                Err(Failure::Transient {
                    reason,
                    retry_after,
                }) => retry.wait(reason, retry_after, false).await?,
            }
        }
    }
}

impl Net {
    async fn answer(&self, url: &Url) -> Result<Response, Failure> {
        Work::add(&self.work.requests, 1);
        let mut response = self
            .send(url, |request| {
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
        let next = text(header::LINK)
            .and_then(|value| next_link(&value))
            .and_then(|link| response.url().join(&link).ok());
        let final_url = response.url().clone();
        let mut body = BytesMut::new();
        loop {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(IDLE, response.chunk()) => chunk,
                () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(url))),
            };
            match chunk {
                Err(_) => {
                    return Err(failure::transient(format!(
                        "{} sent nothing for {} s",
                        host(url),
                        IDLE.as_secs()
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
            body: body.to_vec(),
        })
    }
}

/// the target of the `rel="next"` link in a `Link` header.
fn next_link(value: &str) -> Option<String> {
    value.split(',').find_map(|link| {
        let (target, params) = link.trim().split_once(';')?;
        let is_next = params.split(';').any(|param| {
            let param = param.trim().to_ascii_lowercase();
            param == "rel=\"next\"" || param == "rel=next"
        });
        is_next.then(|| {
            target
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::net::{Notice, Offline, Options, Splitting};
    use crate::source::recorded::block_on;

    fn serve(responses: Vec<Vec<u8>>) -> (Url, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        std::thread::spawn(move || {
            for response in responses {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            if Instant::now() >= deadline {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => return,
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(30)))
                    .unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                    head.push(byte[0]);
                }
                seen.lock().unwrap().push(String::from_utf8(head).unwrap());
                let _ = stream.write_all(&response);
            }
        });
        (url, asked)
    }

    fn reply(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

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
            reply("302 Found", "Location: /listing/page1\r\n", b""),
            reply(
                "200 OK",
                "Link: <../first>; rel=first, <page2?cursor=abc>; rel=next\r\n",
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
            let (url, asked) = serve(vec![reply(status, "", b"not json")]);
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
            let (mut url, _) = serve(vec![reply("200 OK", "", body)]);
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
            reply("200 OK", "", b"[42]"),
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
            reply("503 Unavailable", "Retry-After: 0\r\n", b"not json"),
            reply("200 OK", "", b"[7]"),
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
        let mut responses = vec![reply("429 Too Many Requests", "Retry-After: 0\r\n", b""); 11];
        responses.push(reply("200 OK", "", b"[]"));
        let (url, asked) = serve(responses);
        let mut net = net(&url);
        let retries = Arc::new(Mutex::new(Vec::new()));
        let seen = retries.clone();
        net.notify = Arc::new(move |notice| {
            if let Notice::Retrying { next, .. } = notice {
                seen.lock().unwrap().push(next);
            }
        });
        let response = block_on(net.get(&url)).unwrap();
        assert_eq!(response.json::<Vec<u32>>().unwrap(), Vec::<u32>::new());
        assert_eq!(asked.lock().unwrap().len(), 12);
        assert_eq!(net.work.counts().retries, 11);
        assert_eq!(*retries.lock().unwrap(), [2]);
    }

    #[test]
    fn listing_waits_beyond_the_budget_fail_without_another_request() {
        for (status, wait) in [("503 Unavailable", "61"), ("429 Too Many Requests", "121")] {
            let (url, asked) = serve(vec![reply(
                status,
                &format!("Retry-After: {wait}\r\n"),
                b"",
            )]);
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
}

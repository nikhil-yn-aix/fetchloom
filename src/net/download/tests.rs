use std::io::Write;
use std::sync::{Arc, Mutex};

use super::*;
use crate::net::Options;
use crate::net::scripted::{lines, reply, serve};
use crate::source::recorded::block_on;

impl Chunks for tokio::sync::mpsc::Receiver<Bytes> {
    async fn chunk(&mut self) -> Result<Option<Bytes>, reqwest::Error> {
        Ok(self.recv().await)
    }
}

fn poll<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
    future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

#[tokio::test(start_paused = true)]
async fn a_stalled_adaptive_stream_keeps_its_idle_deadline() {
    let url = Url::parse("https://example.org/file").unwrap();
    let mut net = net(&url);
    net.split.piece = 1024;
    net.split.window = std::time::Duration::from_millis(20);
    net.idle = std::time::Duration::from_millis(250);
    let (source, mut chunks) = tokio::sync::mpsc::channel(16);
    source.send(Bytes::from(vec![b'x'; 3072])).await.unwrap();
    let (sender, mut written) = tokio::sync::mpsc::channel(16);
    let listener = |_| Ok(());
    let mut reading =
        std::pin::pin!(net.read_stream(&url, &mut chunks, &sender, 0, &listener, Some(4096)));
    assert!(poll(reading.as_mut()).is_pending());
    // only 1024 bytes remain, so probe windows cannot hand the body to another stream.
    for _ in 0..12 {
        tokio::time::advance(std::time::Duration::from_millis(20)).await;
        assert!(poll(reading.as_mut()).is_pending());
    }
    tokio::time::advance(std::time::Duration::from_millis(10)).await;
    let std::task::Poll::Ready(Err(Failure::Transient { reason, .. })) = poll(reading.as_mut())
    else {
        panic!("the unchanged idle deadline must end the stalled body");
    };
    assert!(reason.contains("sent nothing for 0 s"), "{reason}");
    assert_eq!(net.work.counts().bytes_down, 3072);
    assert_eq!(
        written.try_recv().unwrap(),
        vec![Bytes::from(vec![b'x'; 3072])]
    );
}

#[tokio::test]
async fn an_uncontended_claim_rechecks_bytes_published_after_the_cache_miss() {
    let bytes = b"data";
    let digest = crate::digest::sha256(bytes);
    let md5 = Md5::from_hex("8d777f385d3dfec8815d20f7496026dc").unwrap();
    for expect in [
        Expect {
            sha256: Some(digest),
            size: Some(4),
            ..Expect::default()
        },
        Expect {
            md5: Some(md5),
            size: Some(4),
            ..Expect::default()
        },
    ] {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache")).unwrap();
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/published",
            server.local_addr().unwrap()
        ))
        .unwrap();
        let net = net(&url);
        let fetch = Fetch {
            expect,
            ..Fetch::new(url.clone())
        };
        assert!(net.in_cache(&fetch, &cache).is_none());

        crate::cache::tests::store(&cache, bytes);
        cache
            .save_stated_md5(
                &md5,
                url.as_str(),
                &crate::cache::Stated {
                    digest,
                    size: 4,
                    version: None,
                },
            )
            .unwrap();
        // publication wins before the first claim attempt, so no contention was observed.
        let fetched = tokio::select! {
            result = net.claim_and_download(&fetch, &cache, None, &|_| Ok(())) => result.unwrap(),
            _ = server.accept() => panic!("a newly claimed download fetched bytes already published after its cache miss"),
        };
        assert_eq!(fetched.origin, Origin::Cache);
        assert_eq!(fetched.digest, digest);
        assert_eq!(fetched.size, 4);
        assert_eq!(net.work.counts().requests, 0);
        assert_eq!(net.work.counts().bytes_down, 0);
        assert_eq!(net.work.counts().downloads_skipped, 1);
    }
}

#[test]
fn a_pinned_sha256_leaves_no_md5_to_compute() {
    let md5 = Md5::from_hex("8d777f385d3dfec8815d20f7496026dc");
    let sha256 = Some(crate::digest::sha256(b"data"));
    assert!(
        !Expect {
            sha256,
            md5,
            size: None,
        }
        .needs_md5(),
        "bytes matching the pinned sha256 match the md5 recorded beside it"
    );
    assert!(
        Expect {
            md5,
            ..Expect::default()
        }
        .needs_md5()
    );
    assert!(!Expect::default().needs_md5());
}

#[test]
fn integrity_errors_mask_query_secrets() {
    let url = Url::parse("https://example.org/data?token=abc&download=1").unwrap();
    let stored = Stored {
        digest: crate::digest::sha256(b"data"),
        size: 4,
        md5: Md5::from_hex("8d777f385d3dfec8815d20f7496026dc"),
        new: true,
    };
    for expect in [
        Expect {
            size: Some(5),
            ..Expect::default()
        },
        Expect {
            sha256: Some(crate::digest::sha256(b"other")),
            ..Expect::default()
        },
        Expect {
            md5: Md5::from_hex("00000000000000000000000000000000"),
            ..Expect::default()
        },
    ] {
        let error = verify(&url, &expect, &stored).unwrap_err();
        assert_eq!(error.kind, Kind::IntegrityMismatch);
        assert!(
            error.message.contains("token=***"),
            "integrity error exposed its query: {}",
            error.message
        );
        assert!(!error.message.contains("abc"), "{}", error.message);
        assert!(error.message.contains("download=1"), "{}", error.message);
    }
}

#[test]
fn a_cache_write_the_disk_refuses_says_what_to_do() {
    let full = local(&io::ErrorKind::StorageFull.into());
    assert_eq!(
        full.help.as_deref(),
        Some("free some space on that disk, then run the command again")
    );
}

fn net(url: &Url) -> Net {
    let options = Options {
        offline: crate::net::Offline::No,
        jobs: 1,
        credentials: Vec::new(),
        split: crate::net::Splitting::WhenSlow,
    };
    let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
    net.typed(url);
    net
}

fn download(start: &Url, divert: Divert) -> Result<Fetched, Error> {
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let fetch = Fetch {
        divert: Some(divert),
        ..Fetch::new(start.clone())
    };
    block_on(net(start).download(&fetch, &cache, &|_| Ok(())))
}

/// follows the link a page holds, relative to where the page came from.
fn follow(page: &Url, _status: u16, text: &str) -> Result<Url, Error> {
    page.join(text.trim())
        .map_err(|err| Error::new(Kind::SourceInvalid, err.to_string()))
}

/// says what reached it.
fn refuse(page: &Url, status: u16, text: &str) -> Result<Url, Error> {
    Err(Error::new(
        Kind::NetworkRateLimited,
        format!("{} {status} {}", page.path(), text.trim()),
    ))
}

const HTML: (&str, &str) = ("Content-Type", "text/html; charset=utf-8");

#[test]
fn a_page_is_read_where_it_came_from_after_redirects() {
    let (url, asked) = serve(vec![
        reply("302 Found", &[("Location", "/pages/confirm")], b""),
        reply("200 OK", &[HTML], b"file"),
        reply(
            "200 OK",
            &[("Content-Type", "application/octet-stream")],
            b"bytes",
        ),
    ]);
    let fetched = download(&url.join("start").unwrap(), follow).unwrap();
    assert_eq!(fetched.size, 5);
    assert_eq!(
        lines(&asked),
        [
            "GET /start HTTP/1.1",
            "GET /pages/confirm HTTP/1.1",
            "GET /pages/file HTTP/1.1"
        ]
    );
}

#[test]
fn a_refusal_page_reaches_the_divert_with_its_status() {
    let (url, _) = serve(vec![reply("403 Forbidden", &[HTML], b"over quota")]);
    let err = download(&url.join("file").unwrap(), refuse).unwrap_err();
    assert_eq!(err.kind, Kind::NetworkRateLimited);
    assert_eq!(err.message, "/file 403 over quota");
}

/// a server nobody typed cannot send a download on to a cloud's metadata service, which oracle
/// keeps at 192.0.0.192, or to a private address wrapped in IPv6.
#[test]
fn a_redirect_to_a_metadata_address_is_refused() {
    for target in [
        "http://192.0.0.192/opc/v2/instance/",
        "http://[::169.254.169.254]/latest/",
        "http://[2002:a9fe:a9fe::1]/latest/",
    ] {
        let (url, _) = serve(vec![reply("302 Found", &[("Location", target)], b"")]);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let net = net(&url);
        let fetch = Fetch::new(url.join("file").unwrap());
        let result = block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                net.download(&fetch, &cache, &|_| Ok(())),
            )
            .await
        });
        let Ok(Err(error)) = result else {
            panic!("the redirect to {target} was followed");
        };
        assert_eq!(
            error.kind,
            Kind::NetworkRefused,
            "{target}: {}",
            error.message
        );
        assert!(
            error.message.contains("private address"),
            "{}",
            error.message
        );
    }
}

#[test]
fn an_html_file_the_server_names_is_the_file() {
    let (url, _) = serve(vec![reply(
        "200 OK",
        &[
            HTML,
            ("Content-Disposition", "attachment; filename=\"index.html\""),
        ],
        b"<html></html>",
    )]);
    let fetched = download(&url.join("file").unwrap(), refuse).unwrap();
    assert_eq!(fetched.name.as_deref(), Some("index.html"));
    assert_eq!(fetched.size, 13);
}

/// a server that ignores `If-Range` answers a resume with the rest of whatever it holds now. a
/// range answer naming another version than the bytes kept must not be appended to them.
#[test]
fn a_resume_answered_from_another_version_starts_over() {
    for (old, new) in [
        (("ETag", "\"v1\""), ("ETag", "\"v2\"")),
        (
            ("Last-Modified", "Tue, 01 Oct 2024 10:00:00 GMT"),
            ("Last-Modified", "Wed, 02 Oct 2024 10:00:00 GMT"),
        ),
    ] {
        // the first answer promises 8 bytes and breaks after 4
        let cut = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 8\r\nDate: Thu, 03 Oct 2024 10:00:00 GMT\r\n{}: {}\r\n\r\naaaa",
            old.0, old.1
        );
        let (url, _) = serve(vec![
            cut.into_bytes(),
            reply(
                "206 Partial Content",
                &[("Content-Range", "bytes 4-7/8"), new],
                b"BBBB",
            ),
            reply("200 OK", &[new], b"BBBBBBBB"),
        ]);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let fetch = Fetch::new(url.clone());
        let fetched = block_on(net(&url).download(&fetch, &cache, &|_| Ok(()))).unwrap();
        let stored = fs::read(cache.object_path(&fetched.digest)).unwrap();
        assert_eq!(String::from_utf8(stored).unwrap(), "BBBBBBBB", "{}", old.0);
    }
}

/// a link that breaks again and again still finishes a file while every try moves it on, as a
/// long download over a connection that drops every few minutes does.
#[test]
fn tries_that_move_the_file_on_earn_the_tries_back() {
    let body = "abcdefg";
    let total = body.len();
    // every answer brings one byte and breaks, more times than a request has tries
    let mut responses = vec![
        format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {total}\r\nETag: \"v1\"\r\n\r\na"
        )
        .into_bytes(),
    ];
    for at in 1..total {
        responses.push(
            format!(
                "HTTP/1.1 206 Partial Content\r\nConnection: close\r\nContent-Length: {}\r\n\
                 Content-Range: bytes {at}-{}/{total}\r\nETag: \"v1\"\r\n\r\n{}",
                total - at,
                total - 1,
                &body[at..=at]
            )
            .into_bytes(),
        );
    }
    assert!(responses.len() > crate::net::RETRIES as usize + 1);
    let (url, asked) = serve(responses);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let net = net(&url);
    let fetched = block_on(net.download(&Fetch::new(url.clone()), &cache, &|_| Ok(()))).unwrap();
    let stored = fs::read(cache.object_path(&fetched.digest)).unwrap();
    assert_eq!(stored, body.as_bytes());
    assert_eq!(asked.lock().unwrap().len(), total);
    assert_eq!(net.work.counts().retries, total as u64 - 1);
}

/// zenodo serves files with neither an `ETag` nor a `Last-Modified`, but states each file's MD5,
/// which proves the joined bytes as a pinned SHA-256 does.
#[test]
fn a_range_nobody_asked_for_is_the_file_from_byte_zero_and_refused_otherwise() {
    let (url, asked) = serve(vec![reply(
        "206 Partial Content",
        &[("Content-Range", "bytes 0-3/4")],
        b"abcd",
    )]);
    let fetched = download(&url.join("file").unwrap(), refuse).unwrap();
    assert_eq!(fetched.size, 4);
    assert_eq!(asked.lock().unwrap().len(), 1);
    // answered again and again, this would start over forever, and a part with no stated total
    // or one short of it would be kept as if it were the whole file
    for range in ["bytes 2-3/4", "bytes 0-3/*", "bytes 0-3/8"] {
        let (url, asked) = serve(vec![
            reply(
                "206 Partial Content",
                &[("Content-Range", range)],
                b"abcd"
            );
            3
        ]);
        assert!(
            download(&url.join("file").unwrap(), refuse).is_err(),
            "{range}"
        );
        assert_eq!(asked.lock().unwrap().len(), 1, "{range}");
    }
}

/// zenodo serves files with neither an `ETag` nor a `Last-Modified`, but states each file's MD5,
/// which proves the joined bytes as a pinned SHA-256 does.
#[test]
fn a_stated_md5_lets_a_download_without_validators_resume() {
    let cut = "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 8\r\n\r\naaaa";
    let (url, _) = serve(vec![
        cut.as_bytes().to_vec(),
        reply(
            "206 Partial Content",
            &[("Content-Range", "bytes 4-7/8")],
            b"bbbb",
        ),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let mut fetch = Fetch::new(url.clone());
    fetch.expect.md5 = Md5::from_hex("c622054d9e6f17b43814ad5d61cab239");
    let fetched = block_on(net(&url).download(&fetch, &cache, &|_| Ok(()))).unwrap();
    let stored = fs::read(cache.object_path(&fetched.digest)).unwrap();
    assert_eq!(stored, b"aaaabbbb");
}

#[test]
fn bidi_controls_are_refused_in_server_names() {
    for disposition in [
        "attachment; filename=\"report\u{202e}txt.exe\"",
        "attachment; filename*=UTF-8''report%E2%80%AEtxt.exe",
    ] {
        let (url, _) = serve(vec![reply(
            "200 OK",
            &[("Content-Disposition", disposition)],
            b"bytes",
        )]);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let fetch = Fetch::new(url.clone());
        let result = block_on(net(&url).download(&fetch, &cache, &|_| Ok(())));
        assert!(result.is_err(), "accepted bidi server filename");
        let error = result.unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(
            error.message.contains("control character"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("report\\u{202e}txt.exe"),
            "{}",
            error.message
        );
        assert!(!error.message.contains('\u{202e}'), "{}", error.message);
        assert!(cache.objects().is_empty());
    }
}

#[test]
fn a_dropbox_page_never_enters_the_cache_as_a_file() {
    let shared = Url::parse("https://www.dropbox.com/s/abc/data.csv?dl=1").unwrap();
    let divert = crate::source::fetch_plan(&shared).divert;
    let divert = divert.unwrap();
    let body = crate::source::recorded::fixture_bytes("dropbox/download-page.html");
    let (url, _) = serve(vec![reply("200 OK", &[HTML], &body)]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let fetch = Fetch {
        from: url.clone(),
        divert: Some(divert),
        revalidate: true,
        ..Fetch::new(shared.clone())
    };
    let error = block_on(net(&url).download(&fetch, &cache, &|_| Ok(()))).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(cache.objects().is_empty());
    assert!(cache.url_record(shared.as_str()).is_none());
}

fn source_fetch(source: &str, from: &Url) -> Fetch {
    let source = Url::parse(source).unwrap();
    Fetch {
        from: from.clone(),
        ..crate::source::fetch_plan(&source)
    }
}

#[test]
fn an_expired_osf_storage_link_is_retried_from_its_stable_url() {
    let body = String::from_utf8(crate::source::recorded::fixture_bytes(
        "osf/expired-storage.xml",
    ))
    .unwrap();
    let (url, asked) = serve(vec![
        reply("302 Found", &[("Location", "/expired")], b""),
        reply(
            "403 Forbidden",
            &[("Content-Type", "application/xml; charset=utf-8")],
            body.as_bytes(),
        ),
        reply("302 Found", &[("Location", "/fresh")], b""),
        reply(
            "200 OK",
            &[("Content-Type", "application/octet-stream")],
            b"data",
        ),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let net = net(&url);
    let fetch = source_fetch(
        "https://osf.io/download/tfa5d/?version=1",
        &url.join("start").unwrap(),
    );
    let fetched = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap();
    assert_eq!(fetched.size, 4);
    assert_eq!(
        fs::read(cache.find(&fetched.digest, Some(4)).unwrap()).unwrap(),
        b"data"
    );
    assert_eq!(net.work.counts().retries, 1);
    assert_eq!(
        lines(&asked),
        [
            "GET /start HTTP/1.1",
            "GET /expired HTTP/1.1",
            "GET /start HTTP/1.1",
            "GET /fresh HTTP/1.1"
        ]
    );
}

#[test]
fn a_plain_osf_403_stays_an_auth_error() {
    let (url, asked) = serve(vec![reply(
        "403 Forbidden",
        &[("Content-Type", "application/xml")],
        b"<Error><Message>Access to this resource is not authorized</Message><Code>AccessDenied</Code></Error>",
    )]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let net = net(&url);
    let fetch = source_fetch("https://osf.io/download/tfa5d/?version=1", &url);
    let error = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(net.work.counts().retries, 0);
    assert_eq!(asked.lock().unwrap().len(), 1);
    assert!(cache.objects().is_empty());
}

#[test]
fn expired_storage_responses_are_not_retried_for_other_sources() {
    let body = String::from_utf8(crate::source::recorded::fixture_bytes(
        "osf/expired-storage.xml",
    ))
    .unwrap();
    for source in [
        "https://example.org/file",
        "http://osf.io/download/tfa5d/",
        "https://osf.io:8443/download/tfa5d/",
        "https://osf.io.example.org/file",
    ] {
        let (url, _) = serve(vec![reply(
            "403 Forbidden",
            &[("Content-Type", "application/xml")],
            body.as_bytes(),
        )]);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let net = net(&url);
        let fetch = source_fetch(source, &url);
        let error = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired, "{source}");
        assert_eq!(net.work.counts().retries, 0);
    }
}

#[test]
fn an_interrupted_pipelined_download_resumes_with_the_right_digest() {
    let body = "abcdefgh".repeat(32 * 1024);
    let received = body.len() / 2;
    let cut = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\nETag: \"v1\"\r\n\r\n{}",
        body.len(),
        &body[..received]
    );
    let range = format!("bytes {received}-{}/{}", body.len() - 1, body.len());
    let (url, asked) = serve(vec![
        cut.into_bytes(),
        reply(
            "206 Partial Content",
            &[("Content-Range", &range), ("ETag", "\"v1\"")],
            &body.as_bytes()[received..],
        ),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let Claim::Owned(partial) = cache.claim(url.as_str()).unwrap() else {
        panic!("nothing else holds the download");
    };
    let net = net(&url);
    let fetch = Fetch::new(url.clone());
    let mut split = Transfer {
        split: false,
        permits: None,
    };
    let result = block_on(net.attempt(&url, &fetch, &partial, None, &|_| Ok(()), &mut split));
    assert!(matches!(result, Err(Failure::Transient { .. })));
    assert_eq!(
        partial.resumable().unwrap().0,
        u64::try_from(received).unwrap()
    );
    drop(partial);
    let fetched = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap();
    assert_eq!(fetched.digest, crate::digest::sha256(body.as_bytes()));
    assert_eq!(
        fs::read(cache.object_path(&fetched.digest)).unwrap(),
        body.as_bytes()
    );
    assert_eq!(net.work.counts().resumes, 1);
    assert_eq!(asked.lock().unwrap().len(), 2);
    assert_eq!(
        fetched.moved,
        u64::try_from(body.len() - received).unwrap(),
        "the bytes the first run kept are not moved again"
    );
}

/// a download that goes on from what an earlier run kept says so before any server answers, so
/// a stop while it waits for a slot or an answer still counts those bytes as kept.
#[test]
fn a_resumed_download_tells_what_an_earlier_run_kept_before_the_server_answers() {
    let body = "abcdefgh".repeat(1024);
    let received = body.len() / 2;
    let cut = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\nETag: \"v1\"\r\n\r\n{}",
        body.len(),
        &body[..received]
    );
    let range = format!("bytes {received}-{}/{}", body.len() - 1, body.len());
    let (url, _) = serve(vec![
        cut.into_bytes(),
        reply(
            "206 Partial Content",
            &[("Content-Range", &range), ("ETag", "\"v1\"")],
            &body.as_bytes()[received..],
        ),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let Claim::Owned(partial) = cache.claim(url.as_str()).unwrap() else {
        panic!("nothing else holds the download");
    };
    let net = net(&url);
    let fetch = Fetch::new(url.clone());
    let mut split = Transfer {
        split: false,
        permits: None,
    };
    let result = block_on(net.attempt(&url, &fetch, &partial, None, &|_| Ok(()), &mut split));
    assert!(matches!(result, Err(Failure::Transient { .. })));
    drop(partial);
    let heard = Mutex::new(Vec::new());
    let listen = |event: Event| {
        let told = match event {
            Event::Kept(bytes) => format!("kept {bytes}"),
            Event::Started { resumed, .. } => format!("started at {resumed}"),
            _ => return Ok(()),
        };
        heard.lock().unwrap().push(told);
        Ok(())
    };
    block_on(net.download(&fetch, &cache, &listen)).unwrap();
    assert_eq!(
        *heard.lock().unwrap(),
        [format!("kept {received}"), format!("started at {received}")]
    );
}

#[test]
fn a_download_moves_its_whole_size_and_a_cached_file_nothing() {
    let (url, _) = serve(vec![reply("200 OK", &[], b"abcd")]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let net = net(&url);
    let mut fetch = Fetch::new(url.clone());
    let fetched = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap();
    assert_eq!(
        (fetched.origin, fetched.size, fetched.moved),
        (Origin::Network, 4, 4)
    );
    fetch.expect.sha256 = Some(fetched.digest);
    let found = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap();
    assert_eq!((found.origin, found.moved), (Origin::Cache, 0));
}

fn md5(bytes: &[u8]) -> Md5 {
    let mut hasher = crate::digest::Hasher::with_md5();
    hasher.update(bytes);
    hasher.finish().1.unwrap()
}

/// a lock may pair a SHA-256 with the MD5 of other bytes. its download checks only the SHA-256,
/// so that MD5 must not come to stand for these bytes when a source states it for another file.
#[test]
fn an_md5_the_download_never_computed_stands_for_nothing() {
    // both downloads ask the same link, so only the check on what the first computed keeps its
    // stated MD5 from standing for the bytes it really served
    let (url, asked) = serve(vec![
        reply("200 OK", &[], b"aaaa"),
        reply("200 OK", &[], b"bbbb"),
    ]);
    let link = url.join("a").unwrap();
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let mut pin = Fetch::new(link.clone());
    pin.expect = Expect {
        sha256: Some(crate::digest::sha256(b"aaaa")),
        md5: Some(md5(b"bbbb")),
        size: Some(4),
    };
    block_on(net(&url).download(&pin, &cache, &|_| Ok(()))).unwrap();
    let mut list = Fetch::new(link);
    list.expect = Expect {
        md5: Some(md5(b"bbbb")),
        size: Some(4),
        ..Expect::default()
    };
    let fetched = block_on(net(&url).download(&list, &cache, &|_| Ok(()))).unwrap();
    assert_eq!(fetched.digest, crate::digest::sha256(b"bbbb"));
    assert_eq!(
        asked.lock().unwrap().len(),
        2,
        "the listed file was asked for"
    );
}

/// two blocks of other bytes with one MD5, the collision Xiaoyun Wang and Hongbo Yu published.
const COLLIDING: [&str; 2] = [
    "d131dd02c5e6eec4693d9a0698aff95c2fcab58712467eab4004583eb8fb7f8955ad340609f4b30283e488832571415a085125e8f7cdc99fd91dbdf280373c5bd8823e3156348f5bae6dacd436c919c6dd53e2b487da03fd02396306d248cda0e99f33420f577ee8ce54b67080a80d1ec69821bcb6a8839396f9652b6ff72a70",
    "d131dd02c5e6eec4693d9a0698aff95c2fcab50712467eab4004583eb8fb7f8955ad340609f4b30283e4888325f1415a085125e8f7cdc99fd91dbd7280373c5bd8823e3156348f5bae6dacd436c919c6dd53e23487da03fd02396306d248cda0e99f33420f577ee8ce54b67080280d1ec69821bcb6a8839396f965ab6ff72a70",
];

#[test]
fn an_md5_checked_for_one_link_proves_nothing_for_another() {
    let [first, second] = COLLIDING.map(|hex| {
        (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect::<Vec<u8>>()
    });
    assert_ne!(first, second);
    assert_eq!(md5(&first), md5(&second));
    let (one, _) = serve(vec![reply("200 OK", &[], &first)]);
    let (other, asked) = serve(vec![reply("200 OK", &[], &second)]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let fetch = |url: &Url, bytes: &[u8]| {
        let mut fetch = Fetch::new(url.join("file").unwrap());
        fetch.expect.md5 = Some(md5(bytes));
        fetch.expect.size = Some(bytes.len() as u64);
        fetch
    };
    for (url, bytes) in [(&one, &first), (&other, &second)] {
        let fetched = block_on(net(url).download(&fetch(url, bytes), &cache, &|_| Ok(()))).unwrap();
        assert_eq!(fetched.digest, crate::digest::sha256(bytes));
    }
    assert_eq!(
        asked.lock().unwrap().len(),
        1,
        "the other link sent its bytes"
    );
    let again = block_on(net(&one).download(&fetch(&one, &first), &cache, &|_| Ok(()))).unwrap();
    assert_eq!(
        again.origin,
        Origin::Cache,
        "the same link needs no download"
    );
    assert_eq!(again.digest, crate::digest::sha256(&first));
}

#[tokio::test(start_paused = true)]
async fn waiting_for_the_rate_cap_is_no_network_silence() {
    let url = Url::parse("https://example.org/file").unwrap();
    let rate = crate::net::Rate::read(&|_| Some("8K".into())).unwrap();
    let mut net = net(&url).with_rate(rate);
    net.idle = std::time::Duration::from_millis(100);
    let (source, mut chunks) = tokio::sync::mpsc::channel(16);
    source
        .send(Bytes::from(vec![b'x'; 1024 * 1024]))
        .await
        .unwrap();
    source.send(Bytes::from(vec![b'y'; 2048])).await.unwrap();
    let (sender, mut written) = tokio::sync::mpsc::channel(16);
    let listener = |_| Ok(());
    let mut reading =
        std::pin::pin!(net.read_stream(&url, &mut chunks, &sender, 0, &listener, None));
    assert!(poll(reading.as_mut()).is_pending());
    tokio::time::advance(std::time::Duration::from_millis(250)).await;
    assert!(poll(reading.as_mut()).is_pending());
    drop(source);
    assert!(matches!(
        poll(reading.as_mut()),
        std::task::Poll::Ready(Ok(None))
    ));
    assert_eq!(written.try_recv().unwrap()[0].len(), 1024 * 1024);
    assert_eq!(written.try_recv().unwrap()[0].len(), 2048);
}

#[tokio::test(start_paused = true)]
async fn waiting_for_an_answer_is_no_network_silence() {
    let url = Url::parse("https://example.org/file").unwrap();
    let mut net = net(&url);
    net.idle = std::time::Duration::from_millis(400);
    let (source, mut chunks) = tokio::sync::mpsc::channel(16);
    source.send(Bytes::from_static(b"da")).await.unwrap();
    let (sender, mut written) = tokio::sync::mpsc::channel(16);
    let first = std::sync::atomic::AtomicBool::new(true);
    let asking = |_: Event| {
        if first.swap(false, std::sync::atomic::Ordering::Relaxed) {
            // advancing the virtual clock happens before advance's first yield.
            let mut answer =
                std::pin::pin!(tokio::time::advance(std::time::Duration::from_millis(800)));
            let _ = poll(answer.as_mut());
        }
        Ok(())
    };
    let mut reading = std::pin::pin!(net.read_stream(&url, &mut chunks, &sender, 0, &asking, None));
    assert!(poll(reading.as_mut()).is_pending());
    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    source.send(Bytes::from_static(b"ta")).await.unwrap();
    drop(source);
    assert!(matches!(
        poll(reading.as_mut()),
        std::task::Poll::Ready(Ok(None))
    ));
    assert_eq!(
        written.try_recv().unwrap(),
        vec![Bytes::from_static(b"da"), Bytes::from_static(b"ta")]
    );
}

/// a pinned size bounds what a download takes, so a far larger answer never needs approval.
#[test]
fn an_answer_stating_another_length_than_the_pin_is_refused_unread() {
    let body = "x".repeat(256 * 1024);
    let (url, asked) = serve(vec![reply("200 OK", &[], body.as_bytes())]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let mut fetch = Fetch::new(url.clone());
    fetch.expect = Expect {
        sha256: Some(crate::digest::sha256(b"data")),
        size: Some(4),
        ..Expect::default()
    };
    let net = net(&url);
    let error = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap_err();
    assert_eq!(error.kind, Kind::IntegrityMismatch);
    assert_eq!(error.details, ["expected 4 bytes, got 262144"]);
    assert_eq!(net.work.counts().bytes_down, 0);
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[test]
fn a_body_of_unstated_length_stops_past_the_pinned_size() {
    let chunk = format!("1000\r\n{}\r\n", "x".repeat(4096));
    let chunks: String = std::iter::repeat_n(chunk, 64).collect();
    let answer = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n{chunks}0\r\n\r\n"
    );
    let (url, _) = serve(vec![answer.into_bytes()]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let mut fetch = Fetch::new(url.clone());
    fetch.expect = Expect {
        sha256: Some(crate::digest::sha256(b"data")),
        size: Some(4),
        ..Expect::default()
    };
    let net = net(&url);
    let error = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap_err();
    assert_eq!(error.kind, Kind::IntegrityMismatch);
    assert!(
        net.work.counts().bytes_down < 64 * 4096,
        "read {} bytes",
        net.work.counts().bytes_down
    );
}

#[test]
fn malformed_tags_do_not_enable_range_splitting() {
    let url = Url::parse("https://example.org/file").unwrap();
    let net = net(&url);
    for tag in ["", "v1", "w/\"v1\""] {
        let mut head = header::HeaderMap::new();
        head.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
        head.insert(header::ETAG, tag.parse().unwrap());
        let proof = if_range(&headers::validators(&head)).is_some();
        assert!(
            !net.may_split(Some(SPLIT_ABOVE), StatusCode::OK, &head, proof),
            "{tag:?}"
        );
        assert!(net.may_split(Some(SPLIT_ABOVE), StatusCode::OK, &head, true));
    }
}

#[test]
fn file_bodies_use_the_separate_connection_pool() {
    let (url, asked) = serve(vec![reply("200 OK", &[], b"data")]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let net = net(&url);
    let fetched = block_on(net.download(&Fetch::new(url.clone()), &cache, &|_| Ok(()))).unwrap();
    assert_eq!(
        fs::read(cache.object_path(&fetched.digest)).unwrap(),
        b"data"
    );
    assert!(net.wire().unwrap().apart.get().is_some());
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[test]
fn a_checksum_restart_reacquires_released_download_slots() {
    let (url, asked) = serve(vec![
        reply(
            "206 Partial Content",
            &[("Content-Range", "bytes 4-7/8"), ("ETag", "\"v1\"")],
            b"bbbb",
        ),
        reply("200 OK", &[("ETag", "\"v1\"")], b"bbbbbbbb"),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let Claim::Owned(partial) = cache.claim(url.as_str()).unwrap() else {
        panic!("the fixture owns its partial")
    };
    let mut writer = partial
        .writer(
            0,
            Some(8),
            Validators {
                etag: Some("\"v1\"".to_owned()),
                ..Validators::default()
            },
            false,
            false,
        )
        .unwrap();
    writer.write_all(b"aaaa").unwrap();
    writer.save().unwrap();
    drop(writer);
    drop(partial);
    let net = net(&url);
    let mut fetch = Fetch::new(url.clone());
    fetch.expect.sha256 = Some(crate::digest::sha256(b"bbbbbbbb"));
    let fetched = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap();
    assert_eq!(
        fs::read(cache.object_path(&fetched.digest)).unwrap(),
        b"bbbbbbbb"
    );
    assert_eq!(asked.lock().unwrap().len(), 2);
}

/// the S3 version a bucket names for the bytes comes with them: from a whole answer, from a
/// resume whose range does not name it again, and from the record of a link asked again when
/// the server says nothing changed.
#[test]
fn an_s3_version_comes_with_the_bytes_however_they_arrive() {
    let version = ("x-amz-version-id", "3HL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY");
    let (url, _) = serve(vec![
        reply("200 OK", &[("ETag", "\"e1\""), version], b"abcdefgh"),
        reply("304 Not Modified", &[("ETag", "\"e1\"")], b""),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let asking = net(&url);
    let fetch = Fetch {
        revalidate: true,
        ..Fetch::new(url.clone())
    };
    let fetched = block_on(asking.download(&fetch, &cache, &|_| Ok(()))).unwrap();
    assert_eq!(fetched.version.as_deref(), Some(version.1));
    let again = block_on(asking.download(&fetch, &cache, &|_| Ok(()))).unwrap();
    assert_eq!(again.origin, Origin::NotModified);
    assert_eq!(again.version.as_deref(), Some(version.1));

    let cut = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 8\r\nETag: \"e1\"\r\n{}: {}\r\n\r\nabcd",
        version.0, version.1
    );
    let (url, _) = serve(vec![
        cut.into_bytes(),
        reply(
            "206 Partial Content",
            &[("Content-Range", "bytes 4-7/8"), ("ETag", "\"e1\"")],
            b"efgh",
        ),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let fetched =
        block_on(net(&url).download(&Fetch::new(url.clone()), &cache, &|_| Ok(()))).unwrap();
    assert_eq!(fetched.version.as_deref(), Some(version.1));
    assert_eq!(fetched.size, 8);
}

/// a pinned S3 version that can no longer be read, refused or gone, is fetched from the object
/// there now, and its pinned hash decides: the same bytes are taken, other bytes are a mismatch
/// that says the version could not be read. a file nothing pins keeps the refusal.
#[test]
fn an_unreadable_pinned_version_comes_from_the_object_there_now() {
    for (status, now, kind) in [
        ("403 Forbidden", b"abcd".as_slice(), None),
        ("404 Not Found", b"abcd", None),
        ("403 Forbidden", b"wxyz", Some(Kind::IntegrityMismatch)),
    ] {
        let (url, asked) = serve(vec![reply(status, &[], b""), reply("200 OK", &[], now)]);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let versioned = url.join("raw/a.csv?versionId=v%2B1").unwrap();
        let fetch = Fetch {
            expect: Expect {
                sha256: Some(crate::digest::sha256(b"abcd")),
                md5: None,
                size: Some(4),
            },
            ..Fetch::new(versioned.clone())
        };
        let result = block_on(net(&url).download(&fetch, &cache, &|_| Ok(())));
        assert_eq!(
            lines(&asked),
            [
                "GET /raw/a.csv?versionId=v%2B1 HTTP/1.1",
                "GET /raw/a.csv HTTP/1.1"
            ],
            "{status}"
        );
        match kind {
            None => assert_eq!(result.unwrap().digest, crate::digest::sha256(b"abcd")),
            Some(kind) => {
                let error = result.unwrap_err();
                assert_eq!(error.kind, kind);
                assert!(
                    error
                        .details
                        .iter()
                        .any(|detail| detail.contains("its pinned version v+1 could not be read")),
                    "{:?}",
                    error.details
                );
            }
        }
    }
    let (url, asked) = serve(vec![reply("403 Forbidden", &[], b"")]);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let fetch = Fetch::new(url.join("raw/a.csv?versionId=v1").unwrap());
    let error = block_on(net(&url).download(&fetch, &cache, &|_| Ok(()))).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(asked.lock().unwrap().len(), 1);
}

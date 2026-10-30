use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::*;
use crate::net::Options;
use crate::source::recorded::block_on;

#[test]
fn a_stalled_adaptive_stream_keeps_its_idle_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let (release, stalled) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
            head.push(byte[0]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nAccept-Ranges: bytes\r\nETag: \"v1\"\r\n\r\n",
            )
            .unwrap();
        stream.write_all(&[b'x'; 3072]).unwrap();
        // keep the body open so only the client's idle deadline can end the attempt.
        let _ = stalled.recv();
    });
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let Claim::Owned(partial) = cache.claim(url.as_str()).unwrap() else {
        panic!("nothing else holds the download");
    };
    let mut net = net(&url);
    net.split.above = 4096;
    net.split.piece = 1024;
    let fetch = Fetch::new(url.clone());
    let mut split = true;
    let started = std::time::Instant::now();
    // the remaining 1024 bytes cannot split, but every rate window still wakes the stream.
    let result = block_on(async {
        tokio::time::timeout(
            IDLE + std::time::Duration::from_secs(5),
            net.attempt(&url, &fetch, &partial, None, &|_| Ok(()), &mut split),
        )
        .await
    });
    let elapsed = started.elapsed();
    drop(release);
    server.join().unwrap();
    let Ok(Err(Failure::Transient { reason, .. })) = result else {
        panic!("the stalled body did not reach its idle timeout");
    };
    assert!(reason.contains("sent nothing for 30 s"), "{reason}");
    assert!(elapsed >= IDLE, "{elapsed:?}");
    assert!(elapsed < IDLE + std::time::Duration::from_secs(5), "{elapsed:?}");
    assert_eq!(net.work.counts().bytes_down, 3072);
    assert_eq!(partial.resumable().unwrap().0, 3072);
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

/// a server on this machine that answers each connection with the next response, and the request
/// line of each connection it took.
fn serve(responses: Vec<Vec<u8>>) -> (Url, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    std::thread::spawn(move || {
        for response in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                head.push(byte[0]);
            }
            let line = String::from_utf8_lossy(&head)
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            seen.lock().unwrap().push(line);
            let _ = stream.write_all(&response);
        }
    });
    (url, asked)
}

fn reply(status: &str, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
    let mut text = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        text.push_str(name);
        text.push_str(": ");
        text.push_str(value);
        text.push_str("\r\n");
    }
    text.push_str("\r\n");
    text.push_str(body);
    text.into_bytes()
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
        reply("302 Found", &[("Location", "/pages/confirm")], ""),
        reply("200 OK", &[HTML], "file"),
        reply(
            "200 OK",
            &[("Content-Type", "application/octet-stream")],
            "bytes",
        ),
    ]);
    let fetched = download(&url.join("start").unwrap(), follow).unwrap();
    assert_eq!(fetched.size, 5);
    assert_eq!(
        *asked.lock().unwrap(),
        [
            "GET /start HTTP/1.1",
            "GET /pages/confirm HTTP/1.1",
            "GET /pages/file HTTP/1.1"
        ]
    );
}

#[test]
fn a_refusal_page_reaches_the_divert_with_its_status() {
    let (url, _) = serve(vec![reply("403 Forbidden", &[HTML], "over quota")]);
    let err = download(&url.join("file").unwrap(), refuse).unwrap_err();
    assert_eq!(err.kind, Kind::NetworkRateLimited);
    assert_eq!(err.message, "/file 403 over quota");
}

#[test]
fn an_html_file_the_server_names_is_the_file() {
    let (url, _) = serve(vec![reply(
        "200 OK",
        &[
            HTML,
            ("Content-Disposition", "attachment; filename=\"index.html\""),
        ],
        "<html></html>",
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
            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 8\r\n{}: {}\r\n\r\naaaa",
            old.0, old.1
        );
        let (url, _) = serve(vec![
            cut.into_bytes(),
            reply(
                "206 Partial Content",
                &[("Content-Range", "bytes 4-7/8"), new],
                "BBBB",
            ),
            reply("200 OK", &[new], "BBBBBBBB"),
        ]);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let fetch = Fetch::new(url.clone());
        let fetched = block_on(net(&url).download(&fetch, &cache, &|_| Ok(()))).unwrap();
        let stored = fs::read(cache.object_path(&fetched.digest)).unwrap();
        assert_eq!(String::from_utf8(stored).unwrap(), "BBBBBBBB", "{}", old.0);
    }
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
            "bytes",
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
    let (url, _) = serve(vec![reply(
        "200 OK",
        &[HTML],
        &String::from_utf8_lossy(&body),
    )]);
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
        reply("302 Found", &[("Location", "/expired")], ""),
        reply(
            "403 Forbidden",
            &[("Content-Type", "application/xml; charset=utf-8")],
            &body,
        ),
        reply("302 Found", &[("Location", "/fresh")], ""),
        reply(
            "200 OK",
            &[("Content-Type", "application/octet-stream")],
            "data",
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
        *asked.lock().unwrap(),
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
        "<Error><Message>Access to this resource is not authorized</Message><Code>AccessDenied</Code></Error>",
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
            &body,
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

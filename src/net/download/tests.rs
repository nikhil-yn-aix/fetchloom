use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::*;
use crate::net::Options;
use crate::source::recorded::block_on;

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
        offline: false,
        jobs: 1,
        credentials: Vec::new(),
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

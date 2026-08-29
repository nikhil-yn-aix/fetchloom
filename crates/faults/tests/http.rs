//! Contract tests over the adversarial server itself.
//!
//! The server is a fixture the transfer suite stands on, so what it does has to
//! be asserted directly rather than inferred from a client's behavior. Every
//! assertion here reads raw bytes off a socket.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use fetchloom_engine as _;

use std::io::{Read, Write};
use std::net::TcpStream;

use fetchloom_faults::{IndexFormat, Reply, Script, TestServer};

fn object() -> Vec<u8> {
    (0..64u8).collect()
}

fn ask(server: &TestServer, headers: &str) -> Vec<u8> {
    let address = server.origin().replace("http://", "");
    let mut stream = TcpStream::connect(address).unwrap();
    let request =
        format!("GET /object HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{headers}\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut answer = Vec::new();
    let _ = stream.read_to_end(&mut answer);
    answer
}

fn split(answer: &[u8]) -> (String, Vec<u8>) {
    let position = answer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    (
        String::from_utf8_lossy(&answer[..position]).into_owned(),
        answer[position + 4..].to_vec(),
    )
}

#[test]
fn the_whole_object_is_served_with_its_length() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let (head, body) = split(&ask(&server, ""));

    assert!(head.starts_with("HTTP/1.1 200 OK"), "head was {head}");
    assert!(head.contains("Content-Length: 64"), "head was {head}");
    assert!(head.contains("Accept-Ranges: bytes"), "head was {head}");
    assert_eq!(body, object());
}

#[test]
fn a_range_is_served_as_partial_content_with_its_span() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let (head, body) = split(&ask(&server, "Range: bytes=32-\r\n"));

    assert!(
        head.starts_with("HTTP/1.1 206 Partial Content"),
        "head was {head}"
    );
    assert!(
        head.contains("Content-Range: bytes 32-63/64"),
        "head was {head}"
    );
    assert_eq!(body, object()[32..]);
}

#[test]
fn a_truncated_reply_stops_short_of_the_length_it_promised() {
    let server =
        TestServer::start(Script::serving(object()).replying(vec![Reply::Truncated { after: 10 }]))
            .unwrap();
    let (head, body) = split(&ask(&server, ""));

    assert!(head.contains("Content-Length: 64"), "head was {head}");
    assert_eq!(body.len(), 10, "the body was not cut short");
}

#[test]
fn a_flipped_reply_serves_one_wrong_byte_and_the_right_length() {
    let server =
        TestServer::start(Script::serving(object()).replying(vec![Reply::Flipped { offset: 7 }]))
            .unwrap();
    let (_, body) = split(&ask(&server, ""));

    assert_eq!(body.len(), 64);
    assert_eq!(body[7], !object()[7], "the byte was not flipped");
    assert_eq!(body[..7], object()[..7]);
    assert_eq!(body[8..], object()[8..]);
}

#[test]
fn a_lying_length_promises_more_than_it_sends() {
    let server = TestServer::start(
        Script::serving(object()).replying(vec![Reply::LyingLength { claimed: 4096 }]),
    )
    .unwrap();
    let (head, body) = split(&ask(&server, ""));

    assert!(head.contains("Content-Length: 4096"), "head was {head}");
    assert_eq!(body.len(), 64);
}

#[test]
fn an_ignored_range_answers_two_hundred_with_the_whole_object() {
    let server =
        TestServer::start(Script::serving(object()).replying(vec![Reply::RangeIgnored])).unwrap();
    let (head, body) = split(&ask(&server, "Range: bytes=32-\r\n"));

    assert!(head.starts_with("HTTP/1.1 200 OK"), "head was {head}");
    assert!(!head.contains("Content-Range"), "head was {head}");
    assert_eq!(body, object());
}

#[test]
fn a_wrong_range_serves_a_span_the_client_did_not_ask_for() {
    let server =
        TestServer::start(Script::serving(object()).replying(vec![Reply::WrongRange { shift: 8 }]))
            .unwrap();
    let (head, body) = split(&ask(&server, "Range: bytes=32-\r\n"));

    assert!(
        head.contains("Content-Range: bytes 40-63/64"),
        "head was {head}"
    );
    assert_eq!(body, object()[40..]);
}

#[test]
fn a_rate_limit_carries_the_wait_it_was_given() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
        code: 429,
        retry_after: Some("120".to_owned()),
    }]))
    .unwrap();
    let (head, _) = split(&ask(&server, ""));

    assert!(
        head.starts_with("HTTP/1.1 429 Too Many Requests"),
        "head was {head}"
    );
    assert!(head.contains("Retry-After: 120"), "head was {head}");
}

#[test]
fn a_rate_limit_without_a_wait_carries_no_retry_after() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
        code: 429,
        retry_after: None,
    }]))
    .unwrap();
    let (head, _) = split(&ask(&server, ""));

    assert!(!head.contains("Retry-After"), "head was {head}");
}

#[test]
fn every_status_the_suite_needs_is_served_with_its_reason() {
    for (code, reason) in [
        (403, "Forbidden"),
        (404, "Not Found"),
        (416, "Range Not Satisfiable"),
        (500, "Internal Server Error"),
        (503, "Service Unavailable"),
    ] {
        let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
            code,
            retry_after: None,
        }]))
        .unwrap();
        let (head, _) = split(&ask(&server, ""));
        assert!(
            head.starts_with(&format!("HTTP/1.1 {code} {reason}")),
            "head was {head}"
        );
    }
}

#[test]
fn a_redirect_names_where_it_points() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Redirect {
        code: 307,
        location: "http://elsewhere.invalid/object".to_owned(),
    }]))
    .unwrap();
    let (head, _) = split(&ask(&server, ""));

    assert!(
        head.starts_with("HTTP/1.1 307 Temporary Redirect"),
        "head was {head}"
    );
    assert!(
        head.contains("Location: http://elsewhere.invalid/object"),
        "head was {head}"
    );
}

#[test]
fn a_closed_body_ends_the_connection_part_way_through() {
    let server = TestServer::start(
        Script::serving(object()).replying(vec![Reply::ClosedMidBody { after: 20 }]),
    )
    .unwrap();
    let (head, body) = split(&ask(&server, ""));

    assert!(head.contains("Content-Length: 64"), "head was {head}");
    assert_eq!(body.len(), 20);
}

#[test]
fn the_entity_tag_changes_between_the_first_request_and_the_second() {
    let server = TestServer::start(
        Script::serving(object()).tagged(vec!["\"first\"".to_owned(), "\"second\"".to_owned()]),
    )
    .unwrap();

    let (first, _) = split(&ask(&server, ""));
    let (second, _) = split(&ask(&server, ""));

    assert!(first.contains("ETag: \"first\""), "head was {first}");
    assert!(second.contains("ETag: \"second\""), "head was {second}");
}

#[test]
fn a_script_runs_out_and_every_later_request_gets_the_same_answer() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
        code: 503,
        retry_after: None,
    }]))
    .unwrap();

    let (first, _) = split(&ask(&server, ""));
    let (second, _) = split(&ask(&server, ""));
    let (third, _) = split(&ask(&server, ""));

    assert!(first.starts_with("HTTP/1.1 503"), "head was {first}");
    assert!(second.starts_with("HTTP/1.1 200"), "head was {second}");
    assert!(third.starts_with("HTTP/1.1 200"), "head was {third}");
}

#[test]
fn a_server_that_withholds_range_support_says_so() {
    let server = TestServer::start(Script::serving(object()).ranges(false)).unwrap();
    let (head, _) = split(&ask(&server, ""));

    assert!(!head.contains("Accept-Ranges"), "head was {head}");
}

#[test]
fn every_index_format_is_served_and_the_unrecognized_one_matches_none_of_them() {
    let signatures = [
        (IndexFormat::ObjectStore, "<ListBucketResult"),
        (IndexFormat::WebDav, "multistatus"),
        (IndexFormat::GeneratedHtml, "<h1>Index of "),
    ];
    for (format, signature) in signatures {
        let server =
            TestServer::start(Script::serving(object()).replying(vec![Reply::Listing { format }]))
                .unwrap();
        let (_, body) = split(&ask(&server, ""));
        let body = String::from_utf8_lossy(&body).into_owned();
        assert!(body.contains(signature), "{format:?} served {body}");
    }

    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Listing {
        format: IndexFormat::Unrecognized,
    }]))
    .unwrap();
    let (_, body) = split(&ask(&server, ""));
    let body = String::from_utf8_lossy(&body).into_owned();
    for (_, signature) in signatures {
        assert!(
            !body.contains(signature),
            "the unrecognized index carried {signature}"
        );
    }
}

#[test]
fn every_request_is_recorded_with_its_headers() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let _ = ask(
        &server,
        "Range: bytes=16-\r\nAuthorization: Bearer secret\r\n",
    );

    let received = server.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].method, "GET");
    assert_eq!(received[0].target, "/object");
    assert_eq!(received[0].header("range"), Some("bytes=16-"));
    assert_eq!(received[0].header("authorization"), Some("Bearer secret"));
    assert_eq!(received[0].header("if-range"), None);
}

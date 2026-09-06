//! What the FTP adapter speaks, driven against a local FTP server.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use rustls as _;
use rustls_graviola as _;
use rustls_platform_verifier as _;
use serde_json as _;
use sha2 as _;
use ureq as _;

use std::io::Read;
use std::sync::Arc;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::seam::source::{ByteRange, Serves, Source, SourceIdentity};
use fetchloom_engine::work::WorkCounter;
use fetchloom_faults::{FtpScript, FtpTestServer};
use fetchloom_sources::FtpSource;

fn counter() -> Arc<WorkCounter> {
    Arc::new(WorkCounter::new())
}

fn source() -> FtpSource {
    FtpSource::new(Limits::default(), counter())
}

fn located(server: &FtpTestServer, path: &str) -> String {
    format!("{}/{path}", server.origin())
}

#[test]
fn a_plain_and_a_secured_scheme_are_served_and_nothing_else_is() {
    let ftp = source();
    assert_eq!(
        ftp.serves("ftp://host/pub/x.tar"),
        Some(Serves::Object),
        "a path naming a file is an object"
    );
    assert_eq!(
        ftp.serves("ftps://host/pub/"),
        Some(Serves::Container),
        "a path ending in a separator is a container"
    );
    assert_eq!(
        ftp.serves("https://host/x"),
        None,
        "the FTP adapter answered for a scheme it does not speak"
    );
    assert_eq!(ftp.serves("sftp://host/x"), None, "SFTP is not spoken");
}

#[test]
fn a_file_is_fetched_over_a_passive_data_connection_and_the_bytes_are_whole() {
    let bytes = b"the sequenced reads".to_vec();
    let server = FtpTestServer::start(FtpScript::serving("pub/reads.txt", bytes.clone())).unwrap();
    let taken = source()
        .fetch(&located(&server, "pub/reads.txt"), None, None)
        .expect("a file the server holds was not served");
    let mut read = Vec::new();
    let mut body = taken.body;
    body.read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes, "the data connection delivered other bytes");
    let spoken = server.spoken();
    assert!(
        spoken.iter().any(|line| line == "TYPE I"),
        "the transfer was not switched to binary, so a Windows server would translate line endings: {spoken:?}"
    );
    assert!(
        spoken.iter().any(|line| line == "PASV"),
        "the transfer did not ask for a passive data connection: {spoken:?}"
    );
}

#[test]
fn a_data_connection_the_server_points_at_another_host_is_refused_by_name() {
    let server = FtpTestServer::start(
        FtpScript::serving("pub/reads.txt", b"the sequenced reads".to_vec())
            .advertising_data_host([203, 0, 113, 7]),
    )
    .unwrap();
    let Err(refused) = source().fetch(&located(&server, "pub/reads.txt"), None, None) else {
        unreachable!(
            "a server that pointed the data connection at another host was followed, which is the \
             bounce attack"
        )
    };
    assert_eq!(refused.kind(), ErrorKind::NetworkRefused);
    let said = refused.next_action();
    assert!(
        said.contains("203.0.113.7"),
        "the refusal did not name the host the server pointed at: {said}"
    );
    assert!(
        said.contains("127.0.0.1"),
        "the refusal did not name the host the control connection is talking to: {said}"
    );
}

#[test]
fn a_resume_asks_for_the_offset_with_rest_before_the_transfer() {
    let bytes = b"0123456789abcdef".to_vec();
    let server = FtpTestServer::start(FtpScript::serving("pub/x.bin", bytes)).unwrap();
    let taken = source()
        .fetch(
            &located(&server, "pub/x.bin"),
            Some(ByteRange { start: 10, end: 16 }),
            None,
        )
        .expect("a ranged fetch was refused");
    let mut read = Vec::new();
    let mut body = taken.body;
    body.read_to_end(&mut read).unwrap();
    assert_eq!(
        read, b"abcdef",
        "the transfer did not restart at the offset asked for"
    );
    let spoken = server.spoken();
    assert!(
        spoken.iter().any(|line| line == "REST 10"),
        "the offset was not asked for with REST, which is the rung four resume: {spoken:?}"
    );
    assert!(
        spoken
            .iter()
            .position(|line| line == "REST 10")
            .zip(spoken.iter().position(|line| line.starts_with("RETR")))
            .is_some_and(|(rest, retr)| rest < retr),
        "REST was not sent before RETR, so the server would have served from zero: {spoken:?}"
    );
}

#[test]
fn a_server_that_does_not_implement_rest_reports_that_ranges_are_not_supported() {
    let server = FtpTestServer::start(
        FtpScript::serving("pub/x.bin", b"0123456789".to_vec()).refusing_rest(),
    )
    .unwrap();
    let ftp = source();
    let described = ftp
        .probe(&located(&server, "pub/x.bin"), None)
        .expect("a file the server holds was not probed");
    assert!(
        !described.supports_ranges,
        "a server whose FEAT does not offer REST was reported as resumable, so a partial would be \
         appended to from the wrong offset"
    );
}

#[test]
fn the_size_and_the_modification_time_the_server_states_are_carried() {
    let server =
        FtpTestServer::start(FtpScript::serving("pub/x.bin", b"0123456789".to_vec())).unwrap();
    let described = source()
        .probe(&located(&server, "pub/x.bin"), None)
        .expect("a file the server holds was not probed");
    assert_eq!(described.size, Some(10), "SIZE was not read");
    assert_eq!(
        described.identity,
        SourceIdentity::WeakValidator("20200102030405".to_owned()),
        "the modification time was not carried as the weak validator rung four stands on"
    );
}

#[test]
fn a_directory_is_listed_by_mlsd_and_every_member_is_under_it() {
    let server = FtpTestServer::start(
        FtpScript::empty()
            .holding("pub/one.txt", b"one".to_vec())
            .holding("pub/deep/two.txt", b"two".to_vec())
            .holding("pub/deep/deeper/three.txt", b"three".to_vec()),
    )
    .unwrap();
    let listed = source()
        .list(&located(&server, "pub/"), None)
        .expect("a directory the server holds was not listed");
    let paths: Vec<&str> = listed
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(
        paths,
        vec!["deep/deeper/three.txt", "deep/two.txt", "one.txt"],
        "a deep tree was not walked to its leaves"
    );
    assert_eq!(
        listed.entries[2].size,
        Some(3),
        "the size MLSD states was not carried"
    );
    let spoken = server.spoken();
    assert!(
        spoken.iter().any(|line| line.starts_with("MLSD")),
        "the machine-readable listing was not asked for: {spoken:?}"
    );
}

#[test]
fn a_server_that_refuses_mlsd_is_listed_by_list_and_says_it_fell_back() {
    let server = FtpTestServer::start(
        FtpScript::empty()
            .holding("pub/one.txt", b"one".to_vec())
            .holding("pub/deep/two.txt", b"two".to_vec())
            .refusing_mlsd(),
    )
    .unwrap();
    let ftp = source();
    let listed = ftp
        .list(&located(&server, "pub/"), None)
        .expect("a server that refuses MLSD was not listed by LIST");
    let paths: Vec<&str> = listed
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, vec!["deep/two.txt", "one.txt"]);
    assert_eq!(
        listed.entries[1].size,
        Some(3),
        "the size the ls line states was not read"
    );
    let fell_back = ftp.take_degradations();
    assert!(
        fell_back
            .iter()
            .any(|held| held.requested.contains("MLSD") && held.used.contains("LIST")),
        "a fallback from MLSD to LIST was silent, and nothing degrades silently here: {fell_back:?}"
    );
}

#[test]
fn a_listing_naming_a_path_outside_the_directory_is_refused() {
    for hostile in [
        "../escape.txt",
        "/etc/passwd",
        "a/../../b.txt",
        "..",
        "C:\\windows",
    ] {
        let server = FtpTestServer::start(
            FtpScript::empty().naming_in_listing("pub", vec![hostile.to_owned()]),
        )
        .unwrap();
        let refused = source()
            .list(&located(&server, "pub/"), None)
            .err()
            .unwrap_or_else(|| unreachable!("a listing naming {hostile} was accepted"));
        assert_eq!(
            refused.kind(),
            ErrorKind::ReferenceUnresolved,
            "a listing naming {hostile} failed as something other than an unresolved reference"
        );
    }
}

#[test]
fn a_secured_scheme_never_continues_in_the_clear_when_the_server_refuses_tls() {
    let server =
        FtpTestServer::start(FtpScript::serving("pub/x.bin", b"0123456789".to_vec())).unwrap();
    let secured = format!("{}/pub/x.bin", server.secured_origin());
    let Err(refused) = source().fetch(&secured, None, None) else {
        unreachable!("ftps:// continued over a control connection the server would not secure")
    };
    assert_eq!(refused.kind(), ErrorKind::NetworkTls);
    let spoken = server.spoken();
    assert!(
        !spoken.iter().any(|line| line.starts_with("PASS")),
        "a password was sent over a control connection that was never secured: {spoken:?}"
    );
}

#[test]
fn a_plain_scheme_offered_no_tls_says_it_fell_back_to_the_clear() {
    let server =
        FtpTestServer::start(FtpScript::serving("pub/x.bin", b"0123456789".to_vec())).unwrap();
    let ftp = source();
    ftp.probe(&located(&server, "pub/x.bin"), None)
        .expect("a plain control connection was refused");
    let fell_back = ftp.take_degradations();
    assert!(
        fell_back
            .iter()
            .any(|held| held.used.contains("clear") || held.used.contains("plain")),
        "a control connection that could not be secured said nothing about it: {fell_back:?}"
    );
}

#[test]
fn a_credential_is_never_sent_over_a_control_connection_that_is_not_secured() {
    use fetchloom_engine::credential::{Credential, CredentialOrigin, Secrets};
    use fetchloom_engine::redact::Secret;
    use fetchloom_engine::reference::Host;

    let server =
        FtpTestServer::start(FtpScript::serving("pub/x.bin", b"0123456789".to_vec())).unwrap();
    let credential = Credential {
        host: Host::new("127.0.0.1".to_owned()),
        origin: CredentialOrigin::Environment,
        secrets: Secrets::Bearer {
            value: Secret::new("hunter2".to_owned()),
        },
    };
    let Err(refused) = source().fetch(&located(&server, "pub/x.bin"), None, Some(&credential))
    else {
        unreachable!("a named credential was carried over a control connection in the clear")
    };
    assert_eq!(refused.kind(), ErrorKind::PolicyCredentialInvalid);
    let spoken = server.spoken();
    assert!(
        !spoken.iter().any(|line| line.contains("hunter2")),
        "the secret reached the wire: {spoken:?}"
    );
    assert!(
        !refused.next_action().contains("hunter2"),
        "the secret reached the refusal: {}",
        refused.next_action()
    );
}

#[test]
fn a_file_the_server_does_not_hold_is_an_unresolved_reference() {
    let server =
        FtpTestServer::start(FtpScript::serving("pub/x.bin", b"0123456789".to_vec())).unwrap();
    let refused = source()
        .probe(&located(&server, "pub/absent.bin"), None)
        .expect_err("a file the server does not hold was probed successfully");
    assert_eq!(refused.kind(), ErrorKind::ReferenceUnresolved);
}

#[test]
fn a_file_larger_than_four_gigabytes_is_resumed_at_an_offset_above_a_thirty_two_bit_count() {
    let start = 5_000_000_000_u64;
    let server = FtpTestServer::start(
        FtpScript::serving("pub/huge.bin", b"tail".to_vec())
            .stating_size("pub/huge.bin", 5_000_000_004),
    )
    .unwrap();
    let described = source()
        .probe(&located(&server, "pub/huge.bin"), None)
        .expect("a large file was not probed");
    assert_eq!(described.size, Some(5_000_000_004));
    let _ = source().fetch(
        &located(&server, "pub/huge.bin"),
        Some(ByteRange {
            start,
            end: 5_000_000_004,
        }),
        None,
    );
    let spoken = server.spoken();
    assert!(
        spoken.iter().any(|line| *line == format!("REST {start}")),
        "an offset above what thirty two bits hold was truncated on the wire: {spoken:?}"
    );
}

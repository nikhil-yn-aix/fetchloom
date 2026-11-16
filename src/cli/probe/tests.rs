use super::*;

fn url() -> Url {
    Url::parse("https://data.example.org/iris.csv").unwrap()
}

/// the outcome `judged` gives, as `--json` names it, with the size a changed file has now.
fn outcome(judged: Result<(), Lack>) -> (&'static str, Option<u64>) {
    match judged {
        Ok(()) => ("served", None),
        Err(Lack::Changed { size, host }) => {
            assert_eq!(host, "data.example.org");
            ("changed", Some(size))
        }
        Err(Lack::Gone(_)) => ("gone", None),
        Err(Lack::Refused(_)) => ("refused", None),
        Err(Lack::Unsized(_)) => ("unsized", None),
        Err(Lack::Unknown(_)) => ("unknown", None),
        Err(Lack::Imported) => ("imported", None),
    }
}

fn answered(status: u16, kind: Kind) -> Probed {
    Probed::Answered {
        status,
        error: Error::new(kind, format!("data.example.org answered {status}")),
    }
}

/// each answer a source can give to a request for a file's first byte, pinned at 10 bytes, and
/// what it says of the file. a page is the file only where one may be.
#[test]
fn an_answer_is_served_only_at_the_pinned_size() {
    let file = |total, page| Probed::File {
        total,
        page,
        slashed: false,
        name: None,
        head: Vec::new(),
        modified: None,
    };
    let cases = [
        (file(Some(10), false), false, ("served", None)),
        (file(Some(12), false), false, ("changed", Some(12))),
        (file(Some(0), false), false, ("changed", Some(0))),
        (file(None, false), false, ("unsized", None)),
        (file(Some(10), true), false, ("unknown", None)),
        (file(Some(10), true), true, ("served", None)),
        (file(Some(12), true), true, ("changed", Some(12))),
        (
            answered(404, Kind::ReferenceUnresolved),
            false,
            ("gone", None),
        ),
        (
            answered(410, Kind::ReferenceUnresolved),
            false,
            ("gone", None),
        ),
        (answered(401, Kind::AuthRequired), false, ("refused", None)),
        (answered(403, Kind::AuthRequired), false, ("refused", None)),
        // a sign-in page sent with 200, and drive's own words for a file it no longer has
        (answered(200, Kind::AuthRequired), false, ("refused", None)),
        (
            answered(400, Kind::ReferenceUnresolved),
            false,
            ("gone", None),
        ),
        (answered(500, Kind::NetworkStatus), false, ("unknown", None)),
        (answered(418, Kind::NetworkStatus), false, ("unknown", None)),
        (
            Probed::Unanswered(Error::new(
                Kind::NetworkUnreachable,
                "gave up on data.example.org after 5 tries",
            )),
            false,
            ("unknown", None),
        ),
    ];
    for (probed, takes_page, expected) in cases {
        let said = format!("{probed:?}");
        assert_eq!(
            outcome(judged(probed, 10, takes_page, &url())),
            expected,
            "{said}"
        );
    }
}

/// a refusal keeps the sign-in hint a download gives, and a gone file the error naming its link.
#[test]
fn a_refusal_keeps_the_hint_a_download_gives() {
    let refused = Probed::Answered {
        status: 403,
        error: Error::new(Kind::AuthRequired, "data.example.org refused access (403)")
            .help("if it needs an account, sign in with `fetchloom auth login data.example.org`"),
    };
    let Err(Lack::Refused(error)) = judged(refused, 10, false, &url()) else {
        panic!("a 403 is a refusal");
    };
    assert_eq!(
        error.help.as_deref(),
        Some("if it needs an account, sign in with `fetchloom auth login data.example.org`")
    );
}

fn pinned(file: &LockedFile) -> Pinned<'_> {
    Pinned::locked("notes", file, false)
}

/// a file without a link was imported, and a link data.lock broke names nothing, so neither is
/// asked of a source.
#[test]
fn an_import_and_a_broken_link_ask_no_source() {
    let mut file = LockedFile {
        path: "notes.csv".to_owned(),
        size: 10,
        sha256: crate::digest::sha256(b"notes"),
        md5: None,
        url: String::new(),
        unpacks_to: None,
    };
    assert!(matches!(pinned(&file).request(), Err(Lack::Imported)));
    file.url = "not a link".to_owned();
    assert!(matches!(pinned(&file).request(), Err(Lack::Unknown(_))));
    file.url = "https://data.example.org/notes.csv".to_owned();
    let fetch = pinned(&file).request().unwrap();
    assert_eq!(fetch.expect.size, Some(10));
    assert_eq!(fetch.expect.sha256, Some(file.sha256));
}

/// a changed size reads exact to the byte when both sizes round alike.
#[test]
fn a_changed_size_reads_exact_when_rounding_hides_it() {
    assert_eq!(sized(1_048_577, 1_048_576), "1,048,577 bytes");
    assert_eq!(sized(12, 10), "12 B");
    assert_eq!(sized(2 << 20, 1 << 20), "2.0 MiB");
}

use super::*;

fn cookie(name: &str, domain: &str, expires: Option<i64>) -> Cookie {
    Cookie {
        name: name.to_owned(),
        value: format!("{name}-secret"),
        domain: domain.to_owned(),
        path: "/".to_owned(),
        secure: false,
        expires,
    }
}

#[test]
fn a_cookies_file_reads_as_curl_reads_it() {
    let text = "# Netscape HTTP Cookie File\n\
        # https://curl.se/docs/http-cookies.html\n\
        \n\
        .example.org\tTRUE\t/\tTRUE\t2000000000\tsession\tabc def\n\
        #HttpOnly_data.example.org\tFALSE\t/files\tFALSE\t0\tcsrf\tx=y\r\n\
        example.org\tTRUE\t/\tFALSE\t1900000000\tprefs\t\n\
        data.example.org\tFALSE\tno-slash\tFALSE\t1900000000\tempty\n\
        not a cookie line\n\
        .example.org\tTRUE\t/\tFALSE\tsoon\tbad\tx\n";
    let (cookies, skipped) = netscape(text);
    assert_eq!(skipped, 2);
    let expected = [
        (
            ".example.org",
            "/",
            true,
            Some(2_000_000_000),
            "session",
            "abc def",
        ),
        ("data.example.org", "/files", false, None, "csrf", "x=y"),
        (".example.org", "/", false, Some(1_900_000_000), "prefs", ""),
        (
            "data.example.org",
            "/",
            false,
            Some(1_900_000_000),
            "empty",
            "",
        ),
    ];
    let read: Vec<_> = cookies
        .iter()
        .map(|c| {
            (
                c.domain.as_str(),
                c.path.as_str(),
                c.secure,
                c.expires,
                c.name.as_str(),
                c.value.as_str(),
            )
        })
        .collect();
    assert_eq!(read, expected);
}

#[test]
fn a_domain_covers_its_own_host_and_those_below_a_leading_dot() {
    for (domain, host, covered) in [
        (".example.org", "data.example.org", true),
        (".example.org", "example.org", true),
        (".Example.ORG", "a.b.example.org:8443", true),
        (".example.org", "badexample.org", false),
        (".example.org", "example.org.evil", false),
        ("data.example.org", "data.example.org", true),
        ("data.example.org", "sub.data.example.org", false),
        ("data.example.org", "example.org", false),
        ("127.0.0.1", "127.0.0.1:8080", true),
        (".0.0.1", "127.0.0.1:8080", false),
        ("[::1]", "[::1]:8080", true),
    ] {
        assert_eq!(covers(domain, host), covered, "{domain} for {host}");
    }
}

#[test]
fn a_host_takes_the_unexpired_sendable_cookies_its_domain_covers() {
    let now = moment(1_950_000_000);
    let mut broken = cookie("broken", "data.example.org", None);
    broken.value = "a;b".to_owned();
    let mut first = cookie("dup", ".example.org", None);
    first.value = "first".to_owned();
    let mut last = cookie("dup", ".EXAMPLE.org", None);
    last.value = "last".to_owned();
    let found = vec![
        cookie("session", ".example.org", Some(2_000_000_000)),
        first,
        cookie("sso", "sso.example.org", None),
        cookie("old", "data.example.org", Some(1_900_000_000)),
        broken,
        cookie("own", "data.example.org", None),
        last,
        cookie("ad", ".tracker.example", None),
    ];
    let picked = pick(found, "data.example.org", now);
    let kept: Vec<(&str, &str)> = picked
        .kept
        .iter()
        .map(|c| (c.name.as_str(), c.value.as_str()))
        .collect();
    assert_eq!(
        kept,
        [
            ("session", "session-secret"),
            ("own", "own-secret"),
            ("dup", "last")
        ]
    );
    let left: Vec<(&str, &str, Why)> = picked
        .left
        .iter()
        .map(|l| (l.name.as_str(), l.domain.as_str(), l.why))
        .collect();
    assert_eq!(
        left,
        [
            ("sso", "sso.example.org", Why::Elsewhere),
            ("old", "data.example.org", Why::Expired),
            ("broken", "data.example.org", Why::Unsendable),
            ("ad", ".tracker.example", Why::Elsewhere),
        ]
    );
}

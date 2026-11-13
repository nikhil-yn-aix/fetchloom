use std::ffi::OsString;
use std::path::Path;

use super::*;

/// a machine whose home is `home`, with no credential store, so keys go in fetchloom's file.
fn machine(home: &Path) -> impl Fn(&str) -> Option<OsString> + '_ {
    move |name| match name {
        "HOME" | "USERPROFILE" | "APPDATA" => Some(home.as_os_str().to_owned()),
        "XDG_CONFIG_HOME" => Some(home.join(".config").into_os_string()),
        "FETCHLOOM_KEYRING" => Some("0".into()),
        _ => None,
    }
}

fn cookie(name: &str, domain: &str, expires: Option<i64>) -> Cookie {
    Cookie {
        name: name.to_owned(),
        value: format!("{name}-secret"),
        domain: domain.to_owned(),
        path: "/".to_owned(),
        secure: true,
        expires,
    }
}

#[test]
fn a_site_is_a_host_with_its_port_and_nothing_more() {
    for (typed, expected) in [
        ("Data.Example.org", "data.example.org"),
        ("data.example.org:443", "data.example.org"),
        ("data.example.org:8443", "data.example.org:8443"),
        ("127.0.0.1:8080", "127.0.0.1:8080"),
        ("localhost:8000", "localhost:8000"),
        ("[::1]:8080", "[::1]:8080"),
    ] {
        assert_eq!(host(typed).as_deref(), Some(expected), "{typed}");
    }
    for typed in [
        "https://data.example.org",
        "data.example.org/files",
        "ada@data.example.org",
        "ada:pw@data.example.org",
        "data.example.org?token=1",
        "data example.org",
        "data.example.org\nevil.example",
        "$(id).example.org",
        "",
    ] {
        assert_eq!(host(typed), None, "{typed:?}");
    }
}

#[test]
fn a_header_that_frames_a_request_or_its_connection_carries_no_key() {
    for typed in [
        "X-API-Key",
        "authorization",
        "X-Auth-Token",
        "PRIVATE-TOKEN",
    ] {
        assert!(header_name(typed).is_ok(), "{typed}");
    }
    for typed in [
        "Host",
        "Content-Length",
        "Transfer-Encoding",
        "Connection",
        "Keep-Alive",
        "TE",
        "Upgrade",
        "Proxy-Authorization",
        "Range",
        "If-None-Match",
        "Accept-Encoding",
    ] {
        let error = header_name(typed).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{typed}");
        assert!(error.message.contains(typed), "{}", error.message);
    }
    let cookie = header_name("Cookie").unwrap_err();
    assert!(
        cookie
            .help
            .as_deref()
            .is_some_and(|help| help.contains("--cookies")),
        "{cookie:?}"
    );
    for typed in ["X API", "X:Key", "", "Key\nX-Other"] {
        let error = header_name(typed).unwrap_err();
        assert_eq!(
            error.message,
            format!("\"{}\" is not a header name", typed.escape_debug())
        );
    }
}

#[test]
fn a_saved_credential_round_trips_and_goes_to_its_host_alone() {
    let home = tempfile::tempdir().unwrap();
    let var = machine(home.path());
    let token = Saved::Token {
        token: "t-secret".to_owned(),
    };
    let header = Saved::Header {
        name: "X-API-Key".to_owned(),
        value: "k-secret".to_owned(),
    };
    let cookies = Saved::Cookies {
        from: Taken::Browser("firefox".to_owned()),
        cookies: vec![cookie("session", "127.0.0.1", Some(2_000_000_000))],
    };
    for (host, saved) in [
        ("data.example.org", &token),
        ("api.example.org:8443", &header),
        ("127.0.0.1:8080", &cookies),
    ] {
        let origin = save(&var, host, saved, store::Place::File).unwrap();
        assert!(matches!(origin, Origin::File(_)), "{origin:?}");
    }
    assert_eq!(
        hosts(&var),
        ["127.0.0.1:8080", "api.example.org:8443", "data.example.org"]
    );
    assert_eq!(find(&var, "data.example.org").unwrap().0, token);
    assert_eq!(find(&var, "api.example.org:8443").unwrap().0, header);
    assert_eq!(find(&var, "api.example.org"), None);
    let index =
        std::fs::read_to_string(store::folder(&var).unwrap().join("accounts.json")).unwrap();
    for secret in ["t-secret", "k-secret", "session-secret"] {
        assert!(
            !index.contains(secret),
            "the index holds no secret: {index}"
        );
    }
    let net = crate::net::Net::new(
        crate::net::Options {
            offline: crate::net::Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: crate::net::Splitting::Never,
        },
        std::sync::Arc::default(),
        std::sync::Arc::default(),
        std::sync::Arc::new(|_| {}),
    );
    let found = sites(&var);
    let net = net.finding_sites(move || found);
    assert_eq!(
        net.site_hosts(),
        ["127.0.0.1:8080", "api.example.org:8443", "data.example.org"]
    );
    assert!(forget(&var, "data.example.org").unwrap().is_some());
    assert!(forget(&var, "data.example.org").unwrap().is_none());
    assert_eq!(hosts(&var), ["127.0.0.1:8080", "api.example.org:8443"]);
}

#[test]
fn a_refusal_names_the_command_that_saves_the_same_kind_again() {
    let file = Saved::Cookies {
        from: Taken::File(PathBuf::from("/home/ada/my cookies.txt")),
        cookies: vec![
            cookie("a", ".example.org", Some(1_900_000_000)),
            cookie("b", ".example.org", None),
            cookie("c", ".example.org", Some(1_800_000_000)),
        ],
    };
    let browser = Saved::Cookies {
        from: Taken::Browser("firefox:work".to_owned()),
        cookies: Vec::new(),
    };
    let header = Saved::Header {
        name: "X-API-Key".to_owned(),
        value: String::new(),
    };
    let token = Saved::Token {
        token: String::new(),
    };
    assert_eq!(
        token.again("data.example.org"),
        "save a current one with `fetchloom auth login data.example.org --token`"
    );
    assert_eq!(
        header.command("api.example.org:8443"),
        "fetchloom auth login api.example.org:8443 --header X-API-Key"
    );
    assert_eq!(
        browser.again("data.example.org:8443"),
        "sign in to data.example.org in firefox again, then run `fetchloom auth login data.example.org:8443 --browser firefox:work`"
    );
    if cfg!(unix) {
        assert_eq!(
            file.command("data.example.org"),
            "fetchloom auth login data.example.org --cookies \"/home/ada/my cookies.txt\""
        );
    }
    assert_eq!(file.soonest(), Some(1_800_000_000));
    assert_eq!(token.soonest(), None);
    assert_eq!(
        [token.kind(), header.kind(), file.kind()],
        ["token", "header", "cookies"]
    );
}

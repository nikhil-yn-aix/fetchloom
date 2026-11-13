//! any website: its own credential, saved for its host with `auth login <host>`, a token, a header
//! or cookies, sent to that host alone and named when the host refuses it; and a web page a
//! person asks for as the data.
#![cfg(debug_assertions)]

use crate::auth::with_stdin;
use crate::support::{Result, Routes, Sandbox, file, json, reply, text};

/// the host and port of a server on this machine, as `auth login` takes it.
fn host_of(server: &Routes) -> String {
    server.url.trim_start_matches("http://").to_owned()
}

/// signs in to `host` with `token`, as a script does.
fn sign_in_with_token(sandbox: &Sandbox, host: &str, token: &str) -> Result {
    let login = with_stdin(
        sandbox.command(&["auth", "login", host, "--token", "--with-token"]),
        &format!("{token}\n"),
    )?;
    let stderr = text(&login.stderr)?;
    assert!(login.status.success(), "{stderr}");
    assert!(!stderr.contains(token), "{stderr}");
    Ok(())
}

/// the value of `name` in each request `server` took, lowercased.
fn heard(server: &Routes, name: &str) -> Vec<Option<String>> {
    server
        .requests()
        .iter()
        .map(|head| {
            head.lines().find_map(|line| {
                let (field, value) = line.split_once(':')?;
                (field == name).then(|| value.trim().to_owned())
            })
        })
        .collect()
}

#[test]
fn a_token_goes_to_its_own_host_and_never_after_a_redirect_leaves_it() -> Result {
    let sandbox = Sandbox::new()?;
    let other = Routes::start(Vec::new())?;
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    site.change(vec![
        (
            "/away".to_owned(),
            reply(
                "302 Found",
                &[
                    ("Location", &format!("{}/hop", other.url)),
                    ("Content-Length", "0"),
                ],
                b"",
            ),
        ),
        (
            "/same".to_owned(),
            reply(
                "302 Found",
                &[("Location", "/data.csv"), ("Content-Length", "0")],
                b"",
            ),
        ),
    ]);
    other.change(vec![(
        "/hop".to_owned(),
        reply(
            "302 Found",
            &[
                ("Location", &format!("{}/data.csv", site.url)),
                ("Content-Length", "0"),
            ],
            b"",
        ),
    )]);
    sandbox.allow(&other.url);
    sign_in_with_token(&sandbox, &host_of(&site), "t-secret-1")?;
    for (path, dir) in [("same", "one"), ("away", "two")] {
        let output = sandbox.run(&["get", &format!("{}/{path}", site.url), "--dir", dir])?;
        let shown = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
        assert!(output.status.success(), "{shown}");
        assert!(!shown.contains("t-secret-1"), "{shown}");
    }
    let bearer = Some("bearer t-secret-1".to_owned());
    assert_eq!(
        heard(&site, "authorization"),
        [bearer.clone(), bearer.clone(), bearer, None],
        "{:?}",
        site.targets()
    );
    assert_eq!(heard(&other, "authorization"), [None]);
    Ok(())
}

#[test]
fn a_refused_token_is_named_with_the_command_that_saves_a_new_one() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![(
        "/data.csv".to_owned(),
        reply("401 Unauthorized", &[("Content-Length", "0")], b""),
    )])?;
    let host = host_of(&site);
    sign_in_with_token(&sandbox, &host, "t-old")?;
    let link = format!("{}/data.csv", site.url);
    let output = sandbox.run(&["--json", "get", &link])?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "auth.required");
    assert_eq!(
        error["message"],
        format!("{host} refused the token saved for it (401); it may have expired")
    );
    assert_eq!(
        error["help"],
        format!(
            "save a current one with `fetchloom auth login {host} --token`, then run `fetchloom --json get {link}` again"
        )
    );
    assert_eq!(error["fix"], serde_json::Value::Null);
    Ok(())
}

#[test]
fn auth_lists_a_websites_token_and_logout_removes_it() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    sign_in_with_token(&sandbox, &host, "t-listed")?;
    let status = sandbox.run(&["auth", "--json", "--offline"])?;
    assert!(status.status.success(), "{}", text(&status.stderr)?);
    assert!(!text(&status.stdout)?.contains("t-listed"));
    let document = json(&status)?;
    let row = document["result"]["accounts"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["account"] == format!("site:{host}"))
        })
        .ok_or("no row for the site")?;
    assert_eq!(row["signed_in"], true);
    assert_eq!(row["kind"], "token");
    assert_eq!(row["expires"], serde_json::Value::Null);
    assert!(
        row["from"]
            .as_str()
            .is_some_and(|from| from.ends_with("credentials.json")),
        "{row}"
    );
    let lines = text(&sandbox.run(&["auth", "--offline"])?.stderr)?;
    assert!(
        lines.contains(&format!("Signed in to {host} with a token ·")),
        "{lines}"
    );
    let logout = sandbox.run(&["auth", "logout", &host, "--json"])?;
    assert!(logout.status.success(), "{}", text(&logout.stderr)?);
    let removed = json(&logout)?;
    assert_eq!(removed["result"]["account"], format!("site:{host}"));
    let output = sandbox.run(&["get", &format!("{}/data.csv", site.url)])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(heard(&site, "authorization"), [None]);
    let again = sandbox.run(&["auth", "logout", &host])?;
    assert!(again.status.success());
    assert!(
        text(&again.stderr)?.contains(&format!("nothing was saved for {host}")),
        "{}",
        text(&again.stderr)?
    );
    Ok(())
}

#[test]
fn a_header_goes_to_its_own_host_by_the_name_the_site_reads() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![("/v1/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    for (name, said) in [
        (
            "Host",
            "--header does not take Host, which fetchloom or HTTP itself sets on each request",
        ),
        (
            "Transfer-Encoding",
            "--header does not take Transfer-Encoding, which fetchloom or HTTP itself sets on each request",
        ),
        ("Cookie", "--header does not take Cookie"),
        ("X API", "\"X API\" is not a header name"),
    ] {
        let refused = with_stdin(
            sandbox.command(&[
                "--json",
                "auth",
                "login",
                &host,
                "--header",
                name,
                "--with-token",
            ]),
            "k-1\n",
        )?;
        assert_eq!(refused.status.code(), Some(2), "{name}");
        assert_eq!(json(&refused)?["errors"][0]["message"], said);
    }
    let login = with_stdin(
        sandbox.command(&[
            "auth",
            "login",
            &host,
            "--header",
            "X-API-Key",
            "--with-token",
        ]),
        "k-1\n",
    )?;
    assert!(login.status.success(), "{}", text(&login.stderr)?);
    let output = sandbox.run(&["get", &format!("{}/v1/data.csv", site.url)])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(heard(&site, "x-api-key"), [Some("k-1".to_owned())]);
    assert_eq!(heard(&site, "authorization"), [None]);
    let lines = text(&sandbox.run(&["auth", "--offline"])?.stderr)?;
    assert!(
        lines.contains(&format!("Signed in to {host} with the X-API-Key header ·")),
        "{lines}"
    );
    Ok(())
}

/// a cookies.txt line, as a browser extension exports it.
fn line(domain: &str, path: &str, secure: bool, expires: i64, name: &str, value: &str) -> String {
    let below = if domain.starts_with('.') {
        "TRUE"
    } else {
        "FALSE"
    };
    let secure = if secure { "TRUE" } else { "FALSE" };
    format!("{domain}\t{below}\t{path}\t{secure}\t{expires}\t{name}\t{value}\n")
}

#[test]
fn cookies_from_a_file_go_to_their_host_by_path_and_secure_flag() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![
        ("/data/file.csv".to_owned(), file(b"a,b\n", "\"v1\"")),
        ("/other/x.csv".to_owned(), file(b"c,d\n", "\"v2\"")),
    ])?;
    let host = host_of(&site);
    let later = 4_000_000_000;
    let cookies = [
        "# Netscape HTTP Cookie File\n".to_owned(),
        line("127.0.0.1", "/data", false, later, "session", "s-value-1"),
        line("127.0.0.1", "/", false, 0, "all", "a-value-1"),
        line("127.0.0.1", "/", true, later, "secure", "s-value-2"),
        line("127.0.0.1", "/", false, 1_000_000_000, "old", "o-value"),
        line("sso.example.org", "/", true, later, "sid", "x-value"),
    ]
    .concat();
    std::fs::write(sandbox.work().join("cookies.txt"), cookies)?;
    let login = sandbox.run(&["auth", "login", &host, "--cookies", "cookies.txt", "--yes"])?;
    let stderr = text(&login.stderr)?;
    assert!(login.status.success(), "{stderr}");
    for told in [
        format!("Found 3 cookies for {host} in cookies.txt\n"),
        "session · 127.0.0.1 · expires 2096-10-02\n".to_owned(),
        "all · 127.0.0.1 · ends with the browser's session\n".to_owned(),
        "Left out 1 cookie for other hosts: sid of sso.example.org\n".to_owned(),
        "Left out 1 cookie that expired: old of 127.0.0.1\n".to_owned(),
    ] {
        assert!(stderr.contains(&told), "{told}: {stderr}");
    }
    for value in ["s-value-1", "a-value-1", "s-value-2", "o-value", "x-value"] {
        assert!(!stderr.contains(value), "{stderr}");
    }
    for path in ["data/file.csv", "other/x.csv"] {
        let output = sandbox.run(&["get", &format!("{}/{path}", site.url)])?;
        assert!(output.status.success(), "{}", text(&output.stderr)?);
    }
    assert_eq!(
        heard(&site, "cookie"),
        [
            Some("session=s-value-1; all=a-value-1".to_owned()),
            Some("all=a-value-1".to_owned())
        ]
    );
    let lines = text(&sandbox.run(&["auth", "--offline"])?.stderr)?;
    assert!(
        lines.contains(&format!(
            "Signed in to {host} with 3 cookies from a cookies file, the soonest expiring 2096-10-02 ·"
        )),
        "{lines}"
    );
    Ok(())
}

/// a website's cookies saved in the sandbox's key file as `auth login` writes them, with
/// `expires` for each.
fn saved_cookies(sandbox: &Sandbox, host: &str, expires: i64) -> Result {
    let home = sandbox.work().parent().ok_or("no sandbox")?.join("home");
    let folder = if cfg!(windows) {
        home.join("fetchloom")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/fetchloom")
    } else {
        home.join(".config/fetchloom")
    };
    std::fs::create_dir_all(&folder)?;
    let id = format!("site:{host}");
    let saved = serde_json::json!({
        "kind": "cookies",
        "from": {"browser": "firefox"},
        "cookies": [{
            "name": "session", "value": "s-old", "domain": "127.0.0.1", "path": "/",
            "secure": false, "expires": expires,
        }],
    });
    std::fs::write(
        folder.join("accounts.json"),
        serde_json::json!({ &id: "file" }).to_string(),
    )?;
    std::fs::write(
        folder.join("credentials.json"),
        serde_json::json!({ &id: saved.to_string() }).to_string(),
    )?;
    Ok(())
}

#[test]
fn expired_cookies_are_named_with_the_command_that_takes_them_again() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![(
        "/data.csv".to_owned(),
        reply("403 Forbidden", &[("Content-Length", "0")], b""),
    )])?;
    let host = host_of(&site);
    saved_cookies(&sandbox, &host, 1_000_000_000)?;
    let again = format!(
        "sign in to 127.0.0.1 in firefox again, then run `fetchloom auth login {host} --browser firefox`"
    );
    let lines = text(&sandbox.run(&["auth", "--offline"])?.stderr)?;
    assert!(
        lines.contains(&format!(
            "Expired 1 cookie from firefox for {host}, the soonest expiring 2001-09-09; {again}\n"
        )),
        "{lines}"
    );
    let link = format!("{}/data.csv", site.url);
    let output = sandbox.run(&["--json", "get", &link])?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        format!("{host} refused access to {link} (403), and the cookies saved for it have expired")
    );
    assert_eq!(
        error["help"],
        format!("{again}, then run `fetchloom --json get {link}` again")
    );
    assert_eq!(heard(&site, "cookie"), [None]);
    Ok(())
}

/// a firefox profile where firefox keeps one on this system, in the sandbox's home, holding
/// `rows` of name, host, expiry in milliseconds and origin attributes, as schema 17 does.
fn firefox_profile(sandbox: &Sandbox, rows: &[(&str, &str, i64, &str)]) -> Result {
    let home = sandbox.work().parent().ok_or("no sandbox")?.join("home");
    let root = if cfg!(windows) {
        home.join("Mozilla/Firefox")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Firefox")
    } else {
        home.join(".mozilla/firefox")
    };
    let folder = root.join("Profiles/q1w2e3r4.default-release");
    std::fs::create_dir_all(&folder)?;
    let db = rusqlite::Connection::open(folder.join("cookies.sqlite"))?;
    db.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA user_version = 17; \
         CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL DEFAULT '', \
         name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, lastAccessed INTEGER, \
         creationTime INTEGER, isSecure INTEGER, isHttpOnly INTEGER, inBrowserElement INTEGER DEFAULT 0, \
         sameSite INTEGER DEFAULT 0, schemeMap INTEGER DEFAULT 0, isPartitionedAttributeSet INTEGER DEFAULT 0, \
         updateTime INTEGER, CONSTRAINT moz_uniqueid UNIQUE (name, host, path, originAttributes))",
    )?;
    for (name, host, expiry, attributes) in rows {
        db.execute(
            "INSERT INTO moz_cookies (originAttributes, name, value, host, path, expiry, isSecure) \
             VALUES (?1, ?2, ?3, ?4, '/', ?5, 0)",
            rusqlite::params![attributes, name, format!("{name}-value"), host, expiry],
        )?;
    }
    Ok(())
}

#[test]
fn cookies_from_firefox_are_shown_and_saved_only_with_a_yes() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    let later = 4_000_000_000_000;
    firefox_profile(
        &sandbox,
        &[
            ("session", "127.0.0.1", later, ""),
            ("work", "127.0.0.1", later, "^userContextId=1"),
            ("sid", "sso.example.org", later, ""),
        ],
    )?;
    let asked = sandbox.run(&["auth", "login", &host, "--browser", "firefox"])?;
    let stderr = text(&asked.stderr)?;
    assert_eq!(asked.status.code(), Some(1), "{stderr}");
    let profile = std::path::Path::new("~").join(if cfg!(windows) {
        "Mozilla/Firefox/Profiles/q1w2e3r4.default-release"
    } else if cfg!(target_os = "macos") {
        "Library/Application Support/Firefox/Profiles/q1w2e3r4.default-release"
    } else {
        ".mozilla/firefox/Profiles/q1w2e3r4.default-release"
    });
    for told in [
        format!(
            "Found 1 cookie for {host} in firefox's profile {}\n",
            profile.display()
        ),
        "session · 127.0.0.1 · expires 2096-10-02\n".to_owned(),
        "Left out 1 cookie the browser keeps apart, for a container, a private window or another site's frames: work of 127.0.0.1\n".to_owned(),
        "help: run it again with --yes".to_owned(),
    ] {
        assert!(stderr.contains(&told), "{told}: {stderr}");
    }
    assert!(
        !stderr.contains("sid"),
        "another site's cookie is not named: {stderr}"
    );
    assert!(!stderr.contains("-value"), "{stderr}");
    let before = json(&sandbox.run(&["auth", "--json", "--offline"])?)?;
    assert!(
        !before.to_string().contains("site:"),
        "a no saves nothing: {before}"
    );
    let saved = sandbox.run(&["auth", "login", &host, "--browser", "firefox", "--yes"])?;
    assert!(saved.status.success(), "{}", text(&saved.stderr)?);
    let output = sandbox.run(&["get", &format!("{}/data.csv", site.url)])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(
        heard(&site, "cookie"),
        [Some("session=session-value".to_owned())]
    );
    let lines = text(&sandbox.run(&["auth", "--offline"])?.stderr)?;
    assert!(
        lines.contains(&format!(
            "Signed in to {host} with 1 cookie from firefox, the soonest expiring 2096-10-02 ·"
        )),
        "{lines}"
    );
    Ok(())
}

/// a chrome profile in the sandbox's home, as chrome keeps one on linux, holding a cookie for
/// `domain` named `name`, sealed as chrome seals it there without a keyring: under `v10`, with
/// chromium's fixed key, after the SHA-256 of its domain.
#[cfg(target_os = "linux")]
fn chrome_profile(sandbox: &Sandbox, domain: &str, name: &str, value: &str) -> Result {
    use aes::cipher::block_padding::Pkcs7;
    use aes::cipher::{BlockModeEncrypt as _, KeyIvInit as _};
    use sha2::Digest as _;
    // PBKDF2-HMAC-SHA1 of "peanuts" over "saltysalt", once, as chromium's posix_key_provider.cc
    // gives it
    let key: [u8; 16] = [
        0xfd, 0x62, 0x1f, 0xe5, 0xa2, 0xb4, 0x02, 0x53, 0x9d, 0xfa, 0x14, 0x7c, 0xa9, 0x27, 0x27,
        0x78,
    ];
    let plain = [
        sha2::Sha256::digest(domain.as_bytes()).as_slice(),
        value.as_bytes(),
    ]
    .concat();
    let sealed = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &[b' '; 16].into())
        .encrypt_padded_vec::<Pkcs7>(&plain);
    let folder = sandbox
        .work()
        .parent()
        .ok_or("no sandbox")?
        .join("home/.config/google-chrome/Default/Network");
    std::fs::create_dir_all(&folder)?;
    let db = rusqlite::Connection::open(folder.join("Cookies"))?;
    db.execute_batch(
        "CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR); \
         INSERT INTO meta VALUES ('version', '24'); \
         CREATE TABLE cookies(creation_utc INTEGER NOT NULL,host_key TEXT NOT NULL,top_frame_site_key TEXT NOT NULL,\
         name TEXT NOT NULL,value TEXT NOT NULL,encrypted_value BLOB NOT NULL,path TEXT NOT NULL,\
         expires_utc INTEGER NOT NULL,is_secure INTEGER NOT NULL,is_httponly INTEGER NOT NULL,\
         last_access_utc INTEGER NOT NULL,has_expires INTEGER NOT NULL,is_persistent INTEGER NOT NULL,\
         priority INTEGER NOT NULL,samesite INTEGER NOT NULL,source_scheme INTEGER NOT NULL,\
         source_port INTEGER NOT NULL,last_update_utc INTEGER NOT NULL,source_type INTEGER NOT NULL,\
         has_cross_site_ancestor INTEGER NOT NULL);",
    )?;
    let sealed = [b"v10".as_slice(), &sealed].concat();
    db.execute(
        "INSERT INTO cookies VALUES (0, ?1, '', ?2, '', ?3, '/', 0, 0, 1, 0, 0, 0, 1, 0, 1, 80, 0, 0, 0)",
        rusqlite::params![domain, name, sealed],
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn cookies_chrome_sealed_open_and_go_to_their_host() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    chrome_profile(&sandbox, "127.0.0.1", "sid", "sealed-value")?;
    let login = sandbox.run(&["auth", "login", &host, "--browser", "chrome", "--yes"])?;
    let stderr = text(&login.stderr)?;
    assert!(login.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "Found 1 cookie for {host} in chrome's profile ~/.config/google-chrome/Default\n"
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains("sid · 127.0.0.1 · ends with the browser's session\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("sealed-value"), "{stderr}");
    let output = sandbox.run(&["get", &format!("{}/data.csv", site.url)])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(
        heard(&site, "cookie"),
        [Some("sid=sealed-value".to_owned())]
    );
    Ok(())
}

/// chrome and edge on windows seal cookies with app-bound encryption, which only they may open.
#[cfg(windows)]
#[test]
fn chrome_on_windows_is_refused_with_what_works_instead() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&[
        "--json",
        "auth",
        "login",
        "data.example.org",
        "--browser",
        "chrome",
        "--yes",
    ])?;
    assert_eq!(output.status.code(), Some(2));
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        "chrome on windows keeps its cookies under app-bound encryption, which only chrome itself may open"
    );
    Ok(())
}

/// a link with no extension that answers with a web page is refused, unless the person asks for
/// the page, on the command line or in data.toml.
#[test]
fn a_web_page_is_pinned_only_when_asked_for() -> Result {
    let sandbox = Sandbox::new()?;
    let page = b"<html><body>the report</body></html>";
    let length = page.len().to_string();
    let site = Routes::start(vec![(
        "/report".to_owned(),
        reply(
            "200 OK",
            &[("Content-Type", "text/html"), ("Content-Length", &length)],
            page,
        ),
    )])?;
    let link = format!("{}/report", site.url);
    let refused = sandbox.run(&["add", &link])?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("if the page itself is the data, add --web-page"),
        "{stderr}"
    );
    let added = sandbox.run(&["add", &link, "--web-page"])?;
    assert!(added.status.success(), "{}", text(&added.stderr)?);
    let manifest = std::fs::read_to_string(sandbox.work().join("data.toml"))?;
    assert!(manifest.contains("web_page = true"), "{manifest}");
    let lock = std::fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains(&crate::support::sha256_hex(page)), "{lock}");
    assert_eq!(
        std::fs::read(sandbox.work().join("data/report/report"))?,
        page
    );
    // data.toml's word alone lets a sync pin a page, as a teammate's clone does
    site.change(vec![(
        "/notes".to_owned(),
        reply(
            "200 OK",
            &[("Content-Type", "text/html"), ("Content-Length", &length)],
            page,
        ),
    )]);
    sandbox.allow(&site.url);
    let notes = format!(
        "{manifest}\n[datasets.notes]\nref = \"{}/notes\"\n",
        site.url
    );
    std::fs::write(sandbox.work().join("data.toml"), &notes)?;
    let refused = sandbox.run(&["sync"])?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("add web_page = true to [datasets.notes] in data.toml"),
        "{stderr}"
    );
    std::fs::write(
        sandbox.work().join("data.toml"),
        format!("{notes}web_page = true\n"),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert!(synced.status.success(), "{}", text(&synced.stderr)?);
    assert_eq!(
        std::fs::read(sandbox.work().join("data/notes/notes"))?,
        page
    );
    for (asked, code) in [(&[][..], Some(10)), (&["--web-page"][..], Some(0))] {
        let mut args = vec!["get", link.as_str(), "-o", "page.html", "--force"];
        args.extend_from_slice(asked);
        let output = sandbox.run(&args)?;
        assert_eq!(
            output.status.code(),
            code,
            "{asked:?}: {}",
            text(&output.stderr)?
        );
    }
    Ok(())
}

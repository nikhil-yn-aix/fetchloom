//! any website: its own credential, saved for its host with `auth login <host>`, a token, a header
//! or cookies, sent to that host alone and named when the host refuses it; and a web page a
//! person asks for as the data.
#![cfg(debug_assertions)]

use crate::auth::with_stdin;
use crate::support::{
    Part, Proxy, Result, Routes, Running, Sandbox, Server, file, json, reply, text,
};

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

/// a proxy reads a plain http request whole, so neither a saved token nor a `.netrc` login goes
/// to a site on this machine through one; `NO_PROXY` sends the request straight there.
#[test]
fn no_key_goes_over_plain_http_through_a_proxy() -> Result {
    let sandbox = Sandbox::new()?;
    let proxy = Proxy::start()?;
    let csv = || file(b"a,b\n", "\"v1\"");
    let site = Routes::start(vec![("/data.csv".to_owned(), csv())])?;
    let lab = Routes::start(vec![("/data.csv".to_owned(), csv())])?;
    let refused = |challenge: &str| {
        reply(
            "401 Unauthorized",
            &[("WWW-Authenticate", challenge), ("Content-Length", "0")],
            b"",
        )
    };
    site.turns("/data.csv", vec![refused("Bearer realm=\"lab\"")]);
    lab.turns("/data.csv", vec![refused("Basic realm=\"lab\"")]);
    sign_in_with_token(&sandbox, &host_of(&site), "t-proxied")?;
    let login = with_stdin(
        sandbox.command(&["auth", "login", &host_of(&lab), "--with-token"]),
        "ada:pw-proxied\n",
    )?;
    assert!(login.status.success(), "{}", text(&login.stderr)?);
    let mut asked = Vec::new();
    for (server, dir) in [(&site, "one"), (&lab, "two")] {
        let link = format!("{}/data.csv", server.url);
        let proxied = sandbox
            .command(&["--json", "get", &link, "--dir", "proxied"])
            .env("HTTP_PROXY", &proxy.url)
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .output()?;
        assert_eq!(
            proxied.status.code(),
            Some(40),
            "{}",
            text(&proxied.stdout)?
        );
        let error = &json(&proxied)?["errors"][0];
        assert_eq!(error["kind"], "auth.required");
        assert_eq!(
            error["help"],
            format!(
                "a proxy, here {}, reads a plain http request whole, so fetchloom sends no key through one: add 127.0.0.1 to NO_PROXY to send the request straight there, or ask the site for an https link",
                proxy.url
            )
        );
        asked.push(format!("get {link} http/1.1"));
        assert_eq!(proxy.asked(), asked);
        let straight = sandbox
            .command(&["get", &link, "--dir", dir])
            .env("HTTP_PROXY", &proxy.url)
            .env("NO_PROXY", "127.0.0.1")
            .output()?;
        assert!(straight.status.success(), "{}", text(&straight.stderr)?);
        assert_eq!(proxy.asked(), asked);
    }
    assert_eq!(
        heard(&site, "authorization"),
        [None, Some("bearer t-proxied".to_owned())]
    );
    assert_eq!(
        heard(&lab, "authorization"),
        [None, Some("basic ywrhonb3lxbyb3hpzwq=".to_owned())]
    );
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

/// a sign-in page where a file was expected, once a saved token went, says the token may have
/// expired and how to save a new one, unless the person asked for the page.
#[test]
fn a_sign_in_page_after_a_saved_token_names_the_command_that_saves_a_new_one() -> Result {
    let sandbox = Sandbox::new()?;
    let page = b"<html><form action=/login>sign in</form></html>";
    let length = page.len().to_string();
    let site = Routes::start(vec![(
        "/login".to_owned(),
        reply(
            "200 OK",
            &[("Content-Type", "text/html"), ("Content-Length", &length)],
            page,
        ),
    )])?;
    let to_login = || {
        reply(
            "302 Found",
            &[("Location", "/login"), ("Content-Length", "0")],
            b"",
        )
    };
    let notes = b"<html><body>the notes</body></html>";
    let notes_length = notes.len().to_string();
    site.change(vec![
        ("/data".to_owned(), to_login()),
        ("/report.html".to_owned(), to_login()),
        (
            "/notes".to_owned(),
            reply(
                "200 OK",
                &[
                    ("Content-Type", "text/html"),
                    ("Content-Length", &notes_length),
                ],
                notes,
            ),
        ),
    ]);
    let host = host_of(&site);
    sign_in_with_token(&sandbox, &host, "t-old")?;
    let link = format!("{}/data", site.url);
    let output = sandbox.run(&["get", &link, "-o", "data.csv"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(40), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "error: {host} sent a web page, such as its sign-in page, where a file was expected, so the token saved for it may have expired"
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "help: save a current one with `fetchloom auth login {host} --token`, then run `fetchloom get {link} -o data.csv` again"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("t-old"), "{stderr}");
    assert!(!sandbox.work().join("data.csv").exists());
    // a page the link moved to is no page the person asked for, nor one a `.html` link named
    for (moved, asked) in [
        (link.clone(), true),
        (format!("{}/report.html", site.url), false),
    ] {
        let mut args = vec!["get", moved.as_str(), "-o", "moved.html"];
        if asked {
            args.push("--web-page");
        }
        let refused = sandbox.run(&args)?;
        let stderr = text(&refused.stderr)?;
        assert_eq!(refused.status.code(), Some(40), "{moved}: {stderr}");
        assert!(stderr.contains("such as its sign-in page"), "{stderr}");
        assert!(!sandbox.work().join("moved.html").exists());
    }
    let notes_link = format!("{}/notes", site.url);
    let asked = sandbox.run(&["get", &notes_link, "-o", "notes.html", "--web-page"])?;
    assert!(asked.status.success(), "{}", text(&asked.stderr)?);
    assert_eq!(std::fs::read(sandbox.work().join("notes.html"))?, notes);
    // a page data.toml asks for is the data when its pin is fetched again, as in a clone
    let added = sandbox.run(&["add", &notes_link, "--web-page", "--as", "notes"])?;
    assert!(added.status.success(), "{}", text(&added.stderr)?);
    crate::support::delete_folder(&sandbox.cache())?;
    crate::support::delete_folder(&sandbox.work().join("data"))?;
    sandbox.allow(&site.url);
    let synced = sandbox.run(&["sync"])?;
    assert!(synced.status.success(), "{}", text(&synced.stderr)?);
    assert_eq!(
        std::fs::read(sandbox.work().join("data/notes/notes"))?,
        notes
    );
    Ok(())
}

/// a website that refuses a link and names no way in is offered each way to sign in: in its
/// help, and at a terminal as a choice that saves the key and runs the command again.
#[test]
fn a_website_that_refuses_is_offered_each_way_in() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![(
        "/data.csv".to_owned(),
        reply("403 Forbidden", &[("Content-Length", "0")], b""),
    )])?;
    let host = host_of(&site);
    let link = format!("{}/data.csv", site.url);
    let output = sandbox.run(&["--json", "get", &link])?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["help"],
        format!(
            "if it needs a sign-in, save it with `fetchloom auth login {host}` and --browser <name> to take the cookies of a browser signed in there, --cookies <file>, --token or --header <name>, then run `fetchloom --json get {link}` again"
        )
    );
    assert_eq!(error["fix"], serde_json::Value::Null);
    #[cfg(target_os = "linux")]
    {
        let gated = crate::support::Server::start(
            "/data.csv",
            vec![
                reply("403 Forbidden", &[("Content-Length", "0")], b""),
                file(b"a,b\n", "\"v1\""),
            ],
        )?;
        let host = gated.url.trim_start_matches("http://");
        let host = host.split_once('/').map_or(host, |(host, _)| host);
        let get = sandbox.command(&["get", &gated.url, "-o", "data.csv"]);
        let asked = format!("sign in to {host} with");
        if let Some((code, screen)) =
            crate::support::typed(&sandbox.work(), &get, "", &asked, "3t-new\ry\r")?
        {
            assert_eq!(code, Some(0), "{screen}");
            assert!(
                screen.contains(&format!("Signed in to {host} with a token")),
                "{screen}"
            );
            assert!(!screen.contains("t-new"), "{screen}");
            let heads = gated.requests();
            assert!(
                heads
                    .get(1)
                    .is_some_and(|head| head.contains("authorization: bearer t-new")),
                "{heads:?}\n{screen}"
            );
            assert_eq!(std::fs::read(sandbox.work().join("data.csv"))?, b"a,b\n");
        }
    }
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
        text(&again.stderr)?.contains(&format!("no {host} key was saved")),
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
        (
            "X-API-Key: sk_live_secret",
            "--header takes a header's name alone, which holds no `:`, space or other character a name cannot",
        ),
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
        let told = [refused.stdout, refused.stderr].concat();
        assert!(!text(&told)?.contains("sk_live_secret"), "{}", text(&told)?);
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

/// a cookie set to last longer than any clock can count, as a broken export may write it, is
/// kept and told as lasting to year 9999, rather than crashing the sign-in or the listing.
#[test]
fn a_cookie_that_never_ends_is_kept_without_a_crash() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    let cookies = line("127.0.0.1", "/", false, i64::MAX, "forever", "f-value");
    std::fs::write(sandbox.work().join("cookies.txt"), cookies)?;
    let login = sandbox.run(&["auth", "login", &host, "--cookies", "cookies.txt", "--yes"])?;
    let stderr = text(&login.stderr)?;
    assert!(login.status.success(), "{stderr}");
    assert!(
        stderr.contains("forever · 127.0.0.1 · expires 9999-12-31"),
        "{stderr}"
    );
    let listed = sandbox.run(&["auth", "--offline"])?;
    assert!(listed.status.success(), "{}", text(&listed.stderr)?);
    let output = sandbox.run(&["get", &format!("{}/data.csv", site.url)])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(heard(&site, "cookie"), [Some("forever=f-value".to_owned())]);
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

/// the target and cookie of each request `server` took, a check that asks nothing left out.
fn cookies_by_target(server: &Routes) -> Vec<(String, Option<String>)> {
    server
        .requests()
        .iter()
        .filter_map(|head| {
            let target = head.lines().next()?.split(' ').nth(1)?.to_owned();
            let cookie = head.lines().find_map(|line| {
                let (field, value) = line.split_once(':')?;
                (field == "cookie").then(|| value.trim().to_owned())
            });
            Some((target, cookie))
        })
        .collect()
}

/// a cookie a signed-in site sets, as a renewed session, goes back to the site where its saved
/// credential may go and nowhere else: not to another port of its host, which plain http
/// reaches, nor through a proxy to a host its domain covers.
#[test]
fn a_cookie_a_signed_in_site_sets_goes_only_where_its_credential_may() -> Result {
    let sandbox = Sandbox::new()?;
    let moved = |location: &str, cookie: Option<&str>| {
        let mut headers = vec![("Location", location), ("Content-Length", "0")];
        headers.extend(cookie.map(|cookie| ("Set-Cookie", cookie)));
        reply("302 Found", &headers, b"")
    };
    let other = Routes::start(vec![("/file.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let elsewhere = format!("{}/file.csv", other.url);
    let site = Routes::start(vec![
        (
            "/renew".to_owned(),
            moved("/again", Some("session=s-new; Path=/")),
        ),
        ("/again".to_owned(), moved(&elsewhere, None)),
    ])?;
    saved_cookies(&sandbox, &host_of(&site), 4_000_000_000)?;
    sandbox.allow(&other.url);
    let output = sandbox.run(&["get", &format!("{}/renew", site.url), "-o", "one.csv"])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(
        cookies_by_target(&site),
        [
            ("/renew".to_owned(), Some("session=s-old".to_owned())),
            ("/again".to_owned(), Some("session=s-new".to_owned()))
        ]
    );
    assert_eq!(cookies_by_target(&other), [("/file.csv".to_owned(), None)]);
    let proxy = Routes::start(vec![
        (
            "http://data.lab/renew".to_owned(),
            moved(
                "http://files.data.lab/file.csv",
                Some("session=s-new; Domain=data.lab; Path=/"),
            ),
        ),
        (
            "http://files.data.lab/file.csv".to_owned(),
            file(b"c,d\n", "\"v2\""),
        ),
    ])?;
    let cookies = [
        "# Netscape HTTP Cookie File\n".to_owned(),
        line("data.lab", "/", false, 4_000_000_000, "session", "s-old"),
    ]
    .concat();
    std::fs::write(sandbox.work().join("cookies.txt"), cookies)?;
    let login = sandbox.run(&[
        "auth",
        "login",
        "data.lab:80",
        "--cookies",
        "cookies.txt",
        "--yes",
    ])?;
    assert!(login.status.success(), "{}", text(&login.stderr)?);
    let proxied = sandbox
        .command(&["get", "http://data.lab/renew", "-o", "two.csv"])
        .env("HTTP_PROXY", &proxy.url)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()?;
    assert!(proxied.status.success(), "{}", text(&proxied.stderr)?);
    assert_eq!(
        cookies_by_target(&proxy),
        [
            ("http://data.lab/renew".to_owned(), None),
            ("http://files.data.lab/file.csv".to_owned(), None)
        ]
    );
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
    firefox_cookies(&root.join("Profiles/q1w2e3r4.default-release"), rows)
}

/// firefox's `cookies.sqlite` in `folder`, holding `rows` as [`firefox_profile`] does.
fn firefox_cookies(folder: &std::path::Path, rows: &[(&str, &str, i64, &str)]) -> Result {
    std::fs::create_dir_all(folder)?;
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
    let folder = sandbox
        .work()
        .parent()
        .ok_or("no sandbox")?
        .join("home/.config/google-chrome/Default");
    chrome_cookies(&folder, &[(domain, name, value, true)])
}

/// chrome's cookie database in the profile `folder`, holding `rows` of domain, name, value and
/// whether chrome sealed it, as [`chrome_profile`] does; an unsealed row keeps its value plain.
#[cfg(target_os = "linux")]
fn chrome_cookies(folder: &std::path::Path, rows: &[(&str, &str, &str, bool)]) -> Result {
    use aes::cipher::block_padding::Pkcs7;
    use aes::cipher::{BlockModeEncrypt as _, KeyIvInit as _};
    use sha2::Digest as _;
    // PBKDF2-HMAC-SHA1 of "peanuts" over "saltysalt", once, as chromium's posix_key_provider.cc
    // gives it
    let key: [u8; 16] = [
        0xfd, 0x62, 0x1f, 0xe5, 0xa2, 0xb4, 0x02, 0x53, 0x9d, 0xfa, 0x14, 0x7c, 0xa9, 0x27, 0x27,
        0x78,
    ];
    let folder = folder.join("Network");
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
    for (domain, name, value, sealed) in rows {
        let (plain, sealed) = if *sealed {
            let plain = [
                sha2::Sha256::digest(domain.as_bytes()).as_slice(),
                value.as_bytes(),
            ]
            .concat();
            let sealed = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &[b' '; 16].into())
                .encrypt_padded_vec::<Pkcs7>(&plain);
            (String::new(), [b"v10".as_slice(), &sealed].concat())
        } else {
            ((*value).to_owned(), Vec::new())
        };
        db.execute(
            "INSERT INTO cookies VALUES (0, ?1, '', ?2, ?3, ?4, '/', 0, 0, 1, 0, 0, 0, 1, 0, 1, 80, 0, 0, 0)",
            rusqlite::params![domain, name, plain, sealed],
        )?;
    }
    Ok(())
}

/// a project folder holding a folder named as a profile, as a cloned repo may, is never read for
/// it: a profile's name is looked up under firefox's own folders, and only an absolute path names
/// a folder of its own.
#[test]
fn a_profile_name_never_reads_the_project_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    let later = 4_000_000_000_000;
    firefox_profile(&sandbox, &[("session", "127.0.0.1", later, "")])?;
    let planted = sandbox.work().join("default-release");
    firefox_cookies(&planted, &[("planted", "127.0.0.1", later, "")])?;
    let login = |profile: &str| {
        sandbox.run(&[
            "auth",
            "login",
            &host,
            "--browser",
            &format!("firefox:{profile}"),
            "--yes",
        ])
    };
    let named = login("default-release")?;
    let stderr = text(&named.stderr)?;
    assert!(named.status.success(), "{stderr}");
    assert!(stderr.contains("session · 127.0.0.1"), "{stderr}");
    assert!(!stderr.contains("planted"), "{stderr}");
    let output = sandbox.run(&["get", &format!("{}/data.csv", site.url)])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(
        heard(&site, "cookie"),
        [Some("session=session-value".to_owned())]
    );
    for relative in ["./default-release", "default-release/../default-release"] {
        let refused = login(relative)?;
        let stderr = text(&refused.stderr)?;
        assert_eq!(refused.status.code(), Some(2), "{relative}: {stderr}");
        assert!(
            stderr.contains("found no firefox profile with cookies"),
            "{stderr}"
        );
    }
    let absolute = login(&planted.display().to_string())?;
    let stderr = text(&absolute.stderr)?;
    assert!(absolute.status.success(), "{stderr}");
    assert!(stderr.contains("planted · 127.0.0.1"), "{stderr}");
    Ok(())
}

/// chrome's profile names go by its own folder too, and a cookie chrome left unsealed, which it
/// never does, is left out: it was put there by someone else.
#[cfg(target_os = "linux")]
#[test]
fn a_chrome_profile_name_never_reads_the_project_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let site = Routes::start(vec![("/data.csv".to_owned(), file(b"a,b\n", "\"v1\""))])?;
    let host = host_of(&site);
    let home = sandbox.work().parent().ok_or("no sandbox")?.join("home");
    chrome_cookies(
        &home.join(".config/google-chrome/Default"),
        &[
            ("127.0.0.1", "sid", "sealed-value", true),
            ("127.0.0.1", "plain", "plain-value", false),
        ],
    )?;
    chrome_cookies(
        &sandbox.work().join("Default"),
        &[("127.0.0.1", "planted", "planted-value", true)],
    )?;
    let login = sandbox.run(&[
        "auth",
        "login",
        &host,
        "--browser",
        "chrome:Default",
        "--yes",
    ])?;
    let stderr = text(&login.stderr)?;
    assert!(login.status.success(), "{stderr}");
    assert!(!stderr.contains("planted"), "{stderr}");
    assert!(
        stderr.contains(
            "Left out 1 cookie kept unsealed, which chromium never does: plain of 127.0.0.1"
        ),
        "{stderr}"
    );
    let output = sandbox.run(&["get", &format!("{}/data.csv", site.url)])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(
        heard(&site, "cookie"),
        [Some("sid=sealed-value".to_owned())]
    );
    let refused = sandbox.run(&[
        "auth",
        "login",
        &host,
        "--browser",
        "chrome:./Default",
        "--yes",
    ])?;
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stderr)?);
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

/// a page an earlier `--web-page` kept in the cache is still a page: a later run without the
/// flag is refused when the server says the page is unchanged, as a 304 does, and offline, where
/// only the cache answers; with the flag, both take it.
#[test]
fn a_kept_page_is_refused_unless_asked_for_on_every_path() -> Result {
    let sandbox = Sandbox::new()?;
    let page = b"<html><body>the report</body></html>";
    let length = page.len().to_string();
    let unchanged = || reply("304 Not Modified", &[("ETag", "\"p1\"")], b"");
    let server = Server::start(
        "/report",
        vec![
            reply(
                "200 OK",
                &[
                    ("Content-Type", "text/html"),
                    ("ETag", "\"p1\""),
                    ("Content-Length", &length),
                ],
                page,
            ),
            unchanged(),
            unchanged(),
        ],
    )?;
    let link = server.url.clone();
    let kept = sandbox.run(&["add", &link, "--web-page"])?;
    assert!(kept.status.success(), "{}", text(&kept.stderr)?);
    let refused = sandbox.run(&["get", &link, "-o", "again.html"])?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(10), "{stderr}");
    assert!(stderr.contains("answered with a web page"), "{stderr}");
    assert!(!sandbox.work().join("again.html").exists());
    let asked = sandbox.run(&["get", &link, "-o", "again.html", "--web-page"])?;
    assert!(asked.status.success(), "{}", text(&asked.stderr)?);
    assert_eq!(std::fs::read(sandbox.work().join("again.html"))?, page);
    let heads = server.requests();
    assert_eq!(heads.len(), 3, "{heads:?}");
    assert!(
        heads[1..].iter().all(|head| head.contains("if-none-match")),
        "{heads:?}"
    );
    let offline = sandbox.run(&["get", &link, "-o", "offline.html", "--offline"])?;
    let stderr = text(&offline.stderr)?;
    assert_eq!(offline.status.code(), Some(10), "{stderr}");
    assert!(stderr.contains("answered with a web page"), "{stderr}");
    let offline = sandbox.run(&[
        "get",
        &link,
        "-o",
        "offline.html",
        "--offline",
        "--web-page",
    ])?;
    assert!(offline.status.success(), "{}", text(&offline.stderr)?);
    Ok(())
}

/// a page pinned with `web_page = true` follows that line: sync takes it while data.toml asks
/// for it, and once the line is gone, the link is asked again and its page refused, also when
/// the server says the page is unchanged.
/// a project's download that waited while another fetchloom took the same link is handed the
/// page that run kept, without asking the server, and takes it only as the page rule allows.
#[test]
fn a_page_another_run_kept_is_refused_unless_asked_for() -> Result {
    let sandbox = Sandbox::new()?;
    let page = b"<html><body>the report</body></html>";
    let mut held = reply(
        "200 OK",
        &[
            ("Content-Type", "text/html"),
            ("ETag", "\"p1\""),
            ("Content-Length", &page.len().to_string()),
        ],
        b"",
    );
    let (release, wait) = std::sync::mpsc::channel();
    held.push(Part::Wait(wait));
    held.push(Part::Bytes(page.to_vec()));
    let server = Server::start("/report", vec![held])?;
    let link = server.url.clone();
    let first = Running::start(&mut sandbox.command(&["add", &link, "--web-page"]))?;
    server.wait_for_requests(1)?;
    let other = sandbox.folder("other")?;
    let second = Running::start(
        sandbox
            .command_in(&other, &["add", &link])
            .env("FETCHLOOM_TEST_TICKER", "on"),
    )?;
    let waiting = second.wait_for("another fetchloom is downloading it");
    drop(release);
    waiting?;
    let first = first.finish()?;
    assert!(first.status.success(), "{}", text(&first.stderr)?);
    let second = second.finish()?;
    let stderr = text(&second.stderr)?;
    assert_eq!(second.status.code(), Some(10), "{stderr}");
    assert!(stderr.contains("answered with a web page"), "{stderr}");
    assert!(!other.join("data.toml").exists());
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// an offline add of a link whose page another project kept learns the file from what the link
/// served last, and takes that page only when asked for one.
#[test]
fn an_offline_add_takes_a_kept_page_only_when_asked() -> Result {
    let sandbox = Sandbox::new()?;
    let page = b"<html><body>the notes</body></html>";
    let server = Server::start(
        "/notes",
        vec![reply(
            "200 OK",
            &[
                ("Content-Type", "text/html"),
                ("ETag", "\"n1\""),
                ("Content-Length", &page.len().to_string()),
            ],
            page,
        )],
    )?;
    let kept = sandbox.run(&["add", &server.url, "--web-page"])?;
    assert!(kept.status.success(), "{}", text(&kept.stderr)?);
    let other = sandbox.folder("other")?;
    let refused = sandbox
        .command_in(&other, &["add", &server.url, "--offline"])
        .output()?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(10), "{stderr}");
    assert!(stderr.contains("answered with a web page"), "{stderr}");
    assert!(!other.join("data/notes/notes").exists());
    let asked = sandbox
        .command_in(&other, &["add", &server.url, "--offline", "--web-page"])
        .output()?;
    assert!(asked.status.success(), "{}", text(&asked.stderr)?);
    assert_eq!(std::fs::read(other.join("data/notes/notes"))?, page);
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// a pin made while data.toml asked for a page is never carried once it no longer asks, though
/// an exclude changed with it: the link is asked again, and the page refused.
#[test]
fn a_page_pin_is_never_carried_once_web_page_goes() -> Result {
    let sandbox = Sandbox::new()?;
    let page = b"<html><body>the notes</body></html>";
    let length = page.len().to_string();
    let server = Server::start(
        "/notes",
        vec![
            reply(
                "200 OK",
                &[
                    ("Content-Type", "text/html"),
                    ("ETag", "\"n1\""),
                    ("Content-Length", &length),
                ],
                page,
            ),
            reply("304 Not Modified", &[("ETag", "\"n1\"")], b""),
        ],
    )?;
    let added = sandbox.run(&["add", &server.url, "--web-page"])?;
    assert!(added.status.success(), "{}", text(&added.stderr)?);
    sandbox.allow(&server.url);
    let manifest = sandbox.work().join("data.toml");
    let asked = std::fs::read_to_string(&manifest)?;
    assert!(asked.contains("web_page = true\n"), "{asked}");
    std::fs::write(
        &manifest,
        asked.replace("web_page = true\n", "exclude = \"*.csv\"\n"),
    )?;
    let refused = sandbox.run(&["sync"])?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("add web_page = true to [datasets.notes] in data.toml"),
        "{stderr}"
    );
    let heads = server.requests();
    assert!(
        heads
            .get(1)
            .is_some_and(|head| head.contains("if-none-match")),
        "{heads:?}"
    );
    Ok(())
}

#[test]
fn a_pinned_page_follows_web_page_in_data_toml() -> Result {
    let sandbox = Sandbox::new()?;
    let page = b"<html><body>the notes</body></html>";
    let length = page.len().to_string();
    let server = Server::start(
        "/notes",
        vec![
            reply(
                "200 OK",
                &[
                    ("Content-Type", "text/html"),
                    ("ETag", "\"n1\""),
                    ("Content-Length", &length),
                ],
                page,
            ),
            reply("304 Not Modified", &[("ETag", "\"n1\"")], b""),
        ],
    )?;
    let link = server.url.clone();
    let added = sandbox.run(&["add", &link, "--web-page"])?;
    assert!(added.status.success(), "{}", text(&added.stderr)?);
    sandbox.allow(&link);
    let settled = sandbox.run(&["sync"])?;
    assert!(settled.status.success(), "{}", text(&settled.stderr)?);
    let manifest = sandbox.work().join("data.toml");
    let asked = std::fs::read_to_string(&manifest)?;
    assert!(asked.contains("web_page = true\n"), "{asked}");
    std::fs::write(&manifest, asked.replace("web_page = true\n", ""))?;
    let check = sandbox.run(&["lock", "--check"])?;
    assert_eq!(check.status.code(), Some(1), "{}", text(&check.stderr)?);
    let refused = sandbox.run(&["sync"])?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("add web_page = true to [datasets.notes] in data.toml"),
        "{stderr}"
    );
    let heads = server.requests();
    assert!(
        heads
            .get(1)
            .is_some_and(|head| head.contains("if-none-match")),
        "{heads:?}"
    );
    std::fs::write(&manifest, asked)?;
    let again = sandbox.run(&["sync"])?;
    assert!(again.status.success(), "{}", text(&again.stderr)?);
    assert_eq!(
        std::fs::read(sandbox.work().join("data/notes/notes"))?,
        page
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

/// a public server's basic login, saved as `.netrc` keeps it, opens its file as curl's would.
#[test]
#[ignore = "live: talks to httpbin.org"]
fn live_a_saved_login_opens_a_public_basic_auth_endpoint() -> Result {
    let sandbox = Sandbox::new()?;
    let link = "https://httpbin.org/basic-auth/ada/pw-live";
    let refused = sandbox.run(&["--json", "get", link, "-o", "answer.json"])?;
    assert_eq!(
        refused.status.code(),
        Some(40),
        "{}",
        text(&refused.stderr)?
    );
    let help = json(&refused)?["errors"][0]["help"].clone();
    assert!(
        help.as_str()
            .is_some_and(|help| help.contains("`fetchloom auth login httpbin.org`")),
        "{help}"
    );
    let login = with_stdin(
        sandbox.command(&["auth", "login", "httpbin.org", "--with-token"]),
        "ada:pw-live\n",
    )?;
    assert!(login.status.success(), "{}", text(&login.stderr)?);
    let output = sandbox.run(&["get", link, "-o", "answer.json"])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    let answer = std::fs::read_to_string(sandbox.work().join("answer.json"))?;
    assert!(answer.contains("\"authenticated\": true"), "{answer}");
    Ok(())
}

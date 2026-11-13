//! a website's own credential, saved for its host with `auth login <host>`: a token, a header or
//! cookies, sent to that host alone and named when the host refuses it.
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

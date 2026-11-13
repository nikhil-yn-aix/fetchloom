//! proxies: behind one, a link nobody typed is still kept away from this machine and its network,
//! while the proxy and the hosts a person typed may be on it.

use std::fs;

use crate::support::{Proxy, Refusing, Result, Sandbox, Server, file, json, reply, text};

const IRIS: &[u8] =
    b"sepal_length,sepal_width,petal_length,petal_width,species\n5.1,3.5,1.4,0.2,setosa\n";

#[test]
fn a_redirect_to_this_machine_is_refused_before_a_proxy_connects_to_it() -> Result {
    let sandbox = Sandbox::new()?;
    let proxy = Proxy::start()?;
    let inward = reply(
        "302 Found",
        &[
            ("Location", "https://localhost:9/admin"),
            ("Content-Length", "0"),
        ],
        b"",
    );
    let server = Server::start("/iris.csv", vec![inward])?;
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("HTTP_PROXY", &proxy.url)
        .env("HTTPS_PROXY", &proxy.url)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()?;
    // the proxy carried the link that was typed, and was never asked for this machine
    let asked = proxy.asked();
    assert_eq!(asked, [format!("get {} http/1.1", server.url)], "{asked:?}");
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(document["errors"][0]["kind"], "network.refused");
    Ok(())
}

#[test]
fn a_redirect_is_followed_one_checked_hop_at_a_time() -> Result {
    let sandbox = Sandbox::new()?;
    let proxy = Proxy::start()?;
    let moved = reply(
        "301 Moved Permanently",
        &[("Location", "/moved/iris.csv"), ("Content-Length", "0")],
        b"",
    );
    let server = Server::start("/iris.csv", vec![moved, file(IRIS, "\"v1\"")])?;
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("HTTP_PROXY", &proxy.url)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let moved = server.url.replace("/iris.csv", "/moved/iris.csv");
    assert_eq!(
        proxy.asked(),
        [
            format!("get {} http/1.1", server.url),
            format!("get {moved} http/1.1")
        ]
    );
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

#[test]
fn a_proxy_on_this_machine_carries_a_host_that_was_typed() -> Result {
    let sandbox = Sandbox::new()?;
    let proxy = Proxy::start()?;
    let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    // a proxy named by its host, as settings usually name one on a local network
    let named = proxy.url.replace("127.0.0.1", "localhost");
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("HTTP_PROXY", &named)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(proxy.asked(), [format!("get {} http/1.1", server.url)]);
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// most sites send http on to https, an origin nobody trusted and so free to reach any public
/// address. a scripted proxy stands in for the site and answers the tunnel with 502.
#[test]
fn a_typed_http_link_follows_its_site_to_https() -> Result {
    let sandbox = Sandbox::new()?;
    let link = "http://data.example.invalid/iris.csv";
    let upgrade = reply(
        "301 Moved Permanently",
        &[
            ("Location", "https://data.example.invalid/iris.csv"),
            ("Content-Length", "0"),
        ],
        b"",
    );
    let site = Server::start(
        "",
        vec![
            upgrade,
            reply("502 Bad Gateway", &[("Content-Length", "0")], b""),
        ],
    )?;
    let output = sandbox
        .command(&["--json", "get", link])
        .env("HTTP_PROXY", &site.url)
        .env("HTTPS_PROXY", &site.url)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .env("FETCHLOOM_RETRIES", "0")
        .output()?;
    let asked: Vec<String> = site
        .requests()
        .iter()
        .filter_map(|head| head.lines().next().map(str::to_owned))
        .collect();
    assert_eq!(
        asked,
        [
            format!("get {link} http/1.1"),
            "connect data.example.invalid:443 http/1.1".to_owned()
        ],
        "{}",
        text(&output.stdout)?
    );
    assert_ne!(json(&output)?["errors"][0]["kind"], "network.refused");
    Ok(())
}

/// a 407 answers an https tunnel and a plain http request alike; the error names the proxy, not
/// the site, a retry would get no further, and the help fits each kind of login.
#[test]
fn a_proxy_that_wants_a_login_is_named_and_asked_once() -> Result {
    let sandbox = Sandbox::new()?;
    for (link, variable, scheme, help) in [
        (
            "https://data.example.org/iris.csv",
            "HTTPS_PROXY",
            "NTLM",
            "it likely wants your windows sign-in, which fetchloom cannot send: run a local relay such as px or cntlm, and point HTTPS_PROXY and HTTP_PROXY at it",
        ),
        (
            "http://data.example.org/iris.csv",
            "HTTP_PROXY",
            "Basic realm=\"lab\"",
            "give it a name and password in its address, as http://name:password@proxy.example.org:8080, in HTTPS_PROXY and HTTP_PROXY",
        ),
    ] {
        let login = || {
            reply(
                "407 Proxy Authentication Required",
                &[("Proxy-Authenticate", scheme), ("Content-Length", "0")],
                b"",
            )
        };
        let proxy = Server::start("", vec![login(), login()])?;
        let output = sandbox
            .command(&["--json", "get", link])
            .env(variable, &proxy.url)
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .env("FETCHLOOM_RETRIES", "1")
            .output()?;
        assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "network.unreachable", "{error}");
        assert_eq!(
            error["message"],
            format!(
                "the proxy at {} asked for a login before it would reach data.example.org",
                proxy.url
            )
        );
        assert_eq!(error["help"], help);
        assert_eq!(
            proxy.requests().len(),
            1,
            "{link}: another try gets no further"
        );
    }
    Ok(())
}

/// passing over the setting, as the HTTP stack would, sends every request around the proxy; the
/// error names the variable and never shows a login in it.
#[test]
fn a_proxy_setting_that_names_no_proxy_stops_before_any_request() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    for (variable, value, shown) in [
        ("HTTP_PROXY", "htp:/proxy:80", "htp:/proxy:80"),
        (
            "all_proxy",
            "htp://name:secret@proxy:80",
            "htp://***@proxy:80",
        ),
    ] {
        let output = sandbox
            .command(&["--json", "get", &server.url])
            .env(variable, value)
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .output()?;
        assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
        let displayed_variable = if cfg!(windows) {
            variable.to_ascii_uppercase()
        } else {
            variable.to_owned()
        };
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "usage", "{error}");
        assert_eq!(
            error["message"],
            format!("{displayed_variable} is \"{shown}\", which is not a proxy address")
        );
        assert_eq!(
            error["help"],
            format!(
                "set {displayed_variable} to a proxy, such as http://proxy.example.org:8080, or unset {displayed_variable} to connect without one. `fetchloom help environment` lists the proxy settings"
            )
        );
    }
    assert!(
        server.requests().is_empty(),
        "nothing went around the proxy"
    );
    Ok(())
}

/// the error names the variable it came from and blames no site, and no retry follows, since none
/// reaches a proxy that is down.
#[test]
fn a_proxy_that_cannot_be_reached_is_named_and_not_tried_again() -> Result {
    let sandbox = Sandbox::new()?;
    let refusing = Refusing::new()?;
    let proxy = format!("http://{}", refusing.address);
    let output = sandbox
        .command(&["--json", "get", "https://data.example.org/iris.csv"])
        .env("HTTPS_PROXY", &proxy)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .env("FETCHLOOM_RETRIES", "1")
        .output()?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "network.unreachable", "{error}");
    assert_eq!(
        error["message"],
        format!(
            "could not reach the proxy at {proxy}, which HTTPS_PROXY names: it refused the connection"
        )
    );
    assert_eq!(
        error["help"],
        "check HTTPS_PROXY, or unset HTTPS_PROXY to connect without a proxy. `fetchloom help environment` lists the proxy settings"
    );
    assert_eq!(document["work"]["retries"], 0, "{document}");
    assert!(!text(&output.stderr)?.contains("trying again"));
    Ok(())
}

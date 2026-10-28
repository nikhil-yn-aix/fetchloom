//! proxies: behind one, a link nobody typed is still kept away from this machine and its network,
//! while the proxy and the hosts a person typed may be on it.

use std::fs;

use crate::support::{Proxy, Result, Sandbox, Server, file, reply, text};

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

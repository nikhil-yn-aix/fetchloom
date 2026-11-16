//! a pinned file whose source is gone comes back from software heritage or the wayback machine,
//! checked against its pin, unless this machine may have needed a key for it, the link may be a
//! key, or the person turned the archives off.

use std::fs;

use crate::project::IRIS;
use crate::support::{
    Result, Routes, Sandbox, delete, file, json, object, reply, sha256_hex, text,
};

const LINK: &str = "https://data.example.org/files/iris.csv";

/// the stand-in for every host, with iris at [`LINK`].
fn hosts(link: &str) -> Result<Routes> {
    let target = link.replacen("https://", "/", 1);
    Routes::start(vec![(target, file(IRIS, "\"v1\""))])
}

/// `link` added in the work folder through `routes`, then the source gone, as a 404 says, and a
/// fresh clone in `name` with an empty cache of its own.
fn gone(sandbox: &Sandbox, routes: &Routes, link: &str, name: &str) -> Result<std::path::PathBuf> {
    let output = sandbox
        .command(&["add", link, "--as", "iris"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let target = link.replacen("https://", "/", 1);
    routes.change(vec![(target, reply("404 Not Found", &[], b""))]);
    crate::project::clone(sandbox, name)
}

/// where software heritage keeps the pinned bytes of iris.
fn heritage() -> String {
    format!(
        "/archive.softwareheritage.org/api/1/content/sha256:{}/raw/",
        sha256_hex(IRIS)
    )
}

/// the archives' requests among what `routes` was asked.
fn archived(routes: &Routes) -> Vec<String> {
    routes
        .targets()
        .into_iter()
        .filter(|target| {
            target.starts_with("/archive.softwareheritage.org/")
                || target.starts_with("/web.archive.org/")
        })
        .collect()
}

/// a replicator's clone of a project whose plain link now 404s syncs from software heritage, and
/// both the output and json name it.
#[cfg(debug_assertions)]
#[test]
fn a_gone_file_comes_back_from_software_heritage() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = hosts(LINK)?;
    let one = gone(&sandbox, &routes, LINK, "one")?;
    routes.change(vec![(heritage(), file(IRIS, "\"h\""))]);
    let run = |folder: &std::path::Path, args: &[&str], cache: &str| -> Result<_> {
        Ok(sandbox
            .command_in(folder, args)
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .env("FETCHLOOM_CACHE", sandbox.folder(cache)?)
            .output()?)
    };
    let output = run(&one, &["sync"], "one cache")?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("iris/iris.csv is gone from its link in data.lock or changed there, so it came from software heritage"),
        "{stderr}"
    );
    assert_eq!(
        fs::read(one.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    let two = crate::project::clone(&sandbox, "two")?;
    let document = json(&run(&two, &["--json", "sync"], "two cache")?)?;
    let warning = crate::support::warning(&document, "source.archived");
    assert!(
        warning["message"]
            .as_str()
            .is_some_and(|said| said.contains("software heritage")),
        "{document}"
    );
    Ok(())
}

/// software heritage's copy of other bytes is refused as a mirror's would be, and the wayback
/// machine's capture of the link, the first since the pin, gives the file.
#[cfg(debug_assertions)]
#[test]
fn other_bytes_from_an_archive_are_refused_and_the_next_is_asked() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = hosts(LINK)?;
    let one = gone(&sandbox, &routes, LINK, "one")?;
    let lock: toml::Table = fs::read_to_string(one.join("data.lock"))?.parse()?;
    let locked = lock["dataset"][0]["locked"].as_str().ok_or("no pin time")?;
    let from: String = locked
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect();
    let index: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("url", LINK)
        .append_pair("output", "json")
        .append_pair("fl", "timestamp")
        .append_pair("filter", "statuscode:200")
        .append_pair("from", &from)
        .append_pair("limit", "1")
        .finish();
    let other = b"sepal_length,species\n9.9,setosa\n";
    assert_eq!(other.len(), IRIS.len());
    routes.change(vec![
        (heritage(), file(other, "\"h\"")),
        (
            format!("/web.archive.org/cdx/search/cdx?{index}"),
            reply(
                "200 OK",
                &[("Content-Type", "application/json")],
                br#"[["timestamp"],["20990101000000"]]"#,
            ),
        ),
        (
            format!("/web.archive.org/web/20990101000000id_/{LINK}"),
            file(IRIS, "\"w\""),
        ),
    ]);
    let output = sandbox
        .command_in(&one, &["sync"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("one cache")?)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("so it came from the wayback machine"),
        "{stderr}"
    );
    assert_eq!(
        fs::read(one.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// a file whose host this machine signs in to may be private, so no archive hears of it, and the
/// failure says why; the same for every file once web archives are turned off, with how to turn
/// them on.
#[cfg(debug_assertions)]
#[test]
fn no_archive_is_asked_about_a_keyed_host_or_with_the_setting_off() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = hosts(LINK)?;
    let one = gone(&sandbox, &routes, LINK, "one")?;
    routes.change(vec![(heritage(), file(IRIS, "\"h\""))]);
    let run = |extra: Option<(&str, &str)>, cache: &str| -> Result<serde_json::Value> {
        let mut command = sandbox.command_in(&one, &["--json", "sync"]);
        command
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .env("FETCHLOOM_CACHE", sandbox.folder(cache)?);
        if let Some((name, value)) = extra {
            command.env(name, value);
        }
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
        json(&output)
    };
    let off = run(Some(("FETCHLOOM_WEB_ARCHIVES", "false")), "off cache")?;
    assert!(
        off.to_string().contains(
            "web_archives is off: `fetchloom settings set web_archives true` turns it on"
        ),
        "{off}"
    );
    assert_eq!(archived(&routes), Vec::<String>::new());
    let home = sandbox.folder("home")?;
    fs::write(
        home.join(".netrc"),
        "machine data.example.org login ada password secret\n",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(home.join(".netrc"), fs::Permissions::from_mode(0o600))?;
    }
    let keyed = run(None, "keyed cache")?;
    assert!(
        keyed
            .to_string()
            .contains("this machine keeps a key for data.example.org"),
        "{keyed}"
    );
    assert_eq!(archived(&routes), Vec::<String>::new());
    Ok(())
}

/// a link with a query may hold a key, so the wayback machine never hears of it; software
/// heritage, asked by the SHA-256 alone, still is.
#[cfg(debug_assertions)]
#[test]
fn a_link_with_a_query_is_never_sent_to_the_wayback_machine() -> Result {
    let sandbox = Sandbox::new()?;
    let link = "https://data.example.org/get?file=iris.csv";
    let routes = hosts(link)?;
    let one = gone(&sandbox, &routes, link, "one")?;
    let output = sandbox
        .command_in(&one, &["--json", "sync"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("one cache")?)
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(10), "{document}");
    assert_eq!(archived(&routes), [heritage()], "{document}");
    assert!(
        document
            .to_string()
            .contains("the wayback machine was not asked, since the link holds a query"),
        "{document}"
    );
    Ok(())
}

/// a link whose host this machine keeps no key for may redirect to one it signs in to, whose
/// `.netrc` login then goes with the download: once the file is gone from its link and from
/// this machine, no archive hears of it, and the failure says why.
#[cfg(debug_assertions)]
#[test]
fn no_archive_is_asked_about_a_file_a_key_went_with() -> Result {
    let sandbox = Sandbox::new()?;
    let keyed = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"k1\""))])?;
    let away = format!("{}/iris.csv", keyed.url);
    let target = LINK.replacen("https://", "/", 1);
    let routes = Routes::start(vec![(
        target.clone(),
        reply(
            "302 Found",
            &[("Location", &away), ("Content-Length", "0")],
            b"",
        ),
    )])?;
    sandbox.allow(&keyed.url);
    let home = sandbox.folder("home")?;
    let port = keyed.url.rsplit(':').next().ok_or("no port")?;
    fs::write(
        home.join(".netrc"),
        format!("machine 127.0.0.1:{port} login lab password lab-secret\n"),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(home.join(".netrc"), fs::Permissions::from_mode(0o600))?;
    }
    let output = sandbox
        .command(&["add", LINK, "--as", "iris"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(
        keyed
            .requests()
            .iter()
            .any(|head| head.contains("authorization: basic")),
        "the login went with the redirected request"
    );
    routes.change(vec![
        (target, reply("404 Not Found", &[], b"")),
        (heritage(), file(IRIS, "\"h\"")),
    ]);
    delete(&sandbox.work().join("data").join("iris").join("iris.csv"))?;
    delete(&object(&sandbox.cache(), IRIS))?;
    let output = sandbox
        .command(&["--json", "sync"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(10), "{document}");
    assert!(
        document.to_string().contains(
            "no web archive was asked, since a key of this machine went with an earlier download of the file, so it may not be public"
        ),
        "{document}"
    );
    assert_eq!(archived(&routes), Vec::<String>::new());
    Ok(())
}

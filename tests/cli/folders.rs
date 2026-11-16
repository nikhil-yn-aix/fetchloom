//! a folder a web server lists, as apache, nginx, lighttpd, rclone and python's http.server do,
//! as one dataset: every file below it, each pinned by its SHA-256, its listing read again only
//! by `lock --upgrade`.

use std::fs;

use crate::lists::pinned;
use crate::support::{Part, Result, Routes, Sandbox, file, json, reply, sha256_hex, text, warning};

const IRIS: &[u8] = b"sepal,petal\n5.1,1.4\n";
const TIPS: &[u8] = b"bill,tip\n16.99,1.01\n";
const NEW: &[u8] = b"petal\n0.2\n";

/// a listing of `folder` that links each of `entries`, a folder's link ending in `/`, as one
/// server writes it.
type Lister = fn(&str, &[&str]) -> Vec<Part>;

/// a listing as python's http.server writes it.
fn python(folder: &str, entries: &[&str]) -> Vec<Part> {
    let items = entries
        .iter()
        .map(|entry| format!("<li><a href=\"{entry}\">{entry}</a></li>\n"))
        .collect::<Vec<_>>()
        .concat();
    page(&format!(
        "<!DOCTYPE HTML>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Directory listing for {folder}</title>\n</head>\n<body>\n<h1>Directory listing for {folder}</h1>\n<hr>\n<ul>\n{items}</ul>\n<hr>\n</body>\n</html>\n"
    ))
}

/// a listing as nginx's autoindex writes it, as dwd's opendata server sends it.
fn nginx(folder: &str, entries: &[&str]) -> Vec<Part> {
    let rows = entries
        .iter()
        .map(|entry| {
            format!(
                "<a href=\"{entry}\">{entry}</a>{:>40}{:>20}\r\n",
                "16-Nov-2026 09:00:07", "-"
            )
        })
        .collect::<Vec<_>>()
        .concat();
    page(&format!(
        "<html>\r\n<head><title>Index of {folder}</title></head>\r\n<body>\r\n<h1>Index of {folder}</h1><hr><pre><a href=\"../\">../</a>\r\n{rows}</pre><hr></body>\r\n</html>\r\n"
    ))
}

/// a listing as nginx's autoindex writes it, as dwd's states each file's size in bytes.
fn nginx_sized(folder: &str, entries: &[&str]) -> Vec<Part> {
    let rows = entries
        .iter()
        .map(|entry| {
            let size = [("iris.csv", IRIS), ("tips.csv", TIPS), ("new.csv", NEW)]
                .iter()
                .find(|(name, _)| name == entry)
                .map_or_else(|| "-".to_owned(), |(_, bytes)| bytes.len().to_string());
            format!(
                "<a href=\"{entry}\">{entry}</a>{:>40}{size:>20}\r\n",
                "16-Nov-2026 09:00:07"
            )
        })
        .collect::<Vec<_>>()
        .concat();
    page(&format!(
        "<html>\r\n<head><title>Index of {folder}</title></head>\r\n<body>\r\n<h1>Index of {folder}</h1><hr><pre><a href=\"../\">../</a>\r\n{rows}</pre><hr></body>\r\n</html>\r\n"
    ))
}

/// a listing as ncei's apache writes it: a folder's link without its `/`, under its name with
/// one, and the folder above as `..`.
fn ncei(folder: &str, entries: &[&str]) -> Vec<Part> {
    let rows = entries
        .iter()
        .map(|entry| {
            let href = entry.strip_suffix('/').unwrap_or(entry);
            format!(
                "<tr>\n<td><a href=\"{href}\">{entry}</a></td>\n<td align=\"right\">2026-11-16 10:00</td>\n<td align=\"right\">-</td>\n<td> </td>\n</tr>\n"
            )
        })
        .collect::<Vec<_>>()
        .concat();
    page(&format!(
        "<!DOCTYPE html PUBLIC \"-//W3C//DTD HTML 3.2 Final//EN\">\n<html>\n<head>\n<title>Index of {folder}</title>\n</head>\n<body>\n<h1>Index of {folder}</h1>\n<table><tbody>\n<tr>\n<th>Name</th>\n<th>Last modified</th>\n<th>Size</th>\n<th>Description</th>\n</tr>\n<tr>\n<td><a href=\"..\">Parent Directory</a></td>\n<td> </td>\n<td align=\"right\">-</td>\n<td> </td>\n</tr>\n{rows}</tbody></table>\n</body>\n</html>\n"
    ))
}

/// a listing as apache writes it inside a page psl's site titles with nothing, as noaa's psl,
/// gml and star do: known by its link to the folder above, which reads "Parent Directory".
fn psl(folder: &str, entries: &[&str]) -> Vec<Part> {
    let parent = folder
        .trim_end_matches('/')
        .rsplit_once('/')
        .map_or("/", |(above, _)| above);
    let rows = entries
        .iter()
        .map(|entry| format!("<tr><td class=\"indexcolname\"><a href=\"{entry}\">{entry}</a></td><td class=\"indexcolsize\"> 1K</td></tr>\n"))
        .collect::<Vec<_>>()
        .concat();
    page(&format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<title></title>\n</head>\n<body>\n<ul class=\"nav\"><li><a href=\"/about/\">About Us</a></li></ul>\n<table id=\"indexlist\">\n<tr class=\"indexhead\"><th class=\"indexcolname\"><a href=\"?C=N;O=D\">Name</a></th><th class=\"indexcolsize\"><a href=\"?C=S;O=A\">Size</a></th></tr>\n<tr><td class=\"indexcolname\"><a href=\"{parent}/\">Parent Directory</a></td><td class=\"indexcolsize\">  - </td></tr>\n{rows}</table>\n</body>\n</html>\n"
    ))
}

/// a listing as `rclone serve http` writes it, titled with the folder's path.
fn rclone(folder: &str, entries: &[&str]) -> Vec<Part> {
    let rows = entries
        .iter()
        .map(|entry| format!("<tr class=\"file\"><td class=\"name\"><a href=\"./{entry}\">{entry}</a></td><td class=\"size\">&mdash;</td></tr>\n"))
        .collect::<Vec<_>>()
        .concat();
    page(&format!(
        "<!DOCTYPE html>\n<html>\n<head>\n<title>{}</title>\n</head>\n<body>\n<h1><a href=\"../\">/</a></h1>\n<table>\n<tr><td class=\"name\"><a href=\"..\">Go up</a></td></tr>\n{rows}</table>\n</body>\n</html>\n",
        folder.trim_end_matches('/')
    ))
}

/// a web page answered with 200.
fn page(html: &str) -> Vec<Part> {
    reply(
        "200 OK",
        &[
            ("Content-Type", "text/html; charset=utf-8"),
            ("Content-Length", &html.len().to_string()),
        ],
        html.as_bytes(),
    )
}

/// a server with iris.csv in its `/data` folder and tips.csv in `/data/sub`, each folder listed
/// as `lister` writes it.
fn served(lister: Lister) -> Result<Routes> {
    Routes::start(vec![
        ("/data/".to_owned(), lister("/data/", &["iris.csv", "sub/"])),
        ("/data/sub/".to_owned(), lister("/data/sub/", &["tips.csv"])),
        ("/data/iris.csv".to_owned(), file(IRIS, "\"i1\"")),
        ("/data/sub/tips.csv".to_owned(), file(TIPS, "\"t1\"")),
    ])
}

/// the data.lock line that pins `path`, at `url`, to `bytes`.
fn pin(path: &str, bytes: &[u8], url: &str) -> String {
    format!(
        "{{ path = \"{path}\", size = {}, sha256 = \"{}\", url = \"{url}\" }},",
        bytes.len(),
        sha256_hex(bytes)
    )
}

/// `add` of a folder's link pins every file below it, those of its folders at their paths in
/// it, each by its SHA-256, as one dataset named after the folder; a clone syncs from data.lock
/// alone and never asks for a listing.
#[test]
fn a_listed_folder_is_one_dataset_of_every_file_below_it() -> Result {
    for lister in [python as Lister, nginx, ncei, psl, rclone] {
        let sandbox = Sandbox::new()?;
        let server = served(lister)?;
        let folder = format!("{}/data/", server.url);
        let output = sandbox.run(&["--json", "add", &folder])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        assert_eq!(
            json(&output)?["result"]["datasets"],
            serde_json::json!(["data"])
        );
        let work = sandbox.work();
        assert_eq!(
            fs::read_to_string(work.join("data.toml"))?,
            format!("[datasets.data]\nref = \"{folder}\"\n")
        );
        let lock = fs::read_to_string(work.join("data.lock"))?;
        assert_eq!(
            pinned(&lock, "data"),
            [
                pin("iris.csv", IRIS, &format!("{folder}iris.csv")),
                pin("sub/tips.csv", TIPS, &format!("{folder}sub/tips.csv")),
            ]
        );
        assert_eq!(fs::read(work.join("data/data/iris.csv"))?, IRIS);
        assert_eq!(fs::read(work.join("data/data/sub/tips.csv"))?, TIPS);

        let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
        sandbox.allow(&server.url);
        let asked = server.targets().len();
        let synced = sandbox
            .command_in(&clone, &["sync"])
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
        assert_eq!(fs::read(clone.join("data/data/iris.csv"))?, IRIS);
        assert_eq!(fs::read(clone.join("data/data/sub/tips.csv"))?, TIPS);
        let later = &server.targets()[asked..];
        assert!(
            !later.iter().any(|target| target.ends_with('/')),
            "a clone needs data.lock alone: {later:?}"
        );
    }
    Ok(())
}

/// `--select` narrows a folder as it narrows any dataset, and only the folders a pattern can
/// reach are listed: a pattern of top files asks for no listing of a folder inside. `get` of the
/// link fetches every file into one folder named after it.
#[test]
fn select_lists_only_the_folders_a_pattern_reaches() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(nginx)?;
    let folder = format!("{}/data/", server.url);
    let narrowed = sandbox.run(&["add", &folder, "--as", "tips", "--select", "sub/*"])?;
    assert_eq!(
        narrowed.status.code(),
        Some(0),
        "{}",
        text(&narrowed.stderr)?
    );
    let before = server.targets().len();
    let top = sandbox.run(&["add", &folder, "--as", "top", "--select", "*.csv"])?;
    assert_eq!(top.status.code(), Some(0), "{}", text(&top.stderr)?);
    let asked = &server.targets()[before..];
    assert!(asked.contains(&"/data/".to_owned()), "{asked:?}");
    assert!(
        !asked.contains(&"/data/sub/".to_owned()),
        "no folder inside is listed: {asked:?}"
    );
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_eq!(
        pinned(&lock, "tips"),
        [pin("sub/tips.csv", TIPS, &format!("{folder}sub/tips.csv"))]
    );
    assert_eq!(
        pinned(&lock, "top"),
        [pin("iris.csv", IRIS, &format!("{folder}iris.csv"))]
    );

    let elsewhere = Sandbox::new()?;
    let got = elsewhere.run(&["get", &folder])?;
    assert_eq!(got.status.code(), Some(0), "{}", text(&got.stderr)?);
    let out = elsewhere.work().join("data");
    assert_eq!(fs::read(out.join("iris.csv"))?, IRIS);
    assert_eq!(fs::read(out.join("sub").join("tips.csv"))?, TIPS);
    Ok(())
}

/// a listing's links to the folder above, a column's sort link with a query, a link to another
/// host and one that climbs out of the folder once percent-decoded are never asked for. the add
/// pins the files below the folder and says which links it left out; the parent and sort links
/// every listing has go unsaid.
#[test]
fn links_that_leave_the_folder_are_never_followed() -> Result {
    let sandbox = Sandbox::new()?;
    let other = Routes::start(vec![("/x.csv".to_owned(), file(TIPS, "\"x1\""))])?;
    let away = format!("{}/x.csv", other.url);
    let server = Routes::start(vec![
        (
            "/data/".to_owned(),
            nginx(
                "/data/",
                &["?C=M;O=A", "/", &away, "%2e%2e/secret.csv", "iris.csv"],
            ),
        ),
        ("/data/iris.csv".to_owned(), file(IRIS, "\"i1\"")),
        ("/secret.csv".to_owned(), file(TIPS, "\"s1\"")),
        ("/data/?C=M;O=A".to_owned(), nginx("/data/", &["iris.csv"])),
    ])?;
    let folder = format!("{}/data/", server.url);
    let output = sandbox.run(&["--json", "add", &folder])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_eq!(
        pinned(&lock, "data"),
        [pin("iris.csv", IRIS, &format!("{folder}iris.csv"))]
    );
    let targets = server.targets();
    for never in ["/", "/secret.csv", "/data/?C=M;O=A"] {
        assert!(!targets.contains(&never.to_owned()), "{never}: {targets:?}");
    }
    assert_eq!(other.targets(), Vec::<String>::new());
    assert_eq!(
        warning(&json(&output)?, "source.partial")["message"],
        format!(
            "the listing at {folder} links {away} and {}/secret.csv outside its folder, which fetchloom leaves out",
            server.url
        )
    );
    Ok(())
}

/// a page at a link ending in / that no server made as a listing, as a lab's own page, is
/// refused before any download, as a web page where a file was expected always is; `--web-page`
/// keeps the page itself.
#[test]
fn a_page_that_is_no_listing_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![(
        "/lab/".to_owned(),
        page(
            "<!DOCTYPE html><html><head><title>Our lab</title></head><body><ul><li><a href=\"data.csv\">our data</a></li></ul></body></html>",
        ),
    )])?;
    let link = format!("{}/lab/", server.url);
    let output = sandbox.run(&["--json", "add", &link])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "source.invalid", "{document}");
    assert_eq!(
        error["message"],
        format!("{link} answered with a web page, not a file or a folder listing fetchloom reads")
    );
    assert_eq!(
        error["help"],
        "fetchloom reads the folder listings apache, nginx, lighttpd, caddy, rclone and python's http.server make, and this page is none of them. open it in a browser, copy the link of each file you want, and give fetchloom those links. if the page itself is the data, add --web-page"
    );
    assert_eq!(server.targets(), ["/lab/"]);
    assert!(!sandbox.work().join("data.toml").exists());

    let kept = sandbox.run(&["add", &link, "--web-page"])?;
    assert_eq!(kept.status.code(), Some(0), "{}", text(&kept.stderr)?);
    let toml = fs::read_to_string(sandbox.work().join("data.toml"))?;
    assert!(toml.contains("web_page = true"), "{toml}");
    Ok(())
}

/// a sync keeps the files data.lock pins without asking for the listing, and `lock --upgrade`
/// reads it again, pinning a file the folder gained since.
#[test]
fn lock_upgrade_pins_a_file_the_folder_gained() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(python)?;
    let folder = format!("{}/data/", server.url);
    let added = sandbox.run(&["add", &folder])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    server.change(vec![
        (
            "/data/".to_owned(),
            python("/data/", &["iris.csv", "new.csv", "sub/"]),
        ),
        ("/data/new.csv".to_owned(), file(NEW, "\"n1\"")),
    ]);
    sandbox.allow(&server.url);
    let work = sandbox.work();
    let asked = server.targets().len();
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert!(
        server.targets()[asked..].is_empty(),
        "{:?}",
        server.targets()
    );
    assert!(!work.join("data/data/new.csv").exists());

    let upgraded = sandbox.run(&["lock", "--upgrade"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert_eq!(
        pinned(&lock, "data"),
        [
            pin("iris.csv", IRIS, &format!("{folder}iris.csv")),
            pin("new.csv", NEW, &format!("{folder}new.csv")),
            pin("sub/tips.csv", TIPS, &format!("{folder}sub/tips.csv")),
        ]
    );
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(work.join("data/data/new.csv"))?, NEW);
    Ok(())
}

/// a dry run of an add tells the files the add then pins and places, and fails where the add
/// fails, in its words: at a page that is no listing, and at a folder's link without its `/`,
/// which the server sends on to the folder, where the help offers the link with its `/`.
#[test]
fn a_dry_run_of_a_folder_tells_what_the_add_does() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(python)?;
    server.change(vec![
        (
            "/lab/".to_owned(),
            page("<!DOCTYPE html><html><head><title>Our lab</title></head><body></body></html>"),
        ),
        (
            "/data".to_owned(),
            reply(
                "301 Moved Permanently",
                &[("Location", "/data/"), ("Content-Length", "0")],
                b"",
            ),
        ),
    ]);
    let folder = format!("{}/data/", server.url);
    let dry = sandbox.run(&["--json", "add", &folder, "--dry-run"])?;
    assert_eq!(dry.status.code(), Some(0), "{}", text(&dry.stdout)?);
    let planned = json(&dry)?["result"]["changes"]["place"].clone();
    assert_eq!(
        planned,
        serde_json::json!(["data/data/iris.csv", "data/data/sub/tips.csv"])
    );
    let added = sandbox.run(&["add", &folder])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    for path in planned.as_array().into_iter().flatten() {
        let path = path.as_str().unwrap_or_default();
        assert!(sandbox.work().join(path).is_file(), "{path}");
    }
    let offered = format!(
        "if it is a folder listing, use its link with a / at the end, {folder}, which fetchloom reads as one dataset."
    );
    for (link, offers) in [
        (format!("{}/lab/", server.url), false),
        (format!("{}/data", server.url), true),
    ] {
        let dry = sandbox.run(&["--json", "add", &link, "--as", "page", "--dry-run"])?;
        let run = sandbox.run(&["--json", "add", &link, "--as", "page"])?;
        assert_eq!(run.status.code(), Some(10), "{}", text(&run.stdout)?);
        assert_eq!(dry.status.code(), run.status.code(), "{link}");
        let (dry, run) = (json(&dry)?, json(&run)?);
        let (dry, run) = (&dry["errors"][0], &run["errors"][0]);
        assert_eq!(dry["message"], run["message"], "{link}");
        assert_eq!(dry["help"], run["help"], "{link}");
        let help = run["help"].as_str().unwrap_or_default();
        assert_eq!(help.starts_with(&offered), offers, "{help}");
    }
    Ok(())
}

/// ncei links each folder inside a listing without its `/`, under its name with one, and the
/// folder above as `..`: the dry run tells the files of the folders inside as the add then pins
/// them, neither asks for a folder as a file, and neither warns of the folder above.
#[test]
fn a_folder_linked_without_its_slash_is_listed_as_ncei_writes_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(ncei)?;
    server.change(vec![(
        "/data/sub".to_owned(),
        reply(
            "301 Moved Permanently",
            &[("Location", "/data/sub/"), ("Content-Length", "0")],
            b"",
        ),
    )]);
    let folder = format!("{}/data/", server.url);
    let dry = sandbox.run(&["--json", "add", &folder, "--dry-run"])?;
    assert_eq!(dry.status.code(), Some(0), "{}", text(&dry.stdout)?);
    let dry = json(&dry)?;
    assert_eq!(
        dry["result"]["changes"]["place"],
        serde_json::json!(["data/data/iris.csv", "data/data/sub/tips.csv"])
    );
    let added = sandbox.run(&["--json", "add", &folder])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stdout)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_eq!(
        pinned(&lock, "data"),
        [
            pin("iris.csv", IRIS, &format!("{folder}iris.csv")),
            pin("sub/tips.csv", TIPS, &format!("{folder}sub/tips.csv")),
        ]
    );
    for document in [&dry, &json(&added)?] {
        assert_eq!(
            warning(document, "source.partial"),
            &serde_json::Value::Null
        );
    }
    let targets = server.targets();
    assert!(!targets.contains(&"/data/sub".to_owned()), "{targets:?}");
    Ok(())
}

/// a listing that states each file's size in bytes, as nginx's does, sizes the folder before
/// anything downloads: the dry run tells each file's size and their sum, past `ask_above` the
/// add asks before its first download, and the add pins the files at those sizes.
#[test]
fn a_listing_that_states_sizes_sizes_the_folder_before_it_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(nginx_sized)?;
    let folder = format!("{}/data/", server.url);
    let dry = sandbox.run(&["--json", "add", &folder, "--dry-run"])?;
    assert_eq!(dry.status.code(), Some(0), "{}", text(&dry.stdout)?);
    let changes = &json(&dry)?["result"]["changes"];
    assert_eq!(
        changes["download"],
        serde_json::json!([
            {"path": "data/iris.csv", "size": IRIS.len()},
            {"path": "data/sub/tips.csv", "size": TIPS.len()},
        ])
    );
    assert_eq!(changes["to_download"]["bytes"], IRIS.len() + TIPS.len());
    assert_eq!(changes["to_download"]["sizes_unknown"], 0);

    let before = server.targets().len();
    let asked = sandbox
        .command(&["--json", "add", &folder])
        .env("FETCHLOOM_ASK_ABOVE", "30B")
        .output()?;
    assert_eq!(asked.status.code(), Some(1), "{}", text(&asked.stdout)?);
    assert_eq!(
        json(&asked)?["errors"][0]["message"],
        "downloading 2 files needs 40 B, and fetchloom cannot ask you here"
    );
    let later = &server.targets()[before..];
    assert!(
        later.iter().all(|target| target.ends_with('/')),
        "nothing downloads before the question: {later:?}"
    );
    assert!(!sandbox.work().join("data.lock").exists());

    let added = sandbox.run(&["add", &folder])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_eq!(
        pinned(&lock, "data"),
        [
            pin("iris.csv", IRIS, &format!("{folder}iris.csv")),
            pin("sub/tips.csv", TIPS, &format!("{folder}sub/tips.csv")),
        ]
    );
    Ok(())
}

/// files whose sizes only their server states, as a listing that gives none leaves them, count
/// together against `ask_above` and `cache_budget`, as a sync from data.lock counts the sizes it
/// knows: three files of 1 MiB at 2 MiB stop an add no one can answer, naming them all.
#[test]
fn sizes_only_a_server_states_count_together() -> Result {
    let big = vec![b'x'; 1 << 20];
    let names = ["a.bin", "b.bin", "c.bin"];
    let mut routes = vec![("/big/".to_owned(), python("/big/", &names))];
    for (index, name) in names.iter().enumerate() {
        routes.push((format!("/big/{name}"), file(&big, &format!("\"{index}\""))));
    }
    let server = Routes::start(routes)?;
    let folder = format!("{}/big/", server.url);
    for (setting, message) in [
        (
            "FETCHLOOM_ASK_ABOVE",
            "downloading 3 files needs more than 2.0 MiB, and fetchloom cannot ask you here",
        ),
        (
            "FETCHLOOM_CACHE_BUDGET",
            "downloading 3 files would bring the cache to 3.0 MiB, past the 2.0 MiB cache_budget",
        ),
    ] {
        let sandbox = Sandbox::new()?;
        let added = sandbox
            .command(&["--json", "add", &folder])
            .env(setting, "2M")
            .output()?;
        assert_eq!(added.status.code(), Some(1), "{}", text(&added.stdout)?);
        assert_eq!(json(&added)?["errors"][0]["message"], message, "{setting}");
        assert!(!sandbox.work().join("data.lock").exists(), "{setting}");
    }
    Ok(())
}

/// a machine that only synced a folder keeps what each file's server sent, as the machine that
/// added it does, so its first `lock --upgrade` asks each server whether its file changed, and a
/// server's "not modified" downloads nothing again.
#[test]
fn a_synced_folder_upgrades_by_asking_whether_its_files_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(python)?;
    let folder = format!("{}/data/", server.url);
    let added = sandbox.run(&["add", &folder])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
    sandbox.allow(&server.url);
    let run = |args: &[&str]| {
        sandbox
            .command_in(&clone, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let synced = run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);

    let unchanged = |etag: &str| reply("304 Not Modified", &[("ETag", etag)], b"");
    server.change(vec![
        ("/data/iris.csv".to_owned(), unchanged("\"i1\"")),
        ("/data/sub/tips.csv".to_owned(), unchanged("\"t1\"")),
    ]);
    let before = server.requests().len();
    let upgraded = run(&["--json", "lock", "--upgrade"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stdout)?
    );
    assert_eq!(json(&upgraded)?["result"]["lock_changed"], false);
    let asked = &server.requests()[before..];
    for (file, etag) in [
        ("/data/iris.csv", "\"i1\""),
        ("/data/sub/tips.csv", "\"t1\""),
    ] {
        let head = asked
            .iter()
            .find(|head| head.starts_with(&format!("get {file} ")))
            .ok_or_else(|| format!("{file} was not asked: {asked:?}"))?;
        assert!(
            head.contains(&format!("if-none-match: {etag}")),
            "{file} was asked without its etag: {head}"
        );
    }
    Ok(())
}

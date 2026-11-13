//! public S3 and cloud storage buckets, listed and downloaded from a server on this machine that
//! stands in for both services.
#![cfg(debug_assertions)]

use std::fmt::Write as _;
use std::fs;
use std::process::Output;

use hmac::{Hmac, KeyInit, Mac};
use md5::Digest as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, percent_encode};
use sha2::Sha256;

use crate::support::{
    Fetchloom, Part, Result, Routes, Sandbox, Server, delete, file, json, reply, text,
};

const PARTICIPANTS: &[u8] = b"participant_id\tsex\nsub-01\tF\n";
const SCAN: &[u8] = b"onset\tduration\n0\t1\n";
const README: &[u8] = b"a tiny dataset\n";

/// the listing query fetchloom sends S3 for `prefix`, escaped as it escapes it.
pub(crate) fn s3_listing(path: &str, prefix: &str, token: Option<&str>) -> String {
    let mut target = format!(
        "{path}?list-type=2&prefix={}&encoding-type=url",
        prefix.replace('/', "%2F")
    );
    if let Some(token) = token {
        let _ = write!(target, "&continuation-token={token}");
    }
    target
}

/// an S3 listing page of `(key, body)` objects, with each key escaped as `encoding-type=url` asks.
pub(crate) fn s3_page(objects: &[(&str, &[u8])], next: Option<&str>) -> Vec<Part> {
    let stated: Vec<(&str, String, u64)> = objects
        .iter()
        .map(|(key, body)| (*key, md5(body), body.len() as u64))
        .collect();
    s3_stated(&stated, next)
}

/// an S3 listing page of `(key, md5, size)` objects, stating whatever the test asks.
fn s3_stated(objects: &[(&str, String, u64)], next: Option<&str>) -> Vec<Part> {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>b</Name><EncodingType>url</EncodingType>",
    );
    match next {
        Some(next) => {
            let _ = write!(
                xml,
                "<IsTruncated>true</IsTruncated><NextContinuationToken>{next}</NextContinuationToken>"
            );
        }
        None => xml.push_str("<IsTruncated>false</IsTruncated>"),
    }
    for (key, md5, size) in objects {
        let _ = write!(
            xml,
            "<Contents><Key>{}</Key><LastModified>2020-08-21T20:24:12.000Z</LastModified><ETag>&quot;{md5}&quot;</ETag><Size>{size}</Size></Contents>",
            key.replace('/', "%2F").replace(' ', "+"),
        );
    }
    xml.push_str("</ListBucketResult>");
    reply(
        "200 OK",
        &[("Content-Type", "application/xml")],
        xml.as_bytes(),
    )
}

pub(crate) fn md5(body: &[u8]) -> String {
    md5::Md5::digest(body)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// a command that sends every S3 and cloud storage request to `routes`, with AWS and google
/// credentials set in its environment, which fetchloom must never send.
fn command(sandbox: &Sandbox, routes: &Routes, args: &[&str]) -> Fetchloom {
    let mut command = sandbox.command(args);
    command
        .env("FETCHLOOM_TEST_BUCKETS", &routes.url)
        .env("AWS_ACCESS_KEY_ID", "AKIAEXAMPLEEXAMPLE")
        .env("AWS_SECRET_ACCESS_KEY", "do-not-send")
        .env("AWS_SESSION_TOKEN", "do-not-send")
        .env("GOOGLE_APPLICATION_CREDENTIALS", "credentials.json");
    command
}

fn assert_anonymous(routes: &Routes) {
    for request in routes.requests() {
        assert!(!request.contains("authorization"), "{request}");
        assert!(!request.contains("x-amz-"), "{request}");
        assert!(!request.contains("do-not-send"), "{request}");
    }
}

/// openneuro's bucket in two pages, its dotted name reached by path, and its objects.
fn openneuro() -> Result<Routes> {
    let base = "/s3.amazonaws.com/openneuro.org/";
    Routes::start(vec![
        (
            s3_listing(base, "ds000001/", None),
            s3_page(
                &[
                    ("ds000001/README", README),
                    ("ds000001/participants.tsv", PARTICIPANTS),
                ],
                Some("next%2Bpage"),
            ),
        ),
        (
            s3_listing(base, "ds000001/", Some("next%252Bpage")),
            s3_page(
                &[
                    ("ds000001/sub-01/", b""),
                    ("ds000001/sub-01/func events.tsv", SCAN),
                ],
                None,
            ),
        ),
        (
            format!("{base}ds000001/README"),
            file(README, &format!("\"{}\"", md5(README))),
        ),
        (
            format!("{base}ds000001/participants.tsv"),
            file(PARTICIPANTS, &format!("\"{}\"", md5(PARTICIPANTS))),
        ),
        (
            format!("{base}ds000001/sub-01/func%20events.tsv"),
            file(SCAN, &format!("\"{}\"", md5(SCAN))),
        ),
    ])
}

/// the whole repository's file count must never read as the files added.
#[test]
fn add_counts_the_files_it_keeps_when_it_resolves() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    let added = [
        "add",
        "s3://openneuro.org/ds000001/",
        "--select",
        "participants.tsv",
    ];
    let output = command(&sandbox, &routes, &added).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Resolved s3://openneuro.org/ds000001/ · 1 of 3 files · 28 B\n"),
        "{stderr}"
    );
    Ok(())
}

/// a public bucket never sees a key, even one `s3_buckets` lists while keys are set.
#[test]
fn an_s3_prefix_is_added_pinned_and_synced_anonymously() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    let output = command(
        &sandbox,
        &routes,
        &["add", "s3://openneuro.org/ds000001/", "--exclude", "README"],
    )
    .env("FETCHLOOM_S3_BUCKETS", "s3://openneuro.org")
    .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.starts_with("[datasets.ds000001]\nref = \"s3://openneuro.org/ds000001/\"\n"),
        "{manifest}"
    );
    let folder = work.join("data").join("ds000001");
    assert_eq!(fs::read(folder.join("participants.tsv"))?, PARTICIPANTS);
    assert_eq!(
        fs::read(folder.join("sub-01").join("func events.tsv"))?,
        SCAN
    );
    assert!(!folder.join("README").exists());
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("resolved = \"s3://openneuro.org/ds000001/\""),
        "{lock}"
    );
    assert!(
        lock.contains(&format!(
            "md5 = \"{}\", url = \"https://s3.amazonaws.com/openneuro.org/ds000001/participants.tsv\"",
            md5(PARTICIPANTS)
        )),
        "{lock}"
    );
    assert!(
        lock.contains(
            "url = \"https://s3.amazonaws.com/openneuro.org/ds000001/sub-01/func%20events.tsv\""
        ),
        "{lock}"
    );
    let targets = routes.targets();
    assert!(
        !targets.iter().any(|target| target.ends_with("/README")),
        "{targets:?}"
    );
    assert_anonymous(&routes);

    let listed = command(&sandbox, &routes, &["list"]).output()?;
    let stdout = text(&listed.stdout)?;
    assert!(
        stdout.starts_with("ds000001  s3://openneuro.org/ds000001/"),
        "{stdout}"
    );

    // placed files are read-only, which exfat and fat32 refuse to delete as they are
    delete(&folder.join("participants.tsv"))?;
    delete(&folder.join("sub-01").join("func events.tsv"))?;
    let synced = command(&sandbox, &routes, &["sync"]).output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(folder.join("participants.tsv"))?, PARTICIPANTS);
    let check = command(&sandbox, &routes, &["lock", "--check"]).output()?;
    assert_eq!(check.status.code(), Some(0), "{}", text(&check.stderr)?);
    assert_anonymous(&routes);
    Ok(())
}

#[test]
fn a_pasted_https_prefix_gets_a_folder_named_after_its_last_segment() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    let output = command(
        &sandbox,
        &routes,
        &[
            "--json",
            "get",
            "https://s3.amazonaws.com/openneuro.org/ds000001/",
            "--select",
            "**/*.tsv",
        ],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let folder = sandbox.work().join("ds000001");
    assert_eq!(document["result"]["folder"], folder.display().to_string());
    assert_eq!(fs::read(folder.join("participants.tsv"))?, PARTICIPANTS);
    assert_eq!(
        fs::read(folder.join("sub-01").join("func events.tsv"))?,
        SCAN
    );
    assert!(!folder.join("README").exists());
    assert_anonymous(&routes);
    Ok(())
}

#[test]
fn a_bucket_in_another_region_is_followed_there() -> Result {
    let sandbox = Sandbox::new()?;
    let moved = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>PermanentRedirect</Code><Message>The bucket you are attempting to access must be addressed using the specified endpoint.</Message><Bucket>sentinel-cogs</Bucket></Error>";
    let routes = Routes::start(vec![
        (
            s3_listing("/sentinel-cogs.s3.amazonaws.com/", "tiles/", None),
            reply(
                "301 Moved Permanently",
                &[
                    ("Content-Type", "application/xml"),
                    ("x-amz-bucket-region", "us-west-2"),
                ],
                moved,
            ),
        ),
        (
            s3_listing("/sentinel-cogs.s3.us-west-2.amazonaws.com/", "tiles/", None),
            s3_page(&[("tiles/a.tif", SCAN)], None),
        ),
        (
            "/sentinel-cogs.s3.us-west-2.amazonaws.com/tiles/a.tif".to_owned(),
            file(SCAN, &format!("\"{}\"", md5(SCAN))),
        ),
    ])?;
    let output = command(&sandbox, &routes, &["add", "s3://sentinel-cogs/tiles/"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(
        lock.contains("url = \"https://sentinel-cogs.s3.us-west-2.amazonaws.com/tiles/a.tif\""),
        "{lock}"
    );
    assert_eq!(routes.targets().len(), 3);
    Ok(())
}

#[test]
fn a_gcs_prefix_pins_each_object_generation() -> Result {
    let sandbox = Sandbox::new()?;
    let page = serde_json::json!({
        "items": [
            {"name": "bigquery/us-states/", "size": "0", "md5Hash": "1B2M2Y8AsgTpgAmY7PhCfg==", "generation": "1"},
            {"name": "bigquery/us-states/us-states.csv", "size": PARTICIPANTS.len().to_string(), "generation": "1625246960472434"},
        ]
    });
    let routes = Routes::start(vec![
        (
            "/storage.googleapis.com/storage/v1/b/cloud-samples-data/o?prefix=bigquery%2Fus-states%2F&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken".to_owned(),
            reply("200 OK", &[("Content-Type", "application/json")], &serde_json::to_vec(&page)?),
        ),
        (
            "/storage.googleapis.com/cloud-samples-data/bigquery/us-states/us-states.csv?generation=1625246960472434".to_owned(),
            file(PARTICIPANTS, "\"v1\""),
        ),
    ])?;
    let output = command(
        &sandbox,
        &routes,
        &[
            "add",
            "https://storage.googleapis.com/cloud-samples-data/bigquery/us-states/",
        ],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains(
            "[datasets.us-states]\nref = \"gs://cloud-samples-data/bigquery/us-states/\""
        ),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("url = \"https://storage.googleapis.com/cloud-samples-data/bigquery/us-states/us-states.csv?generation=1625246960472434\""),
        "{lock}"
    );
    assert_eq!(
        fs::read(work.join("data").join("us-states").join("us-states.csv"))?,
        PARTICIPANTS
    );
    assert_anonymous(&routes);
    Ok(())
}

#[test]
fn nonempty_folder_objects_are_not_ignored() -> Result {
    let sandbox = Sandbox::new()?;
    let base = "/marker-bucket.s3.amazonaws.com/";
    let listing = s3_listing(base, "data/", None);
    let routes = Routes::start(vec![
        (
            listing.clone(),
            s3_page(
                &[
                    ("data/keep.csv", PARTICIPANTS),
                    ("data/payload/", b"payload"),
                ],
                None,
            ),
        ),
        (
            format!("{base}data/keep.csv"),
            file(PARTICIPANTS, &format!("\"{}\"", md5(PARTICIPANTS))),
        ),
    ])?;
    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "s3://marker-bucket/data/"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(
        document["errors"][0]["kind"], "source.invalid",
        "{document}"
    );
    assert!(
        document["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("data/payload/")
    );
    assert!(!sandbox.work().join("data.toml").exists());
    assert!(!sandbox.work().join("data.lock").exists());
    assert!(!sandbox.work().join("data").exists());
    assert_eq!(routes.targets(), [listing]);
    Ok(())
}

#[test]
fn soap_objects_download_through_path_style_urls() -> Result {
    let sandbox = Sandbox::new()?;
    let listing = s3_listing("/soap-bucket.s3.amazonaws.com/", "", None);
    let object = "/s3.amazonaws.com/soap-bucket/soap".to_owned();
    let routes = Routes::start(vec![
        (listing.clone(), s3_page(&[("soap", SCAN)], None)),
        (object.clone(), file(SCAN, &format!("\"{}\"", md5(SCAN)))),
    ])?;
    let output = command(
        &sandbox,
        &routes,
        &["add", "s3://soap-bucket/", "--as", "soap-data"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(sandbox.work().join("data/soap-data/soap"))?, SCAN);
    assert_eq!(routes.targets(), [listing, object]);
    assert_anonymous(&routes);
    Ok(())
}

#[test]
fn missing_gcs_generations_are_refused_before_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let listing = "/storage.googleapis.com/storage/v1/b/generation-bucket/o?prefix=data%2F&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken";
    let page = serde_json::json!({"items": [
        {"name": "data/keep.csv", "size": PARTICIPANTS.len().to_string()},
    ]});
    let routes = Routes::start(vec![
        (
            listing.to_owned(),
            reply(
                "200 OK",
                &[("Content-Type", "application/json")],
                &serde_json::to_vec(&page)?,
            ),
        ),
        (
            "/storage.googleapis.com/generation-bucket/data/keep.csv".to_owned(),
            file(PARTICIPANTS, "\"v1\""),
        ),
    ])?;
    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "gs://generation-bucket/data/"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(
        document["errors"][0]["kind"], "source.invalid",
        "{document}"
    );
    assert!(
        document["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("generation")
    );
    assert!(!sandbox.work().join("data.toml").exists());
    assert!(!sandbox.work().join("data.lock").exists());
    assert!(!sandbox.work().join("data").exists());
    assert_eq!(routes.targets(), [listing]);
    Ok(())
}

/// S3 refusing a request without a key, as S3 words it.
const DENIED: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";
/// the secret beside the keys signed tests set, which no request, file or output may hold.
const SECRET: &str = "secret-that-never-leaves-this-machine";
/// a session token, lowercase like the heads the test server keeps.
const SESSION: &str = "session-sent-with-each-signed-request";

fn refusal() -> Vec<Part> {
    reply(
        "403 Forbidden",
        &[
            ("Content-Type", "application/xml"),
            ("Content-Length", &DENIED.len().to_string()),
        ],
        DENIED,
    )
}

/// what escapes in a signed path or query: everything but the unreserved characters.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

fn escaped(text: &str) -> String {
    let bytes: Vec<u8> = percent_decode_str(text).collect();
    percent_encode(&bytes, UNRESERVED).to_string()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

fn mac(key: &[u8], text: &str) -> Result<Vec<u8>> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).map_err(|_| "no key")?;
    mac.update(text.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

/// checks a request's signature as S3 does and returns the key id and region it was signed with.
/// the test servers lowercase heads, so signed tests use lowercase keys, sessions and versions,
/// and the date's `t` and `z` and the `versionId` name are restored as sent.
fn signed_with(head: &str, secret: &str) -> Result<(String, String)> {
    let mut lines = head.lines();
    let target = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .ok_or("no request line")?
        .replace("versionid=", "versionId=");
    let headers: Vec<(&str, &str)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim(), value.trim()))
        .collect();
    let header = |name: &str| -> Result<String> {
        let value = headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
            .ok_or_else(|| format!("no {name} in {head}"))?;
        Ok(if name == "x-amz-date" {
            value.to_ascii_uppercase()
        } else {
            value
        })
    };
    let authorization = header("authorization")?;
    let rest = authorization
        .strip_prefix("aws4-hmac-sha256 credential=")
        .ok_or("not a signature")?;
    let (credential, rest) = rest
        .split_once(", signedheaders=")
        .ok_or("no signed headers")?;
    let (names, signature) = rest.split_once(", signature=").ok_or("no signature")?;
    let scope: Vec<&str> = credential.splitn(2, '/').collect();
    let (key, scope) = (scope[0], scope.get(1).ok_or("no scope")?);
    let parts: Vec<&str> = scope.split('/').collect();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let path: Vec<String> = path.split('/').map(escaped).collect();
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (escaped(name), escaped(value))
        })
        .collect();
    pairs.sort();
    let query: Vec<String> = pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    let mut canonical = format!("GET\n{}\n{}\n", path.join("/"), query.join("&"));
    for name in names.split(';') {
        let _ = writeln!(canonical, "{name}:{}", header(name)?);
    }
    let _ = write!(canonical, "\n{names}\n{}", header("x-amz-content-sha256")?);
    let date = header("x-amz-date")?;
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{}\n{}",
        scope,
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let mut signing = format!("AWS4{secret}").into_bytes();
    for part in &parts {
        signing = mac(&signing, part)?;
    }
    let expected = hex(&mac(&signing, &to_sign)?);
    if expected != signature {
        return Err(format!("the signature does not match:\n{canonical}\n{head}").into());
    }
    Ok((key.to_owned(), parts[1].to_owned()))
}

/// a command for a bucket test that keeps the keys it is given, with the signed tests' secret.
fn keyed_command(sandbox: &Sandbox, routes: &str, args: &[&str]) -> Fetchloom {
    let mut command = sandbox.command(args);
    command
        .env("FETCHLOOM_TEST_BUCKETS", routes)
        .env("AWS_ACCESS_KEY_ID", "akiaenvironmentkey")
        .env("AWS_SECRET_ACCESS_KEY", SECRET)
        .env("AWS_SESSION_TOKEN", SESSION);
    command
}

/// every file a project and the cache kept, which no secret may reach.
fn kept_files(folder: &std::path::Path, found: &mut Vec<(String, Vec<u8>)>) -> Result {
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path.is_dir() {
            kept_files(&path, found)?;
        } else {
            found.push((path.display().to_string(), fs::read(&path)?));
        }
    }
    Ok(())
}

fn assert_no_secret(surfaces: &[(String, Vec<u8>)]) {
    for (name, bytes) in surfaces {
        let text = String::from_utf8_lossy(bytes);
        for secret in [SECRET, SESSION] {
            assert!(!text.contains(secret), "{name} holds {secret}: {text}");
        }
    }
}

/// the object of a bucket that keeps versions, at its version `3hl4kqtjlcpx+r/1`.
fn versioned() -> Vec<Part> {
    reply(
        "200 OK",
        &[
            ("Content-Length", &PARTICIPANTS.len().to_string()),
            ("ETag", "\"v1\""),
            ("x-amz-version-id", "3hl4kqtjlcpx+r/1"),
        ],
        PARTICIPANTS,
    )
}

/// a teammate's `sync --locked` of the project in the work folder, with keys in an AWS profile
/// and a cache of their own.
fn teammate_sync(sandbox: &Sandbox, routes: &str) -> Result<(std::path::PathBuf, Output)> {
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let credentials = sandbox.folder("aws")?.join("credentials");
    fs::write(
        &credentials,
        "[lab]\naws_access_key_id = akiateammatekey\naws_secret_access_key = teammate-secret\n",
    )?;
    let output = sandbox
        .command_in(&clone, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_TEST_BUCKETS", routes)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .env("FETCHLOOM_S3_BUCKETS", "private-lab")
        .env("AWS_PROFILE", "lab")
        .env("AWS_SHARED_CREDENTIALS_FILE", &credentials)
        .output()?;
    Ok((clone, output))
}

/// the listing is refused without a key and asked again signed, the object is signed from the
/// start and pinned at its version, a teammate's profile signs too, and no secret leaks.
#[test]
fn a_listed_private_bucket_is_listed_and_downloaded_with_a_key() -> Result {
    let sandbox = Sandbox::new()?;
    let object = (
        "/private-lab.s3.us-east-1.amazonaws.com/raw/a.csv",
        PARTICIPANTS,
    );
    let server = Server::start(
        "/",
        vec![
            refusal(),
            s3_page(&[("raw/a.csv", PARTICIPANTS)], None),
            versioned(),
            refusal(),
            versioned(),
        ],
    )?;
    let url = server.url.clone();
    let output = keyed_command(
        &sandbox,
        &url,
        &["-vv", "add", "s3://private-lab/raw/", "--as", "raw"],
    )
    .env("FETCHLOOM_S3_BUCKETS", "s3://private-lab")
    .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let heads = server.requests();
    assert_eq!(heads.len(), 3, "{heads:?}");
    assert!(!heads[0].contains("authorization"), "{}", heads[0]);
    assert!(
        heads[0].starts_with("get /private-lab.s3.amazonaws.com/?list-type=2&prefix=raw%2f"),
        "{}",
        heads[0]
    );
    for head in &heads[1..] {
        let (key, region) = signed_with(head, SECRET)?;
        assert_eq!(
            (key.as_str(), region.as_str()),
            ("akiaenvironmentkey", "us-east-1")
        );
        assert!(
            head.contains(&format!("x-amz-security-token: {SESSION}")),
            "{head}"
        );
    }
    assert!(
        heads[2].starts_with(&format!("get {}", object.0)),
        "{}",
        heads[2]
    );
    let work = sandbox.work();
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(
            "url = \"https://private-lab.s3.us-east-1.amazonaws.com/raw/a.csv?versionId=3hl4kqtjlcpx%2Br%2F1\""
        ),
        "{lock}"
    );
    assert!(
        !lock.contains("md5"),
        "a signed listing states no md5: {lock}"
    );
    assert_eq!(
        fs::read(work.join("data").join("raw").join("a.csv"))?,
        PARTICIPANTS
    );
    let mut surfaces = vec![
        ("stdout".to_owned(), output.stdout),
        ("stderr".to_owned(), output.stderr),
    ];
    kept_files(&work, &mut surfaces)?;
    kept_files(&sandbox.cache(), &mut surfaces)?;
    let (clone, synced) = teammate_sync(&sandbox, &url)?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let heads = server.requests();
    assert_eq!(heads.len(), 5, "{heads:?}");
    assert!(!heads[3].contains("authorization"), "{}", heads[3]);
    assert!(
        heads[4].starts_with(&format!("get {}?versionid=3hl4kqtjlcpx%2br%2f1 ", object.0)),
        "{}",
        heads[4]
    );
    assert_eq!(
        signed_with(&heads[4], "teammate-secret")?.0,
        "akiateammatekey"
    );
    assert_eq!(fs::read(clone.join("data/raw/a.csv"))?, PARTICIPANTS);
    surfaces.push(("sync json".to_owned(), synced.stdout));
    assert_no_secret(&surfaces);
    for head in &heads {
        assert!(
            !head.contains(SECRET) && !head.contains("teammate-secret"),
            "{head}"
        );
    }
    Ok(())
}

#[test]
fn a_listed_bucket_without_keys_says_which_keys_to_set() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(
        s3_listing("/private-lab.s3.amazonaws.com/", "", None),
        refusal(),
    )])?;
    let output = sandbox
        .command(&["--json", "add", "s3://private-lab/"])
        .env("FETCHLOOM_TEST_BUCKETS", &routes.url)
        .env("FETCHLOOM_S3_BUCKETS", "private-lab")
        .output()?;
    assert_eq!(output.status.code(), Some(40));
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "auth.required", "{document}");
    assert_eq!(
        error["message"],
        "s3://private-lab/ is private: S3 refused to list it without a key, and no AWS keys were found"
    );
    assert_eq!(
        error["help"],
        "set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or AWS_PROFILE to a profile with keys in ~/.aws/credentials, a credential_process, or sso or a role through the AWS CLI. see https://nikhil-yn-aix.github.io/fetchloom/user/sources/buckets.html#private-s3-buckets"
    );
    assert_eq!(routes.targets().len(), 1);
    assert_anonymous(&routes);
    Ok(())
}

#[test]
fn a_bucket_nobody_listed_never_gets_a_key() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(
        s3_listing("/private-lab.s3.amazonaws.com/", "", None),
        refusal(),
    )])?;
    let output = command(&sandbox, &routes, &["--json", "add", "s3://private-lab/"])
        .env("FETCHLOOM_S3_BUCKETS", "s3://another-lab")
        .output()?;
    assert_eq!(output.status.code(), Some(40));
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "auth.required", "{document}");
    assert_eq!(
        error["message"],
        "s3://private-lab/ is private: S3 refused to list it without a key"
    );
    assert_eq!(
        error["help"],
        "if you trust s3://private-lab with your AWS keys, add it to s3_buckets in the settings file or FETCHLOOM_S3_BUCKETS"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    assert_eq!(routes.targets().len(), 1);
    assert_anonymous(&routes);
    Ok(())
}

/// a local bucket a cloned data.toml names is reached only as an allowed origin, and with a key
/// only if the person lists it and their AWS settings send keys to that server.
#[test]
fn a_cloned_project_cannot_aim_keys_at_a_server_of_its_own() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(s3_listing("/scans/", "raw/", None), refusal())])?;
    let reference = format!("s3:{}/scans/raw/", routes.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.raw]\nref = \"{reference}\"\n"),
    )?;
    let run = |listed: &str| {
        command(&sandbox, &routes, &["--json", "sync"])
            .env("FETCHLOOM_S3_BUCKETS", listed)
            .output()
    };
    let refused = run("")?;
    assert_eq!(
        refused.status.code(),
        Some(20),
        "{}",
        text(&refused.stdout)?
    );
    assert_eq!(json(&refused)?["errors"][0]["kind"], "network.refused");
    assert_eq!(routes.targets().len(), 0);
    sandbox.allow(&routes.url);
    let unlisted = json(&run("")?)?;
    assert_eq!(unlisted["errors"][0]["kind"], "auth.required", "{unlisted}");
    assert_eq!(
        unlisted["errors"][0]["message"],
        format!(
            "{reference} is private: {} refused to list it without a key",
            routes.url.trim_start_matches("http://")
        )
    );
    let unbound = json(&run(&format!("s3:{}/scans", routes.url))?)?;
    assert_eq!(
        unbound["errors"][0]["message"],
        format!(
            "{reference} is private: {} refused to list it without a key, and no AWS settings send keys to {}",
            routes.url.trim_start_matches("http://"),
            routes.url
        ),
        "{unbound}"
    );
    assert_eq!(routes.targets().len(), 2);
    assert_anonymous(&routes);
    Ok(())
}

/// typed on the command line, a bucket at another service may be on this machine.
#[test]
fn an_endpoint_bucket_signs_with_the_profile_that_names_its_endpoint() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/",
        vec![
            refusal(),
            s3_page(&[("raw/a.csv", PARTICIPANTS)], None),
            file(PARTICIPANTS, "\"v1\""),
        ],
    )?;
    let endpoint = server.url.trim_end_matches('/').to_owned();
    let folder = sandbox.folder("aws")?;
    fs::write(
        folder.join("config"),
        format!("[profile lab]\nendpoint_url = {endpoint}\nregion = lab-1\n"),
    )?;
    fs::write(
        folder.join("credentials"),
        "[default]\naws_access_key_id = akiaawskey\naws_secret_access_key = aws-secret\n[lab]\naws_access_key_id = akialabkey\naws_secret_access_key = lab-secret\n",
    )?;
    let reference = format!("s3:{endpoint}/scans/raw/");
    let output = sandbox
        .command(&["add", &reference, "--as", "raw"])
        .env("FETCHLOOM_S3_BUCKETS", format!("s3:{endpoint}/scans"))
        .env("AWS_CONFIG_FILE", folder.join("config"))
        .env("AWS_SHARED_CREDENTIALS_FILE", folder.join("credentials"))
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let heads = server.requests();
    assert_eq!(heads.len(), 3, "{heads:?}");
    assert!(!heads[0].contains("authorization"), "{}", heads[0]);
    assert!(
        heads[0].starts_with("get /scans/?list-type=2&prefix=raw%2f"),
        "{}",
        heads[0]
    );
    for head in &heads[1..] {
        assert_eq!(
            signed_with(head, "lab-secret")?,
            ("akialabkey".to_owned(), "lab-1".to_owned())
        );
    }
    let work = sandbox.work();
    assert!(
        fs::read_to_string(work.join("data.toml"))?.contains(&format!("ref = \"{reference}\"")),
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!("url = \"{endpoint}/scans/raw/a.csv\"")),
        "{lock}"
    );
    assert!(!lock.contains("md5"), "{lock}");
    assert_eq!(fs::read(work.join("data/raw/a.csv"))?, PARTICIPANTS);
    Ok(())
}

/// what a program that gives keys prints, lowercase like the heads the test server keeps.
#[cfg(unix)]
const PRINTED: &str = r#"{"Version": 1, "AccessKeyId": "akiaprintedkey", "SecretAccessKey": "printed-secret-never-shown", "SessionToken": "printed-session-never-shown", "Expiration": "2999-01-01T00:00:00Z"}"#;
#[cfg(unix)]
const PRINTED_SECRET: &str = "printed-secret-never-shown";
#[cfg(unix)]
const PRINTED_SESSION: &str = "printed-session-never-shown";

/// writes a program at `path` that runs `script` in sh, as a `credential_process` or the AWS CLI.
#[cfg(unix)]
pub(crate) fn program(path: &std::path::Path, script: &str) -> Result {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!/bin/sh\n{script}\n"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// a command for the private bucket `private-lab`, served by `routes`, whose keys come from the
/// AWS profile `lab` in the config file `config`, with no keys in the environment.
fn profile_command(
    sandbox: &Sandbox,
    routes: &str,
    config: &std::path::Path,
    args: &[&str],
) -> Fetchloom {
    let mut command = sandbox.command(args);
    command
        .env("FETCHLOOM_TEST_BUCKETS", routes)
        .env("FETCHLOOM_S3_BUCKETS", "private-lab")
        .env("AWS_CONFIG_FILE", config)
        .env("AWS_PROFILE", "lab");
    command
}

/// the bucket `private-lab`, which refuses every listing of its root.
fn refusing() -> Result<Routes> {
    Routes::start(vec![(
        s3_listing("/private-lab.s3.amazonaws.com/", "", None),
        refusal(),
    )])
}

/// the program runs without a shell, once for the listing and the download, and nothing it
/// printed reaches an output, a file or the cache.
#[cfg(unix)]
#[test]
fn a_credential_process_gives_the_keys_for_a_listed_bucket() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/",
        vec![
            refusal(),
            s3_page(&[("raw/a.csv", PARTICIPANTS)], None),
            versioned(),
        ],
    )?;
    let folder = sandbox.folder("aws")?;
    let runs = folder.join("runs");
    let keys = folder.join("keys");
    program(
        &keys,
        &format!(
            "printf '%s\\n' \"$@\" >> '{}'\nprintf 'said on stderr\\n' >&2\nprintf '%s' '{PRINTED}'",
            runs.display()
        ),
    )?;
    let config = folder.join("config");
    fs::write(
        &config,
        format!(
            "[profile lab]\ncredential_process = \"{}\" --as \"two words\" \"$HOME\" a;b\n",
            keys.display()
        ),
    )?;
    let args = [
        "-vv",
        "--json",
        "add",
        "s3://private-lab/raw/",
        "--as",
        "raw",
    ];
    let output = profile_command(&sandbox, &server.url, &config, &args).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    // one run, with its words as written: nothing a shell would do to them
    assert_eq!(fs::read_to_string(&runs)?, "--as\ntwo words\n$HOME\na;b\n");
    let heads = server.requests();
    assert_eq!(heads.len(), 3, "{heads:?}");
    assert!(!heads[0].contains("authorization"), "{}", heads[0]);
    for head in &heads[1..] {
        assert_eq!(
            signed_with(head, PRINTED_SECRET)?,
            ("akiaprintedkey".to_owned(), "us-east-1".to_owned())
        );
        assert!(
            head.contains(&format!("x-amz-security-token: {PRINTED_SESSION}")),
            "{head}"
        );
        assert!(!head.contains(PRINTED_SECRET), "{head}");
    }
    let work = sandbox.work();
    assert_eq!(fs::read(work.join("data/raw/a.csv"))?, PARTICIPANTS);
    let mut surfaces = vec![
        ("stdout".to_owned(), output.stdout),
        ("stderr".to_owned(), output.stderr),
    ];
    kept_files(&work, &mut surfaces)?;
    kept_files(&sandbox.cache(), &mut surfaces)?;
    for (name, bytes) in &surfaces {
        let text = String::from_utf8_lossy(bytes);
        for printed in [PRINTED_SECRET, PRINTED_SESSION, "said on stderr"] {
            assert!(!text.contains(printed), "{name} holds {printed}: {text}");
        }
    }
    Ok(())
}

/// the profile signs in to IAM identity center, and the AWS CLI comes from a folder PATH names.
#[cfg(unix)]
#[test]
fn a_profile_that_signs_in_gets_its_keys_from_the_aws_cli() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/",
        vec![
            refusal(),
            s3_page(&[("raw/a.csv", PARTICIPANTS)], None),
            versioned(),
        ],
    )?;
    let folder = sandbox.folder("aws")?;
    let asked = folder.join("asked");
    let bin = sandbox.folder("bin")?;
    program(
        &bin.join("aws"),
        &format!(
            "printf '%s\\n' \"$@\" >> '{}'\nprintf '%s' '{PRINTED}'",
            asked.display()
        ),
    )?;
    let config = folder.join("config");
    fs::write(
        &config,
        "[profile lab]\nsso_session = lab\nsso_account_id = 123456789012\nsso_role_name = reader\n[sso-session lab]\nsso_start_url = https://lab.awsapps.com/start\nsso_region = eu-west-1\n",
    )?;
    let args = ["add", "s3://private-lab/raw/", "--as", "raw"];
    let output = profile_command(&sandbox, &server.url, &config, &args)
        .env("PATH", &bin)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read_to_string(&asked)?,
        "configure\nexport-credentials\n--profile\nlab\n--format\nprocess\n"
    );
    let heads = server.requests();
    assert_eq!(heads.len(), 3, "{heads:?}");
    for head in &heads[1..] {
        assert_eq!(signed_with(head, PRINTED_SECRET)?.0, "akiaprintedkey");
    }
    assert_eq!(
        fs::read(sandbox.work().join("data/raw/a.csv"))?,
        PARTICIPANTS
    );
    Ok(())
}

/// only absolute PATH folders count, so an `aws` in a relative one, which a cloned project could
/// hold, never runs.
#[test]
fn a_profile_that_signs_in_without_the_aws_cli_says_to_install_it() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = refusing()?;
    let folder = sandbox.folder("aws")?;
    let config = folder.join("config");
    fs::write(
        &config,
        "[profile lab]\nsso_session = lab\nsso_account_id = 123456789012\nsso_role_name = reader\n",
    )?;
    let planted = sandbox.folder("work/bin")?;
    let ran = sandbox.work().join("ran");
    #[cfg(unix)]
    program(&planted.join("aws"), &format!(": > '{}'", ran.display()))?;
    #[cfg(windows)]
    fs::write(planted.join("aws.exe"), b"not a program")?;
    let path = std::env::join_paths([std::path::PathBuf::from("bin"), sandbox.folder("empty")?])?;
    let output = profile_command(
        &sandbox,
        &routes.url,
        &config,
        &["--json", "add", "s3://private-lab/"],
    )
    .env("PATH", path)
    .output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "auth.required", "{document}");
    assert_eq!(
        error["message"],
        "the AWS profile lab signs in to IAM identity center, which fetchloom leaves to the AWS CLI, and no folder on PATH holds the AWS CLI"
    );
    assert_eq!(
        error["help"],
        "install the AWS CLI, version 2.9 or later, and sign in with `aws sso login --profile lab`"
    );
    assert!(!ran.exists());
    assert_eq!(routes.targets().len(), 1);
    Ok(())
}

/// the request never goes again without a key or with the profile's other keys.
#[cfg(unix)]
#[test]
fn expired_keys_from_a_credential_process_stop_the_request() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/",
        vec![refusal(), s3_page(&[("raw/a.csv", PARTICIPANTS)], None)],
    )?;
    let folder = sandbox.folder("aws")?;
    let keys = folder.join("keys");
    let expired = PRINTED.replace("2999-01-01T00:00:00Z", "2001-01-01T00:00:00+00:00");
    program(&keys, &format!("printf '%s' '{expired}'"))?;
    let config = folder.join("config");
    fs::write(
        &config,
        format!(
            "[profile lab]\ncredential_process = {}\naws_access_key_id = akiaotherkey\naws_secret_access_key = other-secret\n",
            keys.display()
        ),
    )?;
    let args = ["--json", "add", "s3://private-lab/raw/"];
    let output = profile_command(&sandbox, &server.url, &config, &args).output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "auth.required", "{document}");
    assert_eq!(
        error["message"],
        "the keys the credential_process of the AWS profile lab gave expired at 2001-01-01T00:00:00Z"
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// keys can expire mid-run, as after a jump in this machine's clock. the request goes again with
/// the new keys, and the expired session S3's refusal repeats shows in no output.
#[cfg(unix)]
#[test]
fn keys_s3_says_expired_come_again_from_their_program_once() -> Result {
    let sandbox = Sandbox::new()?;
    let expired = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>ExpiredToken</Code><Message>The provided token has expired.</Message><Token-0>printed-session-1</Token-0></Error>";
    let server = Server::start(
        "/",
        vec![
            refusal(),
            reply(
                "400 Bad Request",
                &[
                    ("Content-Type", "application/xml"),
                    ("Content-Length", &expired.len().to_string()),
                ],
                expired,
            ),
            s3_page(&[("raw/a.csv", PARTICIPANTS)], None),
            versioned(),
        ],
    )?;
    let folder = sandbox.folder("aws")?;
    let runs = folder.join("runs");
    let keys = folder.join("keys");
    program(
        &keys,
        &format!(
            "printf 'ran\\n' >> '{runs}'\nrun=$(wc -l < '{runs}')\nprintf '{{\"Version\": 1, \"AccessKeyId\": \"akiaprintedkey\", \"SecretAccessKey\": \"{PRINTED_SECRET}\", \"SessionToken\": \"printed-session-%s\", \"Expiration\": \"2999-01-01T00:00:00Z\"}}' $run",
            runs = runs.display()
        ),
    )?;
    let config = folder.join("config");
    fs::write(
        &config,
        format!("[profile lab]\ncredential_process = {}\n", keys.display()),
    )?;
    let args = ["-vv", "add", "s3://private-lab/raw/", "--as", "raw"];
    let output = profile_command(&sandbox, &server.url, &config, &args).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read_to_string(&runs)?, "ran\nran\n");
    let heads = server.requests();
    assert_eq!(heads.len(), 4, "{heads:?}");
    for (head, session) in heads[1..].iter().zip([1, 2, 2]) {
        assert!(
            head.contains(&format!(
                "x-amz-security-token: printed-session-{session}\r\n"
            )),
            "{head}"
        );
    }
    assert_eq!(
        fs::read(sandbox.work().join("data/raw/a.csv"))?,
        PARTICIPANTS
    );
    for said in [&stderr, &text(&output.stdout)?] {
        assert!(!said.contains("printed-session"), "{said}");
    }
    Ok(())
}

/// printing no keys fetchloom can read fails the same way, with what went wrong.
#[cfg(unix)]
#[test]
fn a_credential_process_that_fails_is_named_without_what_it_printed() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = refusing()?;
    let folder = sandbox.folder("aws")?;
    let keys = folder.join("keys");
    let config = folder.join("config");
    fs::write(
        &config,
        format!("[profile lab]\ncredential_process = {}\n", keys.display()),
    )?;
    let unreadable =
        "the credential_process of the AWS profile lab printed no keys fetchloom can read";
    for (script, message) in [
        (
            format!("printf '%s' '{PRINTED}'\nprintf 'said on stderr' >&2\nexit 3"),
            "the credential_process of the AWS profile lab exited with code 3".to_owned(),
        ),
        (
            format!(
                "printf '%s' '{}'",
                PRINTED.replace("SecretAccessKey", "Secret")
            ),
            format!("{unreadable}: it has no SecretAccessKey"),
        ),
        (
            format!(
                "printf '%s' '{}'",
                PRINTED.replace("\"Version\": 1", "\"Version\": 2")
            ),
            format!("{unreadable}: its Version is not 1"),
        ),
        (
            format!(
                "printf '%s' '{}'",
                PRINTED.replace("2999-01-01T00:00:00Z", "soon")
            ),
            format!("{unreadable}: its Expiration is not a time such as 2026-11-12T12:00:00Z"),
        ),
        (
            format!("printf 'secret %s' '{PRINTED_SECRET}'"),
            format!("{unreadable}: it is not JSON"),
        ),
    ] {
        program(&keys, &script)?;
        let args = ["--json", "add", "s3://private-lab/"];
        let output = profile_command(&sandbox, &routes.url, &config, &args).output()?;
        assert_eq!(output.status.code(), Some(40), "{script}");
        let document = json(&output)?;
        assert_eq!(document["errors"][0]["kind"], "auth.required", "{document}");
        assert_eq!(
            document["errors"][0]["message"],
            message.as_str(),
            "{script}"
        );
        for surface in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(surface);
            for printed in [PRINTED_SECRET, PRINTED_SESSION, "said on stderr"] {
                assert!(!text.contains(printed), "{printed} in {text}");
            }
        }
    }
    Ok(())
}

/// a bucket nobody listed never runs it, nor does a listed one that answers without a key.
#[cfg(unix)]
#[test]
fn a_credential_process_runs_only_for_a_listed_bucket_that_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.folder("aws")?;
    let ran = folder.join("ran");
    let keys = folder.join("keys");
    program(
        &keys,
        &format!(": > '{}'\nprintf '%s' '{PRINTED}'", ran.display()),
    )?;
    let config = folder.join("config");
    fs::write(
        &config,
        format!("[profile lab]\ncredential_process = {}\n", keys.display()),
    )?;
    let refused = refusing()?;
    let unlisted = profile_command(
        &sandbox,
        &refused.url,
        &config,
        &["--json", "add", "s3://private-lab/"],
    )
    .env("FETCHLOOM_S3_BUCKETS", "s3://another-lab")
    .output()?;
    assert_eq!(unlisted.status.code(), Some(40));
    assert_eq!(
        json(&unlisted)?["errors"][0]["message"],
        "s3://private-lab/ is private: S3 refused to list it without a key"
    );
    assert_eq!(refused.targets().len(), 1);
    let public = Routes::start(vec![
        (
            s3_listing("/private-lab.s3.amazonaws.com/", "raw/", None),
            s3_page(&[("raw/a.csv", PARTICIPANTS)], None),
        ),
        (
            "/private-lab.s3.amazonaws.com/raw/a.csv".to_owned(),
            file(PARTICIPANTS, &format!("\"{}\"", md5(PARTICIPANTS))),
        ),
    ])?;
    let listed = profile_command(
        &sandbox,
        &public.url,
        &config,
        &["add", "s3://private-lab/raw/", "--as", "raw"],
    )
    .output()?;
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr)?);
    for request in public.requests() {
        assert!(!request.contains("authorization"), "{request}");
    }
    assert!(!ran.exists());
    Ok(())
}

/// a bucket at `base` whose one prefix lists `objects`, stated as the test asks, serving no file.
fn stated_bucket(base: &str, objects: &[(&str, String, u64)]) -> Result<Routes> {
    Routes::start(vec![(
        s3_listing(base, "data/", None),
        s3_stated(objects, None),
    )])
}

#[test]
fn an_md5_hint_for_a_smaller_file_never_skips_asking() -> Result {
    let sandbox = Sandbox::new()?;
    let base = "/audit-bucket.s3.amazonaws.com/";
    let small = Routes::start(vec![
        (
            s3_listing(base, "data/", None),
            s3_page(&[("data/a.bin", SCAN)], None),
        ),
        (
            format!("{base}data/a.bin"),
            file(SCAN, &format!("\"{}\"", md5(SCAN))),
        ),
    ])?;
    let primed = command(
        &sandbox,
        &small,
        &["get", "s3://audit-bucket/data/", "--dir", "primed"],
    )
    .output()?;
    assert_eq!(primed.status.code(), Some(0), "{}", text(&primed.stderr)?);
    let large = stated_bucket(base, &[("data/a.bin", md5(SCAN), (1 << 30) + 1)])?;
    fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.data]\nref = \"s3://audit-bucket/data/\"\n",
    )?;

    let output = command(&sandbox, &large, &["--json", "lock"]).output()?;

    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "unconfirmed", "{document}");
    assert_eq!(large.targets(), [s3_listing(base, "data/", None)]);
    Ok(())
}

#[test]
fn listed_sizes_too_large_to_add_up_are_asked_about() -> Result {
    let sandbox = Sandbox::new()?;
    let base = "/audit-bucket.s3.amazonaws.com/";
    let huge = 1 << 63;
    let routes = stated_bucket(
        base,
        &[
            ("data/a.bin", md5(b"a"), huge),
            ("data/b.bin", md5(b"b"), huge),
        ],
    )?;
    let listing = s3_listing(base, "data/", None);
    // the sizes are under test, not the disk: FAT32 refuses each file before anything asks
    let command = |sandbox: &Sandbox, routes: &Routes, args: &[&str]| {
        let mut command = command(sandbox, routes, args);
        command.env("FETCHLOOM_TEST_FILESYSTEM", "ext4");
        command
    };

    let got = command(
        &sandbox,
        &routes,
        &["--json", "get", "s3://audit-bucket/data/", "--dir", "out"],
    )
    .output()?;
    assert_eq!(got.status.code(), Some(1), "{}", text(&got.stderr)?);
    assert_eq!(json(&got)?["errors"][0]["kind"], "unconfirmed");
    let added = command(
        &sandbox,
        &routes,
        &["--json", "add", "s3://audit-bucket/data/"],
    )
    .output()?;
    assert_eq!(added.status.code(), Some(1), "{}", text(&added.stderr)?);
    assert_eq!(json(&added)?["errors"][0]["kind"], "unconfirmed");
    assert!(routes.targets().iter().all(|target| *target == listing));

    // approved, the totals reach the progress lines, and only the missing files fail
    let approved = command(
        &sandbox,
        &routes,
        &[
            "get",
            "s3://audit-bucket/data/",
            "--dir",
            "approved",
            "--yes",
        ],
    )
    .output()?;
    let stderr = text(&approved.stderr)?;
    assert_eq!(approved.status.code(), Some(10), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    Ok(())
}

/// the help also gives the way to need fewer files.
#[test]
fn a_listing_past_the_most_files_names_the_setting() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "s3://openneuro.org/ds000001/"],
    )
    .env("FETCHLOOM_MOST_FILES", "2")
    .output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "source.invalid");
    assert_eq!(
        document["errors"][0]["message"],
        "s3://openneuro.org/ds000001/ lists more than 2 files, the most this run lists for one dataset"
    );
    assert_eq!(
        document["errors"][0]["help"],
        "name a narrower prefix, ending in /, for the folder you need, or raise FETCHLOOM_MOST_FILES, or most_files in the settings file; each listed file takes about 2 KiB of memory"
    );
    Ok(())
}

/// past what data.lock lists in itself, a dataset's files go to a compressed list beside it.
#[test]
fn a_big_dataset_keeps_its_file_list_beside_data_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    let run = |folder: &std::path::Path, args: &[&str]| {
        let mut command = command(&sandbox, &routes, args);
        command
            .current_dir(folder)
            .env("FETCHLOOM_TEST_INLINE", "2");
        command.output()
    };
    let work = sandbox.work();
    let output = run(&work, &["add", "s3://openneuro.org/ds000001/"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.starts_with("version = 1\n"), "{lock}");
    assert!(lock.contains("file_count = 3\n"), "{lock}");
    let parts: Vec<_> = fs::read_dir(work.join("data.lock.d"))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    assert_eq!(parts.len(), 1, "{parts:?}");
    assert!(
        lock.contains(&format!("files_in = [\"data.lock.d/{}\"]", parts[0])),
        "{lock}"
    );
    let listed = json(&run(&work, &["--json", "list", "ds000001"])?)?;
    assert_eq!(
        listed["result"]["datasets"][0]["locked_files"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    let clone = sandbox.folder("clone")?;
    fs::create_dir(clone.join("data.lock.d"))?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), clone.join(file))?;
    }
    let part = format!("data.lock.d/{}", parts[0]);
    fs::copy(work.join(&part), clone.join(&part))?;
    let output = run(&clone, &["sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(clone.join("data/ds000001/participants.tsv"))?,
        PARTICIPANTS
    );
    // a damaged list is refused, and gc keeps everything rather than guess
    fs::write(clone.join(&part), b"not a list")?;
    let status = json(&run(&clone, &["--json", "status"])?)?;
    assert_eq!(status["errors"][0]["kind"], "project.invalid");
    let gc = run(&clone, &["--json", "cache", "gc", "--now"])?;
    assert_eq!(gc.status.code(), Some(50), "{}", text(&gc.stderr)?);
    assert!(
        json(&gc)?["errors"][0]["message"]
            .as_str()
            .is_some_and(|message| message.ends_with("cannot be read, so the cache stays as it is")),
    );
    // shrunk to what data.lock lists in itself, the list and its folder go
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    fs::write(
        work.join("data.toml"),
        format!("{manifest}exclude = \"README\"\n"),
    )?;
    let output = run(&work, &["sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(fs::read_to_string(work.join("data.lock"))?.starts_with("version = 1\n"));
    assert!(!work.join("data.lock.d").exists());
    Ok(())
}

/// a cached archive unpacks offline, one the cache dropped once unpacked is fetched by its own
/// link to stay packed, and `sync --locked` calls the lock out of date.
#[test]
fn switching_extract_needs_no_listing() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fs::read(crate::support::workspace().join("tests/fixtures/archives/python.zip"))?;
    let listing = s3_listing("/shard-bucket.s3.amazonaws.com/", "shards/", None);
    let object = "/shard-bucket.s3.amazonaws.com/shards/train.zip".to_owned();
    let routes = Routes::start(vec![
        (
            listing.clone(),
            s3_page(&[("shards/train.zip", &zip)], None),
        ),
        (object.clone(), file(&zip, &format!("\"{}\"", md5(&zip)))),
    ])?;
    let run = |args: &[&str]| command(&sandbox, &routes, args).output();
    let added = run(&[
        "add",
        "--no-extract",
        "s3://shard-bucket/shards/",
        "--as",
        "shards",
    ])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let manifest = sandbox.work().join("data.toml");
    let lock = sandbox.work().join("data.lock");
    let packed = fs::read_to_string(&manifest)?;
    assert!(fs::read_to_string(&lock)?.contains("\nextract = false\n"));
    fs::write(&manifest, packed.replace("extract = false\n", ""))?;

    let status = json(&run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["unlocked"], serde_json::json!(["shards"]));
    let locked = run(&["--json", "sync", "--locked"])?;
    assert_eq!(locked.status.code(), Some(1), "{}", text(&locked.stderr)?);
    let document = json(&locked)?;
    assert_eq!(document["errors"][0]["kind"], "lock.outdated");
    assert_eq!(
        document["errors"][0]["details"][0],
        "shards changed extract in data.toml; `fetchloom lock` needs no network for it while its archives are cached"
    );
    let unpacked = run(&["--offline", "sync"])?;
    assert_eq!(
        unpacked.status.code(),
        Some(0),
        "{}",
        text(&unpacked.stderr)?
    );
    let folder = sandbox.work().join("data/shards");
    assert_eq!(fs::read(folder.join("README.md"))?, b"# sample dataset\n");
    assert!(!folder.join("train.zip").exists());
    let text_now = fs::read_to_string(&lock)?;
    assert!(text_now.contains(", unpacks_to = \"sha256:"), "{text_now}");
    assert!(!text_now.contains("extract"), "{text_now}");
    assert_eq!(routes.targets(), [listing.clone(), object.clone()]);

    fs::write(&manifest, &packed)?;
    let offline = run(&["--json", "--offline", "sync"])?;
    assert_eq!(
        offline.status.code(),
        Some(20),
        "{}",
        text(&offline.stderr)?
    );
    let document = json(&offline)?;
    assert_eq!(document["errors"][0]["kind"], "offline.missing");
    assert_eq!(
        document["errors"][0]["details"],
        serde_json::json!(["shards/train.zip"])
    );
    let repacked = run(&["sync"])?;
    assert_eq!(
        repacked.status.code(),
        Some(0),
        "{}",
        text(&repacked.stderr)?
    );
    assert_eq!(fs::read(folder.join("train.zip"))?, zip);
    assert!(!folder.join("README.md").exists());
    assert!(!fs::read_to_string(&lock)?.contains("unpacks_to"));
    assert_eq!(routes.targets(), [listing, object.clone(), object]);
    assert_anonymous(&routes);
    Ok(())
}

/// a link a source has to list, as a hugging face link does, with one file kept from it.
const LISTED: [&str; 3] = [
    "https://s3.amazonaws.com/openneuro.org/ds000001/",
    "--select",
    "participants.tsv",
];

/// offline nothing lists the link and its bytes left the cache, yet the file holding them stays.
#[test]
fn get_output_of_a_listed_link_runs_again_offline() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    let get = [&["get"][..], &LISTED, &["-o", "p.tsv"]].concat();
    let online = command(&sandbox, &routes, &get).output()?;
    assert_eq!(online.status.code(), Some(0), "{}", text(&online.stderr)?);
    let asked = routes.requests().len();
    let offline = command(&sandbox, &routes, &[&["--offline"][..], &get].concat()).output()?;
    let stderr = text(&offline.stderr)?;
    assert_eq!(offline.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Ok p.tsv already holds what the link served last time"),
        "{stderr}"
    );
    assert_eq!(routes.requests().len(), asked, "offline asks nothing");
    assert_eq!(fs::read(sandbox.work().join("p.tsv"))?, PARTICIPANTS);
    Ok(())
}

/// offline, `--sha256` alone decides about the file at `-o`, with no earlier run.
#[test]
fn get_output_of_a_listed_link_with_its_sha256_needs_nothing_offline() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    fs::write(sandbox.work().join("p.tsv"), PARTICIPANTS)?;
    let sha256 = Sha256::digest(PARTICIPANTS)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    let get = [
        &["--offline", "get"][..],
        &LISTED,
        &["-o", "p.tsv", "--sha256", &sha256],
    ]
    .concat();
    let output = command(&sandbox, &routes, &get).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Ok p.tsv already holds what --sha256 names"),
        "{stderr}"
    );
    assert_eq!(routes.requests(), Vec::<String>::new());
    Ok(())
}

/// the folder's file is checked again from the cache.
#[test]
fn get_folder_of_a_listed_link_runs_again_offline() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    let get = [&["get"][..], &LISTED, &["--dir", "d"]].concat();
    let online = command(&sandbox, &routes, &get).output()?;
    assert_eq!(online.status.code(), Some(0), "{}", text(&online.stderr)?);
    let asked = routes.requests().len();
    let offline = command(&sandbox, &routes, &[&["--offline"][..], &get].concat()).output()?;
    let stderr = text(&offline.stderr)?;
    assert_eq!(offline.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("is already up to date"), "{stderr}");
    assert_eq!(routes.requests().len(), asked, "offline asks nothing");
    Ok(())
}

/// keeping every file, the line counts the whole listing.
#[test]
fn get_counts_the_files_it_keeps_when_it_resolves() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    let one = [&["get"][..], &LISTED, &["-o", "p.tsv"]].concat();
    let output = command(&sandbox, &routes, &one).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Resolved s3://openneuro.org/ds000001/ · 1 of 3 files · 28 B\n"),
        "{stderr}"
    );
    let all = ["get", LISTED[0], "--dir", "all"];
    let output = command(&sandbox, &routes, &all).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Resolved s3://openneuro.org/ds000001/ · 3 files · 62 B\n"),
        "{stderr}"
    );
    Ok(())
}

/// a tar holding the one file `name`, with `body`.
fn tar_of(name: &str, body: &[u8]) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, name, body)?;
    Ok(builder.into_inner()?)
}

/// the routes of an s3-compatible bucket `speech` listing `shards/` with each object's size and
/// `ETag`, and serving each object; such a service promises no MD5 in an `ETag`.
fn speech_bucket(objects: &[(String, Vec<u8>, String)]) -> Vec<(String, Vec<Part>)> {
    let listed: Vec<(&str, String, u64)> = objects
        .iter()
        .map(|(key, body, etag)| (key.as_str(), etag.clone(), body.len() as u64))
        .collect();
    let mut routes = vec![(
        s3_listing("/speech/", "shards/", None),
        s3_stated(&listed, None),
    )];
    for (key, body, etag) in objects {
        routes.push((format!("/speech/{key}"), file(body, &format!("\"{etag}\""))));
    }
    routes
}

/// the first `count` shards of [`speech_bucket`], each a tar of one sample, with the MD5 an
/// s3-compatible service such as `MinIO` gives as its `ETag`.
fn speech_shards(count: usize) -> Result<Vec<(String, Vec<u8>, String)>> {
    (0..count)
        .map(|number| {
            let body = tar_of(
                &format!("sample-{number}.txt"),
                format!("sample {number}").as_bytes(),
            )?;
            let etag = md5(&body);
            Ok((format!("shards/shard-{number:06}.tar"), body, etag))
        })
        .collect()
}

/// a project in the sandbox that pins `objects` of [`speech_bucket`] as the dataset `shards`, and
/// the server answering for them.
fn speech_project(sandbox: &Sandbox, objects: &[(String, Vec<u8>, String)]) -> Result<Routes> {
    let routes = Routes::start(speech_bucket(objects))?;
    sandbox.allow(&routes.url);
    let reference = format!("s3:{}/speech/shards/", routes.url);
    let added = sandbox.run(&["add", &reference, "--as", "shards"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    Ok(routes)
}

/// what `routes` were asked for since they had answered `asked` requests, the listing first.
fn asked_since(routes: &Routes, asked: usize) -> Vec<String> {
    let mut targets = routes.targets().split_off(asked);
    targets.sort();
    targets
}

/// the unchanged objects list the size and `ETag` this machine's cache recorded, so their pins
/// stay without their bytes coming again.
#[test]
fn an_upgrade_downloads_only_the_objects_a_listing_shows_are_new_or_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let mut shards = speech_shards(6)?;
    let new = shards.pop().ok_or("no shards")?;
    let routes = speech_project(&sandbox, &shards)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    shards.push(new);
    routes.change(speech_bucket(&shards));
    let asked = routes.targets().len();

    let upgraded = sandbox.run(&["lock", "--upgrade", "shards"])?;

    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    assert_eq!(
        asked_since(&routes, asked),
        [
            "/speech/?list-type=2&prefix=shards%2F&encoding-type=url",
            "/speech/shards/shard-000005.tar"
        ]
    );
    let upgraded = fs::read_to_string(sandbox.work().join("data.lock"))?;
    for (key, ..) in &shards[..5] {
        let pin = |lock: &str| {
            lock.lines()
                .find(|line| line.contains(key.as_str()))
                .map(str::to_owned)
        };
        assert_eq!(pin(&upgraded), pin(&lock), "{key}");
    }
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("data/shards/sample-5.txt"))?,
        b"sample 5"
    );
    assert_eq!(routes.targets().len(), asked + 2, "sync asks nothing more");
    Ok(())
}

/// a record that no longer matches counts as none, and the new bytes are pinned.
#[test]
fn an_upgrade_downloads_an_object_listed_with_another_etag_or_size() -> Result {
    let sandbox = Sandbox::new()?;
    let mut shards = speech_shards(3)?;
    let routes = speech_project(&sandbox, &shards)?;
    let changed = tar_of("sample-0.txt", b"sample 0 again")?;
    shards[0].2 = md5(&changed);
    shards[0].1 = changed;
    // a service that keeps the ETag of bytes whose size changed still lists the new size
    shards[1].1 = tar_of("sample-1.txt", &[b'x'; 600])?;
    routes.change(speech_bucket(&shards));
    let asked = routes.targets().len();

    let upgraded = sandbox.run(&["lock", "--upgrade", "shards"])?;

    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    assert_eq!(
        asked_since(&routes, asked),
        [
            "/speech/?list-type=2&prefix=shards%2F&encoding-type=url",
            "/speech/shards/shard-000000.tar",
            "/speech/shards/shard-000001.tar"
        ]
    );
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let data = sandbox.work().join("data/shards");
    assert_eq!(fs::read(data.join("sample-0.txt"))?, b"sample 0 again");
    assert_eq!(fs::read(data.join("sample-1.txt"))?, [b'x'; 600]);
    Ok(())
}

/// the record that spares a download lives in this machine's cache, not in data.lock.
#[test]
fn an_upgrade_on_a_machine_without_the_record_downloads_every_object() -> Result {
    let sandbox = Sandbox::new()?;
    let shards = speech_shards(3)?;
    let routes = speech_project(&sandbox, &shards)?;
    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), teammate.join(name))?;
    }
    let asked = routes.targets().len();

    let upgraded = sandbox
        .command_in(&teammate, &["lock", "--upgrade", "shards"])
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;

    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    assert_eq!(asked_since(&routes, asked).len(), 4);
    assert_eq!(
        fs::read_to_string(teammate.join("data.lock"))?,
        fs::read_to_string(sandbox.work().join("data.lock"))?
    );
    Ok(())
}

/// reuse trusts a service to change the `ETag` or size with the bytes, so here the old pin stays;
/// where the bytes are downloaded, the pinned SHA-256 refuses them and nothing is placed.
#[test]
fn bytes_changed_under_an_unchanged_etag_are_refused_where_they_are_downloaded() -> Result {
    let sandbox = Sandbox::new()?;
    let mut shards = speech_shards(2)?;
    let routes = speech_project(&sandbox, &shards)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let changed = tar_of("sample-1.txt", b"sample 9")?;
    assert_eq!(changed.len(), shards[1].1.len());
    shards[1].1 = changed;
    routes.change(speech_bucket(&shards));
    let asked = routes.targets().len();

    let upgraded = sandbox.run(&["lock", "--upgrade", "shards"])?;

    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    assert_eq!(asked_since(&routes, asked).len(), 1, "only the listing");
    assert_eq!(fs::read_to_string(sandbox.work().join("data.lock"))?, lock);
    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), teammate.join(name))?;
    }
    let synced = sandbox
        .command_in(&teammate, &["--json", "sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
    assert_eq!(synced.status.code(), Some(30), "{}", text(&synced.stderr)?);
    let document = json(&synced)?;
    assert_eq!(
        document["errors"][0]["kind"], "integrity.mismatch",
        "{document}"
    );
    assert!(!teammate.join("data/shards/sample-1.txt").exists());
    Ok(())
}

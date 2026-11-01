//! public S3 and cloud storage buckets, listed and downloaded from a server on this machine that
//! stands in for both services.
#![cfg(debug_assertions)]

use std::fs;
use std::process::Command;

use md5::Digest as _;

use crate::support::{Part, Result, Routes, Sandbox, file, json, reply, text};

const PARTICIPANTS: &[u8] = b"participant_id\tsex\nsub-01\tF\n";
const SCAN: &[u8] = b"onset\tduration\n0\t1\n";
const README: &[u8] = b"a tiny dataset\n";

/// the listing query fetchloom sends S3 for `prefix`, escaped as it escapes it.
fn s3_listing(path: &str, prefix: &str, token: Option<&str>) -> String {
    let mut target = format!(
        "{path}?list-type=2&prefix={}&encoding-type=url",
        prefix.replace('/', "%2F")
    );
    if let Some(token) = token {
        target.push_str(&format!("&continuation-token={token}"));
    }
    target
}

/// an S3 listing page of `(key, body)` objects, with each key escaped as `encoding-type=url`
/// asks.
fn s3_page(objects: &[(&str, &[u8])], next: Option<&str>) -> Vec<Part> {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>b</Name><EncodingType>url</EncodingType>",
    );
    match next {
        Some(next) => xml.push_str(&format!(
            "<IsTruncated>true</IsTruncated><NextContinuationToken>{next}</NextContinuationToken>"
        )),
        None => xml.push_str("<IsTruncated>false</IsTruncated>"),
    }
    for (key, body) in objects {
        xml.push_str(&format!(
            "<Contents><Key>{}</Key><LastModified>2020-08-21T20:24:12.000Z</LastModified><ETag>&quot;{}&quot;</ETag><Size>{}</Size></Contents>",
            key.replace('/', "%2F").replace(' ', "+"),
            md5(body),
            body.len()
        ));
    }
    xml.push_str("</ListBucketResult>");
    reply(
        "200 OK",
        &[("Content-Type", "application/xml")],
        xml.as_bytes(),
    )
}

fn md5(body: &[u8]) -> String {
    md5::Md5::digest(body)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// a command that sends every S3 and cloud storage request to `routes`, with AWS and google
/// credentials set in its environment, which fetchloom must never send.
fn command(sandbox: &Sandbox, routes: &Routes, args: &[&str]) -> Command {
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
    Ok(Routes::start(vec![
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
    ])?)
}

#[test]
fn an_s3_prefix_is_added_pinned_and_synced_anonymously() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = openneuro()?;
    let output = command(
        &sandbox,
        &routes,
        &["add", "s3://openneuro.org/ds000001/", "--exclude", "README"],
    )
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
        lock.contains("url = \"https://s3.amazonaws.com/openneuro.org/ds000001/sub-01/func%20events.tsv\""),
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

    fs::remove_dir_all(work.join("data"))?;
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
        manifest.contains("[datasets.us-states]\nref = \"gs://cloud-samples-data/bigquery/us-states/\""),
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
fn a_private_bucket_is_refused_with_auth_required() -> Result {
    let sandbox = Sandbox::new()?;
    let denied = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";
    let routes = Routes::start(vec![(
        s3_listing("/private-lab.s3.amazonaws.com/", "", None),
        reply(
            "403 Forbidden",
            &[("Content-Type", "application/xml")],
            denied,
        ),
    )])?;
    let output = command(&sandbox, &routes, &["--json", "add", "s3://private-lab/"]).output()?;
    assert_eq!(output.status.code(), Some(40));
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "auth.required", "{document}");
    assert_eq!(
        error["message"],
        "s3://private-lab/ is not a public bucket: S3 refused to list it"
    );
    assert!(
        error["help"]
            .as_str()
            .unwrap_or_default()
            .contains("reads public buckets only"),
        "{document}"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    assert_anonymous(&routes);
    Ok(())
}

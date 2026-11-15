//! S3 buckets as mirrors: `fetchloom push` writes one, signed with the AWS keys fetchloom finds,
//! and a sync reads it back, from a server on this machine that keeps objects as S3 does.
#![cfg(debug_assertions)]

use crate::mirrors::{IRIS, MEMBERS, cold, holds_the_project, project};
use crate::support::{
    Fetchloom, Result, Routes, S3, Sandbox, file, json, ok, reply, sha256_hex, text,
};

const SECRET: &str = "secret-that-never-leaves-this-machine";
const MIRROR: &str = "s3://lab-data/mirror";
/// where the server keeps the mirror's top, as `FETCHLOOM_TEST_HOSTS` asks it.
const TOP: &str = "/lab-data.s3.amazonaws.com/mirror";

/// fetchloom with `args`, sending its S3 requests to `s3` with keys from the environment.
fn keyed(sandbox: &Sandbox, s3: &S3, key: &str, args: &[&str]) -> Fetchloom {
    let mut command = sandbox.command(args);
    command
        .env("FETCHLOOM_TEST_HOSTS", &s3.url)
        .env("AWS_ACCESS_KEY_ID", key)
        .env("AWS_SECRET_ACCESS_KEY", SECRET);
    command
}

/// the path the server keeps the object pinned to `bytes` at.
fn object(bytes: &[u8]) -> String {
    let hex = sha256_hex(bytes);
    format!("{TOP}/objects/{}/{}", &hex[..2], &hex[2..])
}

fn files() -> Vec<&'static [u8]> {
    std::iter::once(IRIS)
        .chain(MEMBERS.iter().map(|(_, bytes)| *bytes))
        .collect()
}

/// every file goes up signed, the larger ones in parts S3 checks one by one, and a clone with a
/// cache of its own syncs them back down from the bucket alone.
#[test]
fn a_push_to_a_bucket_comes_back_through_a_clean_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let asked = source.requests().len();
    let s3 = S3::start(SECRET)?;
    let output = keyed(
        &sandbox,
        &s3,
        "AKIALABPUSHWXYZ",
        &["--json", "--yes", "push", "--to", MIRROR],
    )
    .env("FETCHLOOM_TEST_MULTIPART", "16")
    .output()?;
    ok(&output)?;
    let document = json(&output)?;
    assert_eq!(document["result"]["target"], MIRROR, "{document}");
    assert_eq!(document["result"]["started"], true);
    assert_eq!(document["result"]["copied"], 6);
    assert_eq!(document["result"]["trees"], 1);
    assert!(s3.faults().is_empty(), "{:?}", s3.faults());
    assert_eq!(
        s3.object(&format!("{TOP}/fetchloom-mirror")).as_deref(),
        Some(&b"fetchloom mirror 1\n"[..])
    );
    for bytes in files() {
        assert_eq!(s3.object(&object(bytes)).as_deref(), Some(bytes));
    }
    let asked_s3 = s3.asked();
    assert!(
        asked_s3
            .iter()
            .all(|line| line.ends_with(" AKIALABPUSHWXYZ")),
        "{asked_s3:#?}"
    );
    // iris.csv's 48 bytes go in three parts of 16, each with its own hash, then are joined
    let iris = object(IRIS);
    let parts: Vec<&String> = asked_s3
        .iter()
        .filter(|line| line.starts_with(&format!("PUT {iris}?partNumber=")))
        .collect();
    assert_eq!(parts.len(), 3, "{asked_s3:#?}");
    assert!(
        asked_s3
            .iter()
            .any(|line| line.starts_with(&format!("POST {iris}?uploadId="))),
        "{asked_s3:#?}"
    );
    assert_eq!(s3.open_uploads(), 0);
    let (clone, cache) = cold(&sandbox, "clone")?;
    let synced = keyed(
        &sandbox,
        &s3,
        "AKIALABPUSHWXYZ",
        &["--json", "sync", "--locked", "--mirror", MIRROR],
    )
    .current_dir(&clone)
    .env("FETCHLOOM_CACHE", &cache)
    .output()?;
    ok(&synced)?;
    holds_the_project(&clone)?;
    assert_eq!(json(&synced)?["work"]["mirror_objects"], 6);
    assert_eq!(source.requests().len(), asked, "no source is asked");
    // the clone's reads go unsigned first, as for any listed bucket, then signed
    let reads: Vec<String> = s3.asked().split_off(asked_s3.len());
    assert!(
        reads[0].starts_with(&format!("GET {TOP}/fetchloom-mirror -")),
        "{reads:#?}"
    );
    assert!(
        reads[1..]
            .iter()
            .all(|line| line.ends_with(" AKIALABPUSHWXYZ")),
        "{reads:#?}"
    );
    for shown in [
        &output.stdout,
        &output.stderr,
        &synced.stdout,
        &synced.stderr,
    ] {
        assert!(!text(shown)?.contains(SECRET));
    }
    Ok(())
}

/// a second push looks once at each file with a HEAD and sends none of those there already.
#[test]
fn a_second_push_to_a_bucket_skips_what_is_there() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let s3 = S3::start(SECRET)?;
    let push = || {
        keyed(
            &sandbox,
            &s3,
            "AKIALABPUSHWXYZ",
            &["--json", "--yes", "push", "--to", MIRROR],
        )
        .output()
    };
    ok(&push()?)?;
    let first = s3.asked().len();
    let again = push()?;
    ok(&again)?;
    let document = json(&again)?;
    assert_eq!(document["result"]["started"], false);
    assert_eq!(document["result"]["present"], 6, "{document}");
    assert_eq!(document["result"]["copied"], 0);
    assert_eq!(document["result"]["trees"], 0);
    let asked = s3.asked().split_off(first);
    let methods: Vec<&str> = asked
        .iter()
        .map(|line| line.split(' ').next().unwrap_or_default())
        .collect();
    assert_eq!(methods[0], "GET", "{asked:#?}");
    assert!(
        methods[1..].iter().all(|method| *method == "HEAD"),
        "{asked:#?}"
    );
    assert_eq!(methods.len(), 1 + 6 + 1, "{asked:#?}");
    let dry = keyed(
        &sandbox,
        &s3,
        "AKIALABPUSHWXYZ",
        &["--json", "push", "--dry-run", "--to", MIRROR],
    )
    .output()?;
    ok(&dry)?;
    let document = json(&dry)?;
    assert_eq!(document["result"]["dry_run"], true);
    assert_eq!(document["result"]["present"], 6, "{document}");
    assert_eq!(document["result"]["changes"]["copy"], serde_json::json!([]));
    Ok(())
}

/// a key the bucket refuses is named masked, with what the bucket's policy must grant it, and no
/// file is sent.
#[test]
fn a_refused_key_names_what_the_bucket_must_grant() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let s3 = S3::start(SECRET)?;
    s3.refuse("AKIAREFUSEDWXYZ");
    let output = keyed(
        &sandbox,
        &s3,
        "AKIAREFUSEDWXYZ",
        &["--json", "--yes", "push", "--to", MIRROR],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "auth.required", "{document}");
    assert_eq!(
        error["message"],
        "S3 refused the AWS key \u{2026}WXYZ from AWS_ACCESS_KEY_ID for s3://lab-data/mirror"
    );
    assert_eq!(
        error["help"],
        "ask the owner of s3://lab-data to let that key push there: its policy must grant s3:ListBucket on the bucket, and s3:GetObject, s3:PutObject and s3:AbortMultipartUpload on mirror/*. or set AWS_PROFILE to a profile whose key may"
    );
    assert!(s3.paths().is_empty(), "{:?}", s3.paths());
    assert!(!text(&output.stdout)?.contains("AKIAREFUSED"));
    assert!(!text(&output.stdout)?.contains(SECRET));
    Ok(())
}

/// `status --sources` asks a bucket mirror for each file's first byte, so a file its source lost
/// still counts as had, named as one only the mirror serves now.
#[test]
fn a_bucket_mirror_counts_for_status_sources() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![("/files/iris.csv".to_owned(), file(IRIS, "\"i\""))])?;
    sandbox.allow(&source.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.csv", source.url)])?)?;
    let s3 = S3::start(SECRET)?;
    ok(&keyed(
        &sandbox,
        &s3,
        "AKIALABPUSHWXYZ",
        &["--yes", "push", "--to", MIRROR],
    )
    .output()?)?;
    source.change(vec![(
        "/files/iris.csv".to_owned(),
        reply("404 Not Found", &[("Content-Length", "0")], b""),
    )]);
    let asked = source.requests().len();
    let status = |mirror: Option<&str>| {
        let mut command = keyed(
            &sandbox,
            &s3,
            "AKIALABPUSHWXYZ",
            &["--json", "status", "--sources"],
        );
        if let Some(mirror) = mirror {
            command.arg("--mirror").arg(mirror);
        }
        command.output()
    };
    let output = status(Some(MIRROR))?;
    ok(&output)?;
    let document = json(&output)?;
    let iris = &document["result"]["sources"]["datasets"][0];
    assert_eq!(iris["name"], "iris", "{document}");
    assert_eq!(iris["on_mirrors"], 1, "{iris}");
    assert_eq!(iris["lost_at_source"][0]["outcome"], "gone", "{iris}");
    assert_eq!(
        source.requests().len(),
        asked + 1,
        "its source is asked first"
    );
    assert!(
        s3.asked()
            .iter()
            .any(|line| line.starts_with(&format!("GET {}", object(IRIS)))),
        "{:#?}",
        s3.asked()
    );
    // without the mirror, the lost file counts against its source
    let output = status(None)?;
    ok(&output)?;
    let iris = &json(&output)?["result"]["sources"]["datasets"][0];
    assert_eq!(iris["on_mirrors"], 0, "{iris}");
    Ok(())
}

/// offline asks no bucket mirror, and says so in its own words.
#[test]
fn offline_names_the_bucket_mirror_it_did_not_ask() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let (clone, cache) = cold(&sandbox, "clone")?;
    let output = sandbox
        .command_in(
            &clone,
            &[
                "--json",
                "--offline",
                "sync",
                "--locked",
                "--mirror",
                MIRROR,
            ],
        )
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stderr)?);
    let help = &json(&output)?["errors"][0]["help"];
    assert!(
        help.as_str().is_some_and(|help| help
            .starts_with("offline asks no S3 mirror, so s3://lab-data/mirror was not asked: ")),
        "{help}"
    );
    Ok(())
}

/// a login typed into a bucket's address is refused, and never repeated.
#[test]
fn a_login_in_a_bucket_address_is_refused_unshown() -> Result {
    let sandbox = Sandbox::new()?;
    let address = "s3:https://ada:CNRYsecret@minio.lab.example/scans/m";
    for args in [["push", "--to", address], ["sync", "--mirror", address]] {
        let output = sandbox.run(&args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(!stderr.contains("CNRYsecret"), "{stderr}");
        assert!(stderr.contains("it holds a login"), "{stderr}");
    }
    Ok(())
}

/// a part that fails takes its upload back, so the bucket keeps no parts and no object.
#[test]
fn a_failed_part_takes_its_upload_back() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let s3 = S3::start(SECRET)?;
    s3.fail_part(2);
    let output = keyed(
        &sandbox,
        &s3,
        "AKIALABPUSHWXYZ",
        &["--json", "--yes", "push", "--to", MIRROR],
    )
    .env("FETCHLOOM_TEST_MULTIPART", "16")
    .env("FETCHLOOM_RETRIES", "0")
    .output()?;
    assert_ne!(output.status.code(), Some(0));
    let asked = s3.asked();
    let iris = object(IRIS);
    assert!(
        asked
            .iter()
            .any(|line| line.starts_with(&format!("DELETE {iris}?uploadId="))),
        "{asked:#?}"
    );
    assert_eq!(s3.open_uploads(), 0, "{asked:#?}");
    assert_eq!(s3.object(&iris), None);
    assert!(s3.faults().is_empty(), "{:?}", s3.faults());
    let stderr = text(&output.stderr)?;
    assert!(!stderr.contains(SECRET), "{stderr}");
    Ok(())
}

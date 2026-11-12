//! azure blob containers, listed and downloaded from a server on this machine that stands in for
//! azure's blob hosts, public ones without a SAS and private ones with the SAS the person gave.
#![cfg(debug_assertions)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::support::{Part, Result, Routes, Sandbox, reply, text};

const A_CSV: &[u8] = b"site,value\nlab,1\n";
const A_MD5: &str = "7IgFjUFwc9UPsxF0OL2kYw==";
const B_CSV: &[u8] = b"site,value\nfield,2\n";
const B_MD5: &str = "wDwK+pN4pPcTmE3sjbu0wQ==";
/// a SAS as the azure portal gives one, its values escaped as they come.
const SAS: &str = "sv=2022-11-02&sr=c&sp=rl&se=2099-01-01T00%3A00%3A00Z&sig=lab-signature%3D";
/// what of [`SAS`] must never show: its signature, as it comes and unescaped.
const SECRETS: [&str; 2] = ["lab-signature%3D", "lab-signature="];

/// the listing fetchloom asks azure for, of `prefix` in `container` at `account`, and with
/// the SAS when `sas` is given.
fn listing(account: &str, container: &str, prefix: &str, sas: Option<&str>) -> String {
    let mut target = format!(
        "/{account}.blob.core.windows.net/{container}?restype=container&comp=list&prefix={}",
        prefix.replace('/', "%2F")
    );
    if let Some(sas) = sas {
        let _ = write!(target, "&{sas}");
    }
    target
}

/// a blob's link at the stand-in, with the SAS when `sas` is given.
fn blob(account: &str, container: &str, name: &str, sas: Option<&str>) -> String {
    let target = format!("/{account}.blob.core.windows.net/{container}/{name}");
    match sas {
        Some(sas) => format!("{target}?{sas}"),
        None => target,
    }
}

/// a page of a container's listing, as version 2020-10-02 writes one, of `(name, size, md5)`
/// blobs, with the marker of the next page when one follows.
fn page(container: &str, blobs: &[(&str, usize, &str)], next: Option<&str>) -> Vec<Part> {
    let mut xml = format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults ServiceEndpoint=\"https://lab.blob.core.windows.net/\" ContainerName=\"{container}\"><Blobs>"
    );
    for (name, size, md5) in blobs {
        let _ = write!(
            xml,
            "<Blob><Name>{name}</Name><Properties><Creation-Time>Wed, 26 Feb 2020 10:25:49 GMT</Creation-Time><Last-Modified>Wed, 26 Feb 2020 10:26:01 GMT</Last-Modified><Etag>0x8D7BAA64729D940</Etag><Content-Length>{size}</Content-Length><Content-Type>text/csv</Content-Type><Content-MD5>{md5}</Content-MD5><BlobType>BlockBlob</BlobType><AccessTier>Hot</AccessTier></Properties><OrMetadata /></Blob>"
        );
    }
    match next {
        Some(next) => {
            let _ = write!(xml, "</Blobs><NextMarker>{next}</NextMarker>");
        }
        None => xml.push_str("</Blobs><NextMarker />"),
    }
    xml.push_str("</EnumerationResults>");
    reply(
        "200 OK",
        &[
            ("Content-Type", "application/xml"),
            ("x-ms-version", "2020-10-02"),
        ],
        xml.as_bytes(),
    )
}

/// azure's answer to a request for a private container that carries no SAS.
fn unauthenticated() -> Vec<Part> {
    reply(
        "401 Server failed to authenticate the request. Please refer to the information in the www-authenticate header.",
        &[
            ("Content-Type", "application/xml"),
            ("x-ms-error-code", "NoAuthenticationInformation"),
            (
                "WWW-Authenticate",
                "Bearer authorization_uri=https://login.microsoftonline.com/common/oauth2/authorize resource_id=https://storage.azure.com",
            ),
        ],
        "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?><Error><Code>NoAuthenticationInformation</Code><Message>Server failed to authenticate the request. Please refer to the information in the www-authenticate header.\nRequestId:ebc76255-e01e-0013-09be-720b4b000000\nTime:2026-11-12T15:58:31.1001994Z</Message></Error>".as_bytes(),
    )
}

fn bytes(body: &[u8]) -> Vec<Part> {
    reply(
        "200 OK",
        &[
            ("Content-Type", "text/csv"),
            ("ETag", "\"0x8D7BAA64729D940\""),
        ],
        body,
    )
}

/// a command that sends every azure blob request to `routes`.
fn command(sandbox: &Sandbox, routes: &Routes, folder: &Path, args: &[&str]) -> Command {
    let mut command = sandbox.command_in(folder, args);
    command.env("FETCHLOOM_TEST_BUCKETS", &routes.url);
    command
}

/// every file under `folder`, named, for a search for secrets.
fn kept_files(folder: &Path, found: &mut Vec<(String, Vec<u8>)>) -> Result {
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

fn assert_no_sas(surfaces: &[(String, Vec<u8>)]) {
    for (name, bytes) in surfaces {
        let said = String::from_utf8_lossy(bytes);
        for secret in SECRETS {
            assert!(!said.contains(secret), "{name} holds {secret}: {said}");
        }
    }
}

/// the requests a stand-in took, each as its request line, in lowercase as the stand-in keeps
/// it, and whether it carried the version of azure's API fetchloom reads.
fn asked(routes: &Routes) -> Vec<(String, bool)> {
    routes
        .requests()
        .iter()
        .map(|head| {
            let line = head.lines().next().unwrap_or_default().to_owned();
            (line, head.contains("x-ms-version: 2020-10-02"))
        })
        .collect()
}

/// the request line of a GET of `target`, as [`asked`] keeps it.
fn get(target: &str) -> String {
    format!("get {} http/1.1", target.to_ascii_lowercase())
}

/// a private container the person lists, with a SAS in the environment as the az cli reads it:
/// the listing goes without the SAS, is refused, and goes again with it; the blobs carry it from
/// the start. a teammate's sync, given the container's SAS link in the settings file instead,
/// does the same. data.lock keeps no SAS, and none reaches an output, a log or a file.
#[test]
fn a_listed_private_container_is_read_with_its_sas_after_a_refusal() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        (listing("lab", "raw", "scans/", None), unauthenticated()),
        (
            listing("lab", "raw", "scans/", Some(SAS)),
            page(
                "raw",
                &[
                    ("scans/a.csv", A_CSV.len(), A_MD5),
                    ("scans/b.csv", B_CSV.len(), B_MD5),
                ],
                None,
            ),
        ),
        (blob("lab", "raw", "scans/a.csv", None), unauthenticated()),
        (blob("lab", "raw", "scans/b.csv", None), unauthenticated()),
        (blob("lab", "raw", "scans/a.csv", Some(SAS)), bytes(A_CSV)),
        (blob("lab", "raw", "scans/b.csv", Some(SAS)), bytes(B_CSV)),
    ])?;
    let trace = sandbox.folder("traces")?.join("add.json");
    let work = sandbox.work();
    let output = command(
        &sandbox,
        &routes,
        &work,
        &["-vv", "add", "azure:lab/raw/scans/", "--as", "scans"],
    )
    .env("FETCHLOOM_AZURE_CONTAINERS", "azure:lab/raw")
    .env("AZURE_STORAGE_SAS_TOKEN", SAS)
    .env("FETCHLOOM_TRACE", &trace)
    .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let sent = asked(&routes);
    assert_eq!(
        sent[..2],
        [
            (get(&listing("lab", "raw", "scans/", None)), true),
            (get(&listing("lab", "raw", "scans/", Some(SAS))), true),
        ]
    );
    let mut downloads: Vec<_> = sent[2..].to_vec();
    downloads.sort();
    assert_eq!(
        downloads,
        [
            (get(&blob("lab", "raw", "scans/a.csv", Some(SAS))), true),
            (get(&blob("lab", "raw", "scans/b.csv", Some(SAS))), true),
        ]
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("url = \"https://lab.blob.core.windows.net/raw/scans/a.csv\"")
            && lock.contains("md5 = \"ec88058d417073d50fb3117438bda463\""),
        "{lock}"
    );
    assert_eq!(fs::read(work.join("data/scans/a.csv"))?, A_CSV);
    let mut surfaces = vec![
        ("stdout".to_owned(), output.stdout),
        ("stderr".to_owned(), output.stderr),
        ("trace".to_owned(), fs::read(&trace)?),
    ];
    kept_files(&work, &mut surfaces)?;
    kept_files(&sandbox.cache(), &mut surfaces)?;

    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), clone.join(name))?;
    }
    let settings = sandbox.folder("teammate")?.join("settings.toml");
    fs::write(
        &settings,
        format!("azure_containers = [\"https://lab.blob.core.windows.net/raw?{SAS}\"]\n"),
    )?;
    let synced = command(&sandbox, &routes, &clone, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .env("FETCHLOOM_SETTINGS", &settings)
        .env("FETCHLOOM_JOBS", "1")
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stdout)?);
    assert_eq!(fs::read(clone.join("data/scans/b.csv"))?, B_CSV);
    let resent = asked(&routes)[4..].to_vec();
    assert_eq!(
        resent
            .iter()
            .filter(|(line, _)| !line.contains("sig="))
            .count(),
        1,
        "one blob is asked without the SAS, and refused, before both go with it: {resent:?}"
    );
    surfaces.push(("sync json".to_owned(), synced.stdout));
    surfaces.push(("sync stderr".to_owned(), synced.stderr));
    kept_files(&clone, &mut surfaces)?;
    kept_files(&sandbox.folder("teammate-cache")?, &mut surfaces)?;
    assert_no_sas(&surfaces);
    Ok(())
}

/// a container nobody listed never gets the SAS, though one is on the machine, and the refusal
/// says how to list it.
#[test]
fn a_container_nobody_listed_never_gets_the_sas() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(
        blob("other", "raw", "a.csv", None),
        unauthenticated(),
    )])?;
    let output = command(
        &sandbox,
        &routes,
        &sandbox.work(),
        &["get", "https://other.blob.core.windows.net/raw/a.csv"],
    )
    .env("FETCHLOOM_AZURE_CONTAINERS", "azure:lab/raw")
    .env("AZURE_STORAGE_SAS_TOKEN", SAS)
    .output()?;
    let stderr = text(&output.stderr)?;
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("azure_containers") && stderr.contains("azure:other/raw"),
        "{stderr}"
    );
    for (line, _) in asked(&routes) {
        assert!(!line.contains("sig="), "{line}");
    }
    assert_no_sas(&[
        ("stdout".to_owned(), output.stdout),
        ("stderr".to_owned(), output.stderr),
    ]);
    Ok(())
}

/// a link carrying a SAS is never written to data.toml: add refuses it and says where a SAS
/// belongs, without repeating it.
#[test]
fn add_refuses_a_link_that_carries_a_sas() -> Result {
    let sandbox = Sandbox::new()?;
    let link = format!("https://lab.blob.core.windows.net/raw/scans/a.csv?{SAS}");
    let output = sandbox.command(&["add", &link]).output()?;
    let stderr = text(&output.stderr)?;
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("AZURE_STORAGE_SAS_TOKEN") && stderr.contains("azure_containers"),
        "{stderr}"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    assert_no_sas(&[
        ("stdout".to_owned(), output.stdout),
        ("stderr".to_owned(), output.stderr),
    ]);
    Ok(())
}

/// `settings` shows the containers a SAS may go to, from the variable or the file, and never a
/// SAS an entry carries.
#[test]
fn settings_show_the_azure_containers_without_their_sas() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox
        .command(&["settings"])
        .env(
            "FETCHLOOM_AZURE_CONTAINERS",
            format!("https://lab.blob.core.windows.net/raw?{SAS}, azure:lab/scans"),
        )
        .output()?;
    let stdout = text(&output.stdout)?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let line = stdout
        .lines()
        .find(|line| line.contains("azure_containers"))
        .unwrap_or_default();
    assert!(
        line.contains("azure:lab/raw") && line.contains("azure:lab/scans"),
        "{stdout}"
    );
    assert_no_sas(&[("stdout".to_owned(), output.stdout)]);
    Ok(())
}

/// a public container answers without a SAS, so none is ever sent, though the container is
/// listed and a SAS is on the machine. a pasted folder link is read as the container's prefix,
/// its blobs are pinned with the MD5 azure states, and a teammate syncs them from the lock.
#[test]
fn a_public_container_is_pinned_with_azures_md5_and_never_gets_a_sas() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        (
            listing("lab", "open", "scans/", None),
            page(
                "open",
                &[
                    ("scans/a.csv", A_CSV.len(), A_MD5),
                    ("scans/b.csv", B_CSV.len(), ""),
                ],
                None,
            ),
        ),
        (blob("lab", "open", "scans/a.csv", None), bytes(A_CSV)),
        (blob("lab", "open", "scans/b.csv", None), bytes(B_CSV)),
    ])?;
    let work = sandbox.work();
    let output = command(
        &sandbox,
        &routes,
        &work,
        &[
            "--json",
            "add",
            "https://lab.blob.core.windows.net/open/scans/",
            "--as",
            "open",
        ],
    )
    .env("FETCHLOOM_AZURE_CONTAINERS", "azure:lab/open")
    .env("AZURE_STORAGE_SAS_TOKEN", SAS)
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("ref = \"azure:lab/open/scans/\""),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("md5 = \"ec88058d417073d50fb3117438bda463\""),
        "{lock}"
    );
    assert_eq!(fs::read(work.join("data/open/b.csv"))?, B_CSV);
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), clone.join(name))?;
    }
    let synced = command(&sandbox, &routes, &clone, &["sync", "--locked"])
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(clone.join("data/open/a.csv"))?, A_CSV);
    let sent = asked(&routes);
    assert_eq!(sent.len(), 5, "{sent:?}");
    for (line, versioned) in sent {
        assert!(versioned && !line.contains("sig="), "{line}");
    }
    Ok(())
}

/// bytes that differ from the MD5 azure states for a blob are refused, and nothing is placed.
#[test]
fn bytes_that_differ_from_azures_md5_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        (
            listing("lab", "open", "scans/", None),
            page("open", &[("scans/a.csv", A_CSV.len(), A_MD5)], None),
        ),
        (
            blob("lab", "open", "scans/a.csv", None),
            bytes(b"site,value\nlab,9\n"),
        ),
    ])?;
    let output = command(
        &sandbox,
        &routes,
        &sandbox.work(),
        &["add", "azure:lab/open/scans/", "--as", "open"],
    )
    .output()?;
    let stderr = text(&output.stderr)?;
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("MD5"), "{stderr}");
    assert!(!sandbox.work().join("data/open/a.csv").exists());
    Ok(())
}

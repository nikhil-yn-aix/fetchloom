//! azure blob containers, read from a stand-in for azure's blob hosts on this machine: public ones
//! with no credential, private ones with the credential the person gave.
#![cfg(debug_assertions)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;

use crate::support::{Fetchloom, Part, Result, Routes, Sandbox, Server, reply, text};

const A_CSV: &[u8] = b"site,value\nlab,1\n";
const A_MD5: &str = "7IgFjUFwc9UPsxF0OL2kYw==";
const B_CSV: &[u8] = b"site,value\nfield,2\n";
const B_MD5: &str = "wDwK+pN4pPcTmE3sjbu0wQ==";
/// a SAS as the azure portal gives one, its values escaped as they come.
pub(crate) const SAS: &str =
    "sv=2022-11-02&sr=c&sp=rl&se=2099-01-01T00%3A00%3A00Z&sig=lab-signature%3D";
/// what of [`SAS`] must never show: its signature, as it comes and unescaped.
pub(crate) const SECRETS: [&str; 2] = ["lab-signature%3D", "lab-signature="];

/// the request target of the listing fetchloom asks azure for, as the stand-in sees it.
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

/// azure's answer to a request for a private container that carries no credential.
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
fn command(sandbox: &Sandbox, routes: &Routes, folder: &Path, args: &[&str]) -> Fetchloom {
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

/// the SAS comes from the environment as the az cli reads it, or a teammate's settings file, and
/// no output, log or file keeps it.
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

/// an account key as the azure portal gives one, the base64 of `KEY_BYTES`.
pub(crate) const KEY: &str = "c2VjcmV0LWtleQ==";
const KEY_BYTES: &[u8] = b"secret-key";

/// base64 with the standard alphabet and padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let joined = chunk.iter().enumerate().fold(0_u32, |joined, (at, byte)| {
            joined | (u32::from(*byte) << (16 - 8 * at))
        });
        for at in 0..4 {
            text.push(if at <= chunk.len() {
                char::from(ALPHABET[((joined >> (18 - 6 * at)) & 63) as usize])
            } else {
                '='
            });
        }
    }
    text
}

/// checks a head's shared key as azure does, for `resource`: account, path and decoded, sorted
/// query. the server lowercases heads, so the date is recased and the signature lowercased too.
fn signed_for(head: &str, resource: &str) -> Result {
    let header = |name: &str| {
        head.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}: ")))
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let date: Vec<String> = header("x-ms-date")
        .split(' ')
        .map(|word| match word {
            "gmt" => "GMT".to_owned(),
            word if word.starts_with(|c: char| c.is_ascii_alphabetic()) => {
                word[..1].to_ascii_uppercase() + &word[1..]
            }
            word => word.to_owned(),
        })
        .collect();
    let signed = format!(
        "GET\n\n\n\n\n\n\n\n\n\n\n\nx-ms-date:{}\nx-ms-version:{}\n{resource}",
        date.join(" "),
        header("x-ms-version")
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(KEY_BYTES).map_err(|_| "no key")?;
    mac.update(signed.as_bytes());
    let expected = format!("sharedkey lab:{}", base64(&mac.finalize().into_bytes()));
    if header("authorization") != expected.to_ascii_lowercase() {
        return Err(format!("the shared key does not match:\n{signed}\n{head}").into());
    }
    Ok(())
}

/// the key comes from the environment as the az cli reads it; the listing is signed once refused
/// and the blob from the start, and the key itself is never sent or shown.
#[test]
fn a_listed_private_container_is_read_with_its_account_key() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/",
        vec![
            unauthenticated(),
            page("raw", &[("scans/a.csv", A_CSV.len(), A_MD5)], None),
            bytes(A_CSV),
        ],
    )?;
    let output = sandbox
        .command(&["-vv", "add", "azure:lab/raw/scans/", "--as", "scans"])
        .env("FETCHLOOM_TEST_BUCKETS", &server.url)
        .env("FETCHLOOM_AZURE_CONTAINERS", "azure:lab/raw")
        .env("AZURE_STORAGE_KEY", KEY)
        .env("AZURE_STORAGE_ACCOUNT", "lab")
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let heads = server.requests();
    assert_eq!(heads.len(), 3, "{heads:?}");
    assert!(!heads[0].contains("authorization"), "{}", heads[0]);
    signed_for(
        &heads[1],
        "/lab/raw\ncomp:list\nprefix:scans/\nrestype:container",
    )?;
    signed_for(&heads[2], "/lab/raw/scans/a.csv")?;
    assert_eq!(fs::read(sandbox.work().join("data/scans/a.csv"))?, A_CSV);
    let key = KEY.to_ascii_lowercase();
    for said in heads.iter().chain([&stderr, &text(&output.stdout)?]) {
        assert!(!said.contains(&key) && !said.contains(KEY), "{said}");
    }
    Ok(())
}

/// with no SAS or key in the environment, `az account get-access-token` runs once from PATH, and
/// its token goes with the listing once refused and with the blob, never shown.
#[cfg(unix)]
#[test]
fn a_listed_private_container_is_read_with_the_az_clis_login() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/",
        vec![
            unauthenticated(),
            page("raw", &[("scans/a.csv", A_CSV.len(), A_MD5)], None),
            bytes(A_CSV),
        ],
    )?;
    let bin = sandbox.folder("bin")?;
    let asked = sandbox.folder("az")?.join("asked");
    crate::buckets::program(
        &bin.join("az"),
        &format!(
            "printf '%s\\n' \"$@\" >> '{}'\nprintf '{{\"accessToken\": \"printed-token\", \"expires_on\": 4102444800, \"tokenType\": \"Bearer\"}}'",
            asked.display()
        ),
    )?;
    let output = sandbox
        .command(&["-vv", "add", "azure:lab/raw/scans/", "--as", "scans"])
        .env("FETCHLOOM_TEST_BUCKETS", &server.url)
        .env("FETCHLOOM_AZURE_CONTAINERS", "azure:lab/raw")
        .env("PATH", &bin)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read_to_string(&asked)?,
        "account\nget-access-token\n--resource\nhttps://storage.azure.com/\n--output\njson\n"
    );
    let heads = server.requests();
    assert_eq!(heads.len(), 3, "{heads:?}");
    assert!(!heads[0].contains("authorization"), "{}", heads[0]);
    for head in &heads[1..] {
        assert!(
            head.contains("\r\nauthorization: bearer printed-token\r\n"),
            "{head}"
        );
    }
    assert_eq!(fs::read(sandbox.work().join("data/scans/a.csv"))?, A_CSV);
    for said in [&stderr, &text(&output.stdout)?] {
        assert!(!said.contains("printed-token"), "{said}");
    }
    Ok(())
}

/// `AZURE_TOKEN_CREDENTIALS` asks for the identity. its token, from the metadata service asked
/// with `metadata: true`, goes with the listing once refused and with the blob, never shown.
#[test]
fn a_listed_private_container_is_read_with_the_machines_identity() -> Result {
    let sandbox = Sandbox::new()?;
    let identity = Routes::start(vec![(
        "/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https%3A%2F%2Fstorage.azure.com%2F".to_owned(),
        reply(
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"access_token":"identity-token","expires_in":"86399","resource":"https://storage.azure.com/","token_type":"Bearer"}"#,
        ),
    )])?;
    let server = Server::start(
        "/",
        vec![
            unauthenticated(),
            page("raw", &[("scans/a.csv", A_CSV.len(), A_MD5)], None),
            bytes(A_CSV),
        ],
    )?;
    let output = sandbox
        .command(&["-vv", "add", "azure:lab/raw/scans/", "--as", "scans"])
        .env("FETCHLOOM_TEST_BUCKETS", &server.url)
        .env(
            "FETCHLOOM_TEST_AZURE_IDENTITY",
            format!("{}/metadata/identity/oauth2/token", identity.url),
        )
        .env("FETCHLOOM_AZURE_CONTAINERS", "azure:lab/raw")
        .env("AZURE_TOKEN_CREDENTIALS", "ManagedIdentityCredential")
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let asked = identity.requests();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].contains("\r\nmetadata: true\r\n"), "{}", asked[0]);
    let heads = server.requests();
    assert_eq!(heads.len(), 3, "{heads:?}");
    assert!(!heads[0].contains("authorization"), "{}", heads[0]);
    for head in &heads[1..] {
        assert!(
            head.contains("\r\nauthorization: bearer identity-token\r\n"),
            "{head}"
        );
    }
    assert_eq!(fs::read(sandbox.work().join("data/scans/a.csv"))?, A_CSV);
    for said in [&stderr, &text(&output.stdout)?] {
        assert!(!said.contains("identity-token"), "{said}");
    }
    Ok(())
}

/// the SAS on the machine stays unsent, and the refusal says how to list the container.
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

/// the refusal says where a SAS belongs without repeating it, and data.toml stays unwritten.
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

/// the azure containers are those a SAS may go to, named by the variable or the file.
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

/// the container answers without a SAS, so none goes, though it is listed and one is on the
/// machine. a pasted folder link is read as the container's prefix.
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

/// nothing of a refused blob is placed.
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

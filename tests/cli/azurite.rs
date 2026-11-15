//! azure blob storage signed for azurite, microsoft's own emulator of it, which checks each shared
//! key and SAS as azure does. a relay stands it in for azure's blob hosts, and the test fills a
//! private container with requests it signs itself.
#![cfg(debug_assertions)]

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;

use crate::azure::base64;
use crate::support::{Relay, Result, Sandbox, json, skipped, text};

/// the account the emulator keeps, and its key: the base64 of `KEY_BYTES`.
const KEY: &str = "ZmV0Y2hsb29tIHRlc3Qga2V5LCBub3QgYSBzZWNyZXQgZm9yIGFueSBhY2NvdW50ISE=";
const KEY_BYTES: &[u8] = b"fetchloom test key, not a secret for any account!!";
/// a key the account never had, and its bytes.
const OTHER_KEY: &str = "YSBrZXkgYXp1cml0ZSB3YXMgbmV2ZXIgZ2l2ZW4sIGZvciB0aGUgcmVmdXNhbA==";
const OTHER_KEY_BYTES: &[u8] = b"a key azurite was never given, for the refusal";
const HOST: &str = "lab.blob.core.windows.net";
/// the private container's blobs: a plain name, and one with a space and a letter outside ascii,
/// which a signature holds escaped.
const BLOBS: [(&str, &[u8]); 2] = [
    ("scans/a.csv", b"site,value\nlab,1\n"),
    ("scans/na\u{ef}ve b.csv", b"site,value\nfield,2\n"),
];

/// azurite's blob service, run from `PATH` with no telemetry and nothing on disk, keeping the
/// account `lab`.
struct Azurite {
    address: String,
    child: Child,
}

impl Azurite {
    /// azurite, with the container `raw` holding [`BLOBS`]; none, said as a skip, where it is
    /// not installed.
    fn start(sandbox: &Sandbox) -> Result<Option<Azurite>> {
        let started = Command::new("azurite-blob")
            .args([
                "--blobHost",
                "127.0.0.1",
                "--blobPort",
                "0",
                "--inMemoryPersistence",
                "--extentMemoryLimit",
                "64",
                "--disableTelemetry",
                "--silent",
            ])
            .env("AZURITE_ACCOUNTS", format!("lab:{KEY}"))
            .current_dir(sandbox.folder("azurite")?)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match started {
            Ok(child) => child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                skipped(
                    "azurite",
                    "no azurite-blob, so no azure emulator to sign for",
                )?;
                return Ok(None);
            }
            Err(err) => return Err(err.into()),
        };
        let said = child.stdout.take().ok_or("azurite has no stdout")?;
        let mut azurite = Azurite {
            address: String::new(),
            child,
        };
        // azurite says where it listens once it does; what it says after is read and dropped
        let mut lines = BufReader::new(said).lines();
        for line in lines.by_ref() {
            if let Some(at) = line?.split("listens on http://").nth(1) {
                at.trim().clone_into(&mut azurite.address);
                break;
            }
        }
        if azurite.address.is_empty() {
            return Err("azurite ended without listening".into());
        }
        std::thread::spawn(move || lines.for_each(drop));
        azurite.put("/raw", "restype=container", &[], b"")?;
        for (name, bytes) in BLOBS {
            let path = format!("/raw/{}", escaped(name));
            azurite.put(&path, "", &[("x-ms-blob-type", "BlockBlob")], bytes)?;
        }
        Ok(Some(azurite))
    }

    /// a PUT of `body` to `path` and `query` on the account's host, signed with its key as azure's
    /// documentation says, which azurite answers with 201.
    fn put(&self, path: &str, query: &str, headers: &[(&str, &str)], body: &[u8]) -> Result {
        let date = httpdate::fmt_http_date(std::time::SystemTime::now());
        let length = body.len().to_string();
        let mut own: Vec<(&str, &str)> = vec![("x-ms-date", &date), ("x-ms-version", "2020-10-02")];
        own.extend_from_slice(headers);
        own.sort_unstable();
        let mut signed = format!(
            "PUT\n\n\n{}\n\n\n\n\n\n\n\n\n",
            if body.is_empty() { "" } else { &length }
        );
        for (name, value) in &own {
            let _ = writeln!(signed, "{name}:{value}");
        }
        let _ = write!(signed, "/lab{path}");
        let mut pairs: Vec<&str> = query.split('&').filter(|pair| !pair.is_empty()).collect();
        pairs.sort_unstable();
        for pair in pairs {
            let _ = write!(signed, "\n{}", pair.replacen('=', ":", 1));
        }
        let target = if query.is_empty() {
            path.to_owned()
        } else {
            format!("{path}?{query}")
        };
        let mut request = format!(
            "PUT {target} HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\nContent-Length: {length}\r\nAuthorization: SharedKey lab:{}\r\n",
            signature(KEY_BYTES, &signed)?
        );
        for (name, value) in &own {
            let _ = write!(request, "{name}: {value}\r\n");
        }
        request.push_str("\r\n");
        let mut stream = std::net::TcpStream::connect(&self.address)?;
        stream.write_all(request.as_bytes())?;
        stream.write_all(body)?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer)?;
        if !answer.starts_with("HTTP/1.1 201") {
            return Err(format!("azurite refused {target}:\n{answer}").into());
        }
        Ok(())
    }
}

impl Drop for Azurite {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// the base64 of the HMAC-SHA256 of `text` under `key`.
fn signature(key: &[u8], text: &str) -> Result<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| "no key")?;
    mac.update(text.as_bytes());
    Ok(base64(&mac.finalize().into_bytes()))
}

/// a blob's name as a link's path writes it: all but the unreserved characters of RFC 3986
/// escaped, as azure's own libraries escape them.
fn escaped(name: &str) -> String {
    const UNRESERVED: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    name.split('/')
        .map(|part| percent_encoding::utf8_percent_encode(part, UNRESERVED).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// a SAS for the container `raw` that lets requests read and list it, as the azure portal makes
/// one with the account key `key`: its service SAS of version 2022-11-02.
fn sas(key: &[u8]) -> Result<String> {
    let signed = [
        "rl",
        "",
        "2099-01-01T00:00:00Z",
        "/blob/lab/raw",
        "",
        "",
        "",
        "2022-11-02",
        "c",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
    ]
    .join("\n");
    let signature: String =
        url::form_urlencoded::byte_serialize(signature(key, &signed)?.as_bytes()).collect();
    Ok(format!(
        "sv=2022-11-02&sr=c&sp=rl&se=2099-01-01T00%3A00%3A00Z&sig={signature}"
    ))
}

/// `add` of the container's `scans/` folder in `folder`, through `relay`, with the credential
/// `set` gives.
fn add(sandbox: &Sandbox, relay: &Relay, folder: &Path, set: &[(&str, &str)]) -> Result<Output> {
    let mut command = sandbox.command_in(
        folder,
        &["--json", "add", "azure:lab/raw/scans/", "--as", "scans"],
    );
    command
        .env("FETCHLOOM_TEST_HOSTS", &relay.url)
        .env("FETCHLOOM_AZURE_CONTAINERS", "azure:lab/raw")
        .envs(set.iter().copied());
    Ok(command.output()?)
}

/// each request the relay took as its target below the host, its SAS taken off, and what
/// credential it carried: the account a shared key signed for, a SAS, or nothing.
fn carried(relay: &Relay) -> Vec<(String, Option<String>)> {
    relay
        .requests()
        .iter()
        .map(|head| {
            let target = head
                .lines()
                .next()
                .and_then(|line| line.split(' ').nth(1))
                .unwrap_or_default();
            let target = target.strip_prefix(&format!("/{HOST}")).unwrap_or(target);
            let key = head
                .lines()
                .find_map(|line| line.strip_prefix("authorization: sharedkey "))
                .and_then(|value| value.split(':').next())
                .map(|account| format!("key of {account}"));
            match target.find("sv=2022-11-02") {
                // the SAS goes last, after a `?` or an `&`
                Some(at) => (target[..at - 1].to_owned(), Some("sas".to_owned())),
                None => (target.to_owned(), key),
            }
        })
        .collect()
}

/// what fetchloom asks of the container: the listing, then each blob, once refused without a
/// credential and then with `credential`.
fn asked(credential: &str) -> Vec<(String, Option<String>)> {
    let listing = "/raw?restype=container&comp=list&prefix=scans%2f";
    let mut asked = vec![
        (listing.to_owned(), None),
        (listing.to_owned(), Some(credential.to_owned())),
    ];
    let mut blobs: Vec<(String, Option<String>)> = BLOBS
        .iter()
        .map(|(name, _)| {
            (
                format!("/raw/{}", escaped(name)).to_ascii_lowercase(),
                Some(credential.to_owned()),
            )
        })
        .collect();
    blobs.sort();
    asked.extend(blobs);
    asked
}

/// the blobs `add` placed in `folder`, by name.
fn placed(folder: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    BLOBS
        .iter()
        .map(|(name, _)| {
            let name = name.trim_start_matches("scans/");
            Ok((
                name.to_owned(),
                std::fs::read(folder.join("data/scans").join(name))?,
            ))
        })
        .collect()
}

fn expected() -> Vec<(String, Vec<u8>)> {
    BLOBS
        .iter()
        .map(|(name, bytes)| (name.trim_start_matches("scans/").to_owned(), bytes.to_vec()))
        .collect()
}

/// a listing and the downloads after it, each signed with the account's key, are what azurite
/// takes; signed with a key the account never had, the listing is refused, and nothing is placed.
#[test]
fn azurite_takes_requests_signed_with_the_account_key() -> Result {
    let sandbox = Sandbox::new()?;
    let Some(azurite) = Azurite::start(&sandbox)? else {
        return Ok(());
    };
    let wrong = Relay::start(&azurite.address)?;
    let refused = sandbox.folder("refused")?;
    let output = add(
        &sandbox,
        &wrong,
        &refused,
        &[
            ("AZURE_STORAGE_ACCOUNT", "lab"),
            ("AZURE_STORAGE_KEY", OTHER_KEY),
        ],
    )?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["errors"][0]["message"],
        "azure refused the key in AZURE_STORAGE_KEY for azure:lab/raw/scans/"
    );
    assert_eq!(carried(&wrong), asked("key of lab")[..2]);
    assert!(!refused.join("data").exists());
    let relay = Relay::start(&azurite.address)?;
    let output = add(
        &sandbox,
        &relay,
        &sandbox.work(),
        &[("AZURE_STORAGE_ACCOUNT", "lab"), ("AZURE_STORAGE_KEY", KEY)],
    )?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(placed(&sandbox.work())?, expected());
    let mut sent = carried(&relay);
    sent[2..].sort();
    assert_eq!(sent, asked("key of lab"));
    for head in relay.requests() {
        assert!(!head.contains(&KEY.to_ascii_lowercase()), "{head}");
    }
    Ok(())
}

/// a listing and the downloads after it with a SAS the account's key signed are what azurite
/// takes; with one another key signed, the listing is refused, and nothing is placed.
#[test]
fn azurite_takes_requests_with_a_sas_its_key_signed() -> Result {
    let sandbox = Sandbox::new()?;
    let Some(azurite) = Azurite::start(&sandbox)? else {
        return Ok(());
    };
    let wrong = Relay::start(&azurite.address)?;
    let refused = sandbox.folder("refused")?;
    let forged = sas(OTHER_KEY_BYTES)?;
    let output = add(
        &sandbox,
        &wrong,
        &refused,
        &[("AZURE_STORAGE_SAS_TOKEN", &forged)],
    )?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["errors"][0]["message"],
        "azure refused the SAS in AZURE_STORAGE_SAS_TOKEN for azure:lab/raw/scans/"
    );
    assert_eq!(carried(&wrong), asked("sas")[..2]);
    assert!(!refused.join("data").exists());
    let relay = Relay::start(&azurite.address)?;
    let signed = sas(KEY_BYTES)?;
    let output = add(
        &sandbox,
        &relay,
        &sandbox.work(),
        &[("AZURE_STORAGE_SAS_TOKEN", &signed)],
    )?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(placed(&sandbox.work())?, expected());
    let mut sent = carried(&relay);
    sent[2..].sort();
    assert_eq!(sent, asked("sas"));
    let said = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
    let signature = signed.split("sig=").nth(1).unwrap_or_default();
    assert!(!said.contains(signature), "{said}");
    Ok(())
}

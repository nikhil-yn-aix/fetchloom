//! the cookies chromium's browsers keep, chrome, chromium, brave, edge, vivaldi and opera, on
//! linux and macos: a `Cookies` database whose values are encrypted with AES-128 in CBC mode,
//! under a key derived from a secret the system's credential store keeps.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::sqlite::{Database, Value};
use super::{Left, Read, Why, covers};
use crate::auth::site::Cookie;
use crate::machine::Var;

/// the salt chromium derives every cookie key with.
const SALT: &[u8] = b"saltysalt";
/// the vector chromium starts every cookie's CBC chain with: sixteen spaces.
const VECTOR: [u8; 16] = [b' '; 16];
/// seconds between 1601, where chromium counts its times from, and 1970.
const EPOCHS_APART: i64 = 11_644_473_600;
/// the database version from which a value starts with the SHA-256 of its cookie's domain.
const DOMAIN_HASHED: i64 = 24;

/// a browser built on chromium: where it keeps its profiles, and the names its key goes by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Chromium {
    pub(crate) name: &'static str,
    /// its folder in the linux configuration folder.
    linux: &'static str,
    /// its folder in macos's `Library/Application Support`.
    macos: &'static str,
    /// the `application` its key is kept for in the linux secret service.
    application: &'static str,
    /// the keychain service and account its key is kept under on macos.
    keychain: (&'static str, &'static str),
}

/// the chromium browsers fetchloom reads, with the names chromium's own code and each browser's
/// changes to it give their keys.
pub(crate) const BROWSERS: [Chromium; 6] = [
    Chromium {
        name: "chrome",
        linux: "google-chrome",
        macos: "Google/Chrome",
        application: "chrome",
        keychain: ("Chrome Safe Storage", "Chrome"),
    },
    Chromium {
        name: "chromium",
        linux: "chromium",
        macos: "Chromium",
        application: "chromium",
        keychain: ("Chromium Safe Storage", "Chromium"),
    },
    Chromium {
        name: "brave",
        linux: "BraveSoftware/Brave-Browser",
        macos: "BraveSoftware/Brave-Browser",
        application: "brave",
        keychain: ("Brave Safe Storage", "Brave"),
    },
    Chromium {
        name: "edge",
        linux: "microsoft-edge",
        macos: "Microsoft Edge",
        application: "chromium",
        keychain: ("Microsoft Edge Safe Storage", "Microsoft Edge"),
    },
    Chromium {
        name: "vivaldi",
        linux: "vivaldi",
        macos: "Vivaldi",
        application: "chrome",
        keychain: ("Vivaldi Safe Storage", "Vivaldi"),
    },
    Chromium {
        name: "opera",
        linux: "opera",
        macos: "com.operasoftware.Opera",
        application: "chromium",
        keychain: ("Opera Safe Storage", "Opera"),
    },
];

impl Chromium {
    /// the folder the browser keeps its profiles in on this system, or none where fetchloom
    /// does not read it.
    pub(crate) fn root(&self, var: Var<'_>) -> Option<PathBuf> {
        let home = crate::machine::home(var)?;
        if cfg!(target_os = "macos") {
            return Some(home.join("Library/Application Support").join(self.macos));
        }
        if cfg!(windows) {
            return None;
        }
        let config = var("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        Some(config.join(self.linux))
    }

    /// the key the browser encrypts the cookies of `tag` with, from its secret in the system's
    /// credential store: `v11` on linux, `v10` on macos. on linux, `v10` is chromium's fixed key.
    pub(crate) fn key(&self, tag: &str) -> Option<[u8; 16]> {
        if cfg!(target_os = "macos") {
            let (service, account) = self.keychain;
            let secret = (tag == "v10")
                .then(|| {
                    keyring::Entry::new(service, account)
                        .ok()?
                        .get_secret()
                        .ok()
                })
                .flatten()?;
            return Some(derive(&secret, 1003));
        }
        match tag {
            "v10" => Some(derive(b"peanuts", 1)),
            "v11" => {
                // the store keyring sets up; its search finds a key by the attributes chromium
                // gives it
                keyring::Entry::store_status().as_ref().ok()?;
                let found = keyring_core::Entry::search(&HashMap::from([(
                    "application",
                    self.application,
                )]))
                .ok()?;
                let secret = found.first()?.get_secret().ok()?;
                Some(derive(&secret, 1))
            }
            _ => None,
        }
    }
}

/// the AES-128 key PBKDF2 with HMAC-SHA1 derives from `secret` over chromium's salt.
pub(crate) fn derive(secret: &[u8], iterations: u32) -> [u8; 16] {
    let mut key = [0; 16];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA1,
        std::num::NonZeroU32::new(iterations.max(1)).unwrap_or(std::num::NonZeroU32::MIN),
        SALT,
        secret,
        &mut key,
    );
    key
}

/// the cookie database of the profile `named` asks for in the browser's folder `root`: a
/// profile's folder name there, as `Profile 1`, or a folder by its absolute path; without a
/// name, the one the browser wrote to last.
pub(crate) fn file(root: &Path, named: Option<&str>) -> Option<PathBuf> {
    let in_profile = |folder: &Path| {
        [
            folder.join("Network").join("Cookies"),
            folder.join("Cookies"),
        ]
        .into_iter()
        .find(|file| file.is_file())
    };
    if let Some(named) = named {
        let named = Path::new(named);
        if named.is_absolute() {
            return in_profile(named);
        }
        // a name is never a path from where fetchloom runs, which a cloned repo could fill, nor
        // one that leaves the browser's folder
        let mut parts = named.components();
        return match (parts.next(), parts.next()) {
            (Some(std::path::Component::Normal(_)), None) => in_profile(&root.join(named)),
            _ => None,
        };
    }
    let mut found: Vec<PathBuf> = in_profile(root).into_iter().collect();
    if let Ok(entries) = std::fs::read_dir(root) {
        found.extend(
            entries
                .flatten()
                .filter_map(|entry| in_profile(&entry.path())),
        );
    }
    super::newest(found)
}

/// the cookies for `host` in the chromium database at `path`, each value decrypted with the key
/// `key` gives for its version tag, asked for once a tag and only for a cookie the host takes.
/// another host's cookies are left out unread, and those the browser keeps for another site's
/// frames, or that no key opens, are left out by name.
pub(crate) fn read(
    path: &Path,
    host: &str,
    key: &dyn Fn(&str) -> Option<[u8; 16]>,
) -> std::io::Result<Read> {
    taken(&Database::open(path)?, host, key)
        .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidData, why.0))
}

/// the cookies for `host` in `database`, a chromium `Cookies` file, as [`read`] takes them.
pub(super) fn taken(
    database: &Database,
    host: &str,
    key: &dyn Fn(&str) -> Option<[u8; 16]>,
) -> Result<Read, super::sqlite::Broken> {
    let version = database
        .rows("meta", &["key", "value"])?
        .iter()
        .find(|row| row[0].text() == Some("version"))
        .and_then(|row| row[1].text().and_then(|text| text.parse().ok()))
        .unwrap_or(0);
    let rows = database.rows(
        "cookies",
        &[
            "host_key",
            "name",
            "encrypted_value",
            "path",
            "expires_utc",
            "has_expires",
            "is_secure",
            "secure",
            "top_frame_site_key",
        ],
    )?;
    let mut keys: HashMap<String, Option<[u8; 16]>> = HashMap::new();
    let mut read = Read::default();
    for row in rows {
        let text = |at: usize| row[at].text().unwrap_or_default().to_owned();
        let (domain, name) = (text(0), text(1));
        let left = |why| Left {
            name: name.clone(),
            domain: domain.clone(),
            why,
        };
        if !covers(&domain, host) {
            read.left.push(left(Why::Elsewhere));
            continue;
        }
        if !text(8).is_empty() {
            read.left.push(left(Why::Apart));
            continue;
        }
        let encrypted = match &row[2] {
            Value::Blob(bytes) => bytes.as_slice(),
            _ => &[],
        };
        // chromium seals every cookie it keeps, so a plain one was put there by someone else
        if encrypted.is_empty() {
            read.left.push(left(Why::Unsealed));
            continue;
        }
        let tag = String::from_utf8_lossy(&encrypted[..encrypted.len().min(3)]).into_owned();
        let key = *keys.entry(tag.clone()).or_insert_with(|| key(&tag));
        let Some(value) = opened(encrypted, key, &domain, version) else {
            read.left.push(left(Why::Locked));
            continue;
        };
        let expires = (row[5].integer() != Some(0))
            .then(|| row[4].integer())
            .flatten()
            .filter(|micros| *micros > 0)
            .map(|micros| micros / 1_000_000 - EPOCHS_APART);
        let secure = row[6].integer().or_else(|| row[7].integer()) == Some(1);
        read.found.push(Cookie {
            name,
            value,
            domain,
            path: text(3),
            secure,
            expires,
        });
    }
    Ok(read)
}

/// the value an encrypted cookie for `domain` holds, opened with `key`: its version tag, then
/// AES-128 in CBC mode, and from database version 24 the SHA-256 of the domain before the
/// value, which a value opened with the wrong key could not match. chromium on linux tries the
/// key of an empty secret when its own fails, and so does this.
fn opened(encrypted: &[u8], key: Option<[u8; 16]>, domain: &str, version: i64) -> Option<String> {
    use aes::cipher::block_padding::Pkcs7;
    use aes::cipher::{BlockModeDecrypt as _, KeyIvInit as _};
    let sealed = encrypted.get(3..)?;
    let decrypt = |key: &[u8; 16]| {
        cbc::Decryptor::<aes::Aes128>::new(key.into(), &VECTOR.into())
            .decrypt_padded_vec::<Pkcs7>(sealed)
            .ok()
    };
    let unhashed = |plain: Vec<u8>| -> Option<String> {
        let plain = if version >= DOMAIN_HASHED {
            let hash = crate::digest::sha256(domain.as_bytes());
            plain.strip_prefix(hash.bytes().as_slice())?.to_vec()
        } else {
            plain
        };
        String::from_utf8(plain).ok()
    };
    let empty = (!cfg!(target_os = "macos")).then(|| derive(b"", 1));
    key.iter()
        .chain(empty.iter())
        .find_map(|key| decrypt(key).and_then(unhashed))
}

#[cfg(test)]
mod tests;

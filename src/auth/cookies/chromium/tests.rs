use std::cell::RefCell;

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockModeEncrypt as _, KeyIvInit as _};
use rusqlite::Connection;

use super::*;

/// a value as chromium seals it with `key` under `tag`: from version 24 after the SHA-256 of its
/// cookie's domain, then AES-128 in CBC mode over sixteen spaces.
fn sealed(tag: &str, key: &[u8; 16], domain: &str, value: &str, version: i64) -> Vec<u8> {
    let mut plain = Vec::new();
    if version >= DOMAIN_HASHED {
        plain.extend_from_slice(crate::digest::sha256(domain.as_bytes()).bytes());
    }
    plain.extend_from_slice(value.as_bytes());
    let sealed = cbc::Encryptor::<aes::Aes128>::new(key.into(), &VECTOR.into())
        .encrypt_padded_vec::<Pkcs7>(&plain);
    [tag.as_bytes(), &sealed].concat()
}

/// a cookie row: domain, name, plain value, sealed value, expiry in chromium's microseconds and
/// partition key.
type Row<'a> = (&'a str, &'a str, &'a str, Vec<u8>, i64, &'a str);

/// a chromium `Cookies` database at `path`, with the schema of database version `version`.
fn database(path: &Path, version: i64, rows: &[Row<'_>]) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR); \
         CREATE TABLE cookies(creation_utc INTEGER NOT NULL,host_key TEXT NOT NULL,top_frame_site_key TEXT NOT NULL,\
         name TEXT NOT NULL,value TEXT NOT NULL,encrypted_value BLOB NOT NULL,path TEXT NOT NULL,\
         expires_utc INTEGER NOT NULL,is_secure INTEGER NOT NULL,is_httponly INTEGER NOT NULL,\
         last_access_utc INTEGER NOT NULL,has_expires INTEGER NOT NULL,is_persistent INTEGER NOT NULL,\
         priority INTEGER NOT NULL,samesite INTEGER NOT NULL,source_scheme INTEGER NOT NULL,\
         source_port INTEGER NOT NULL,last_update_utc INTEGER NOT NULL,source_type INTEGER NOT NULL,\
         has_cross_site_ancestor INTEGER NOT NULL);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO meta VALUES ('version', ?1)",
        [version.to_string()],
    )
    .unwrap();
    for (domain, name, value, encrypted, expires, partition) in rows {
        db.execute(
            "INSERT INTO cookies VALUES (0, ?1, ?2, ?3, ?4, ?5, '/', ?6, 1, 1, 0, ?7, 1, 1, 0, 2, 443, 0, 0, 0)",
            rusqlite::params![domain, partition, name, value, encrypted, expires, i64::from(*expires > 0)],
        )
        .unwrap();
    }
}

/// chromium's own constants, from `components/os_crypt/async/browser/posix_key_provider.cc` and
/// `components/os_crypt/async/common/encryptor.cc`.
#[test]
fn keys_derive_as_chromium_derives_them() {
    assert_eq!(
        derive(b"peanuts", 1),
        [
            0xfd, 0x62, 0x1f, 0xe5, 0xa2, 0xb4, 0x02, 0x53, 0x9d, 0xfa, 0x14, 0x7c, 0xa9, 0x27,
            0x27, 0x78
        ]
    );
    assert_eq!(
        derive(b"", 1),
        [
            0xd0, 0xd0, 0xec, 0x9c, 0x7d, 0x77, 0xd4, 0x3a, 0xc5, 0x41, 0x87, 0xfa, 0x48, 0x18,
            0xd1, 0x7f
        ]
    );
}

#[test]
fn a_hosts_cookies_open_with_the_key_of_their_tag_and_no_other_is_asked() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("Cookies");
    let peanuts = derive(b"peanuts", 1);
    let stored = derive(b"from the keyring", 1);
    let wrong = derive(b"another key", 1);
    let later = (2_000_000_000 + EPOCHS_APART) * 1_000_000;
    let host = "data.example.org";
    database(
        &path,
        24,
        &[
            (".example.org", "plain", "p-value", Vec::new(), 0, ""),
            (
                host,
                "old",
                "",
                sealed("v10", &peanuts, host, "o-value", 24),
                later,
                "",
            ),
            (
                ".example.org",
                "new",
                "",
                sealed("v11", &stored, ".example.org", "n-value", 24),
                later,
                "",
            ),
            (
                host,
                "wrong",
                "",
                sealed("v11", &wrong, host, "w-value", 24),
                later,
                "",
            ),
            (
                host,
                "framed",
                "",
                sealed("v10", &peanuts, host, "f-value", 24),
                later,
                "https://other.example",
            ),
            (
                "other.example",
                "away",
                "",
                sealed("v99", &wrong, "other.example", "a", 24),
                later,
                "",
            ),
        ],
    );
    let asked = RefCell::new(Vec::new());
    let key = |tag: &str| {
        asked.borrow_mut().push(tag.to_owned());
        match tag {
            "v10" => Some(peanuts),
            "v11" => Some(stored),
            _ => None,
        }
    };
    let read = read(&path, host, &key).unwrap();
    let found: Vec<_> = read
        .found
        .iter()
        .map(|c| (c.name.as_str(), c.value.as_str(), c.expires, c.secure))
        .collect();
    assert_eq!(
        found,
        [
            ("plain", "p-value", None, true),
            ("old", "o-value", Some(2_000_000_000), true),
            ("new", "n-value", Some(2_000_000_000), true),
        ]
    );
    let left: Vec<_> = read.left.iter().map(|l| (l.name.as_str(), l.why)).collect();
    assert_eq!(
        left,
        [
            ("wrong", Why::Locked),
            ("framed", Why::Apart),
            ("away", Why::Elsewhere)
        ]
    );
    assert_eq!(
        *asked.borrow(),
        ["v10", "v11"],
        "each tag once, and none for another host"
    );
}

/// before version 24 no domain hash comes first; from it, a value without the hash is refused,
/// as chromium refuses it, so a wrong key whose padding happens to fit opens nothing.
#[test]
fn the_domain_hash_comes_first_from_version_24() {
    let folder = tempfile::tempdir().unwrap();
    let key = derive(b"peanuts", 1);
    for (version, hashed, opens) in [(23, false, true), (24, true, true), (24, false, false)] {
        let path = folder.path().join(format!("{version}-{hashed}"));
        let sealing = if hashed { 24 } else { 23 };
        database(
            &path,
            version,
            &[(
                "h.example",
                "c",
                "",
                sealed("v10", &key, "h.example", "value", sealing),
                0,
                "",
            )],
        );
        let read = read(&path, "h.example", &|_| Some(key)).unwrap();
        assert_eq!(read.found.len(), usize::from(opens), "{version} {hashed}");
        if opens {
            assert_eq!(read.found[0].value, "value");
        }
    }
}

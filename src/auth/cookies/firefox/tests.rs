use std::ffi::OsString;
use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use super::*;

/// a firefox profile's `cookies.sqlite` in `folder`, at schema `version`, holding `rows` of
/// name, host, expiry, whether secure, and origin attributes.
fn profile(folder: &Path, version: u32, rows: &[(&str, &str, i64, bool, &str)]) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let path = folder.join("cookies.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(&format!(
        "PRAGMA journal_mode = WAL; PRAGMA user_version = {version}; \
         CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL DEFAULT '', \
         name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, lastAccessed INTEGER, \
         creationTime INTEGER, isSecure INTEGER, isHttpOnly INTEGER, inBrowserElement INTEGER DEFAULT 0, \
         sameSite INTEGER DEFAULT 0, schemeMap INTEGER DEFAULT 0, isPartitionedAttributeSet INTEGER DEFAULT 0, \
         updateTime INTEGER, CONSTRAINT moz_uniqueid UNIQUE (name, host, path, originAttributes))"
    ))
    .unwrap();
    for (name, host, expiry, secure, attributes) in rows {
        db.execute(
            "INSERT INTO moz_cookies (originAttributes, name, value, host, path, expiry, isSecure) \
             VALUES (?1, ?2, ?3, ?4, '/', ?5, ?6)",
            rusqlite::params![
                attributes,
                name,
                format!("{name}-secret"),
                host,
                expiry,
                secure
            ],
        )
        .unwrap();
    }
    path
}

#[test]
fn firefox_cookies_read_with_their_expiry_and_containers_left_apart() {
    let home = tempfile::tempdir().unwrap();
    let path = profile(
        &home.path().join("a.default-release"),
        17,
        &[
            ("session", ".example.org", 2_000_000_000_000, true, ""),
            ("own", "data.example.org", 1_900_000_000_000, false, ""),
            (
                "work",
                "data.example.org",
                1_900_000_000_000,
                false,
                "^userContextId=1",
            ),
            (
                "framed",
                "data.example.org",
                1_900_000_000_000,
                false,
                "^partitionKey=%28https%2Cother.example%29",
            ),
        ],
    );
    let read = read(&path).unwrap();
    let found: Vec<_> = read
        .found
        .iter()
        .map(|c| (c.name.as_str(), c.domain.as_str(), c.expires, c.secure))
        .collect();
    assert_eq!(
        found,
        [
            ("session", ".example.org", Some(2_000_000_000), true),
            ("own", "data.example.org", Some(1_900_000_000), false),
        ]
    );
    assert_eq!(read.found[0].value, "session-secret");
    let apart: Vec<_> = read.left.iter().map(|l| (l.name.as_str(), l.why)).collect();
    assert_eq!(apart, [("work", Why::Apart), ("framed", Why::Apart)]);
    // before schema 16 firefox kept an expiry in seconds
    let old = profile(
        &home.path().join("b.old"),
        15,
        &[("session", ".example.org", 2_000_000_000, true, "")],
    );
    assert_eq!(read_expiry(&old), Some(2_000_000_000));
}

fn read_expiry(path: &Path) -> Option<i64> {
    read(path).unwrap().found[0].expires
}

#[test]
fn the_profile_used_last_is_read_unless_one_is_named() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("firefox");
    let root = root.as_path();
    let used = profile(&root.join("Profiles/abc123.default-release"), 17, &[]);
    let older = profile(&root.join("Profiles/def456.work"), 17, &[]);
    let elsewhere = profile(&base.path().join("elsewhere/xyz789.lab"), 17, &[]);
    std::fs::write(
        root.join("profiles.ini"),
        format!(
            "[Profile1]\nName=lab\nIsRelative=0\nPath={}\n\n[Profile0]\nName=default-release\nIsRelative=1\nPath=Profiles/abc123.default-release\n",
            elsewhere.parent().unwrap().display()
        ),
    )
    .unwrap();
    // times fat can hold: from 1980, in steps of two seconds
    let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
    for (file, changed) in [
        (&used, 1_700_000_200),
        (&older, 1_700_000_100),
        (&elsewhere, 1_700_000_000),
    ] {
        std::fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(at(changed))
            .unwrap();
    }
    let roots = [root.to_path_buf()];
    assert_eq!(file(&roots, None), Some(used.clone()));
    assert_eq!(file(&roots, Some("work")), Some(older.clone()));
    assert_eq!(file(&roots, Some("def456.work")), Some(older));
    assert_eq!(file(&roots, Some("lab")), Some(elsewhere.clone()));
    let folder = elsewhere.parent().unwrap().to_str().unwrap().to_owned();
    assert_eq!(file(&[], Some(&folder)), Some(elsewhere));
    assert_eq!(file(&roots, Some("nobody")), None);
}

/// an `XDG_CONFIG_HOME` counts on linux only when it is absolute, as the XDG base directory
/// spec says.
#[test]
fn firefox_is_looked_for_where_each_system_keeps_it() {
    let found = |config: Option<&str>| {
        roots(&|name: &str| {
            match name {
                "HOME" => Some("/home/someone"),
                "XDG_CONFIG_HOME" => config,
                "APPDATA" => Some(r"C:\Users\someone\AppData\Roaming"),
                "LOCALAPPDATA" => Some(r"C:\Users\someone\AppData\Local"),
                _ => None,
            }
            .map(OsString::from)
        })
    };
    let linux = |config| {
        vec![
            config,
            "/home/someone/.mozilla/firefox",
            "/home/someone/.var/app/org.mozilla.firefox/config/mozilla/firefox",
            "/home/someone/.var/app/org.mozilla.firefox/.mozilla/firefox",
            "/home/someone/snap/firefox/common/.mozilla/firefox",
        ]
    };
    let default = "/home/someone/.config/mozilla/firefox";
    for (config, folders) in [
        (None, linux(default)),
        (Some(""), linux(default)),
        (Some("config"), linux(default)),
        (Some("/xdg"), linux("/xdg/mozilla/firefox")),
    ] {
        let folders = if cfg!(windows) {
            vec![
                r"C:\Users\someone\AppData\Roaming\Mozilla\Firefox",
                r"C:\Users\someone\AppData\Local\Packages\Mozilla.Firefox_n80bbvh6b1yt2\LocalCache\Roaming\Mozilla\Firefox",
            ]
        } else if cfg!(target_os = "macos") {
            vec!["/home/someone/Library/Application Support/Firefox"]
        } else {
            folders
        };
        let expected: Vec<PathBuf> = folders.into_iter().map(PathBuf::from).collect();
        assert_eq!(found(config), expected, "XDG_CONFIG_HOME {config:?}");
    }
}

/// a profile's name in profiles.ini need not match its folder, which is a path from the root
/// unless `IsRelative=0` makes it absolute.
#[test]
fn profiles_ini_names_each_profile_and_finds_its_folder() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("firefox");
    let near = root.join("Profiles/k3j4h5g6.default-release");
    let far = base.path().join("lab/p0o9i8u7.field");
    for folder in [&near, &far] {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join("cookies.sqlite"), b"").unwrap();
    }
    std::fs::write(
        root.join("profiles.ini"),
        format!(
            "[Install4F96D1932A9F858E]\nDefault=Profiles/k3j4h5g6.default-release\n\n\
             [Profile1]\nName=field notes\nIsRelative=0\nPath={}\n\n\
             [Profile0]\nName=main\nIsRelative=1\nPath=Profiles/k3j4h5g6.default-release\n\n\
             [General]\nStartWithLastProfile=1\nVersion=2\n",
            far.display()
        ),
    )
    .unwrap();
    assert_eq!(
        profiles(&root),
        [
            (far, Some("field notes".to_owned())),
            (near, Some("main".to_owned()))
        ]
    );
}

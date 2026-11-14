//! the readers on cookie databases as browsers leave them after months of use, built here by
//! SQLite itself: thousands of rows over a tree of interior pages, values longer than a page on
//! chains of overflow pages, pages of 4096 and 32768 bytes, a third of the rows deleted onto the
//! free list, an index beside the table, columns a migration added after rows were written and,
//! in WAL mode, commits still in the log after one that restarted it. what the readers return,
//! row by row and for a host, must be what SQLite returns.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockModeEncrypt as _, KeyIvInit as _};
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use sha2::Digest as _;

use super::sqlite::{Database, Value};
use super::{Cookie, chromium, firefox, pick};

/// the host whose cookies the readers pick.
const HOST: &str = "data.example.org";
/// the rows each database starts with; a later commit adds a hundred more.
const ROWS: i64 = 6_000;
/// the page sizes browsers' databases use, each with and without a write-ahead log.
const SHAPES: [(u32, bool); 4] = [
    (4_096, false),
    (4_096, true),
    (32_768, false),
    (32_768, true),
];
/// the domains of the rows in turn: the host, domains that cover it, and some that must not.
const DOMAINS: [&str; 8] = [
    HOST,
    ".example.org",
    ".data.example.org",
    "other.example.org",
    ".other.org",
    "example.org",
    "data.example.org.evil.test",
    "127.0.0.1",
];
/// chromium's key without a keyring, PBKDF2-HMAC-SHA1 of "peanuts" over "saltysalt", once.
const KEY: [u8; 16] = [
    0xfd, 0x62, 0x1f, 0xe5, 0xa2, 0xb4, 0x02, 0x53, 0x9d, 0xfa, 0x14, 0x7c, 0xa9, 0x27, 0x27, 0x78,
];
/// seconds from 1601, where chromium counts from, to 1970.
const EPOCHS_APART: i64 = 11_644_473_600;

fn at(row: i64) -> usize {
    usize::try_from(row).unwrap()
}

/// the value of row `row`: mostly short, as cookies are, with some longer than any page, up to
/// a chain of overflow pages at the largest page size.
fn value(row: i64) -> String {
    let length = match row {
        _ if row % 997 == 0 => 100_000,
        _ if row % 97 == 0 => 40_000,
        _ if row % 31 == 0 => 9_000,
        _ => at(row * 37 % 1_200 + 1),
    };
    let alphabet = "abcdefghijklmnopqrstuvwxyz0123456789";
    let start = at(row) % alphabet.len();
    alphabet.repeat(length / alphabet.len() + 2)[start..start + length].to_owned()
}

/// when row `row` expires, in seconds after 1970: long past, never stated, or in 2033.
fn expires(row: i64) -> i64 {
    match row {
        _ if row % 17 == 0 => 1_000_000_000,
        _ if row % 19 == 0 => 0,
        _ => 2_000_000_000 + row,
    }
}

/// what a browser keeps apart, for a container or another site's frames, for row `row`.
fn apart(row: i64) -> &'static str {
    match row {
        _ if row % 11 == 0 => "^userContextId=2",
        _ if row % 13 == 0 => "^partitionKey=%28https%2Cexample.com%29",
        _ => "",
    }
}

fn path_of(row: i64) -> &'static str {
    ["/", "/files", "/files/deep"][at(row % 3)]
}

fn domain_of(row: i64) -> &'static str {
    DOMAINS[at(row % 8)]
}

/// a database at `path` with pages of `page_size` bytes, in WAL mode when `wal`, that SQLite
/// never checkpoints on its own.
fn opened(path: &Path, page_size: u32, wal: bool) -> Connection {
    let db = Connection::open(path).unwrap();
    let journal = if wal { "WAL" } else { "DELETE" };
    db.execute_batch(&format!(
        "PRAGMA page_size = {page_size}; PRAGMA journal_mode = {journal}; \
         PRAGMA wal_autocheckpoint = 0;"
    ))
    .unwrap();
    db
}

/// the months a browser's database lives through, in commits: `insert` adds rows in six, the
/// first before `migrate` changes the table, a checkpoint lets the next commit start the log
/// again, a third of the rows go, `change` grows and shrinks values twice over, and a hundred
/// rows come where the deleted ones were.
fn aged(
    db: &Connection,
    table: &str,
    insert: &dyn Fn(&Connection, i64),
    migrate: &dyn Fn(&Connection),
    change: &dyn Fn(&Connection, i64),
) {
    for batch in 0..6 {
        let commit = db.unchecked_transaction().unwrap();
        for row in batch * 1_000..(batch + 1) * 1_000 {
            insert(&commit, row);
        }
        commit.commit().unwrap();
        if batch == 0 {
            migrate(db);
        }
    }
    db.execute_batch("PRAGMA wal_checkpoint(PASSIVE);").unwrap();
    db.execute(&format!("DELETE FROM {table} WHERE rowid % 3 = 0"), [])
        .unwrap();
    for step in [10, 9] {
        let commit = db.unchecked_transaction().unwrap();
        for row in (1..ROWS).filter(|row| row % step == 1) {
            change(&commit, row);
        }
        commit.commit().unwrap();
    }
    let commit = db.unchecked_transaction().unwrap();
    for row in ROWS..ROWS + 100 {
        insert(&commit, row);
    }
    commit.commit().unwrap();
}

/// what a value SQLite returns is, in this reader's terms.
fn valued(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(integer) => Value::Integer(integer),
        ValueRef::Real(real) => Value::Real(real),
        ValueRef::Text(text) => Value::Text(String::from_utf8(text.to_vec()).unwrap()),
        ValueRef::Blob(blob) => Value::Blob(blob.to_vec()),
    }
}

/// every row of `table` with every column, in rowid order, as SQLite reads them.
fn sqlite_rows(db: &Connection, table: &str) -> (Vec<String>, Vec<Vec<Value>>) {
    let mut statement = db
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
        .unwrap();
    let names: Vec<String> = statement.column_names().map(str::to_owned).collect();
    let rows = statement
        .query_map([], |row| {
            (0..names.len())
                .map(|column| row.get_ref(column).map(valued))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (names, rows)
}

/// a count SQLite gives.
fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// checks, through SQLite's own account of its pages, that the database at `path` holds what
/// it stands for: a table more than one page deep, overflow chains, a free list, an index of
/// its own and, in WAL mode, commits the file alone lacks after a restarted log.
fn shaped(
    db: &Connection,
    path: &Path,
    (table, index, values): (&str, &str, &str),
    page_size: u32,
) {
    let pages = |kind: &str, extra: &str| {
        count(
            db,
            &format!(
                "SELECT count(*) FROM dbstat WHERE name = '{table}' AND pagetype = '{kind}'{extra}"
            ),
        )
    };
    assert!(pages("internal", "") > 0, "{page_size}: no interior page");
    if page_size == 4_096 {
        assert!(
            pages("internal", " AND path != '/'") > 0,
            "the tree is three pages deep"
        );
    }
    assert!(pages("overflow", "") > 0, "{page_size}: no overflow page");
    let longest = count(db, &format!("SELECT max(length({values})) FROM {table}"));
    assert!(longest > 2 * i64::from(page_size), "a chain of 3 pages");
    assert!(count(db, "PRAGMA freelist_count") > 0, "no free list");
    let indexed = count(
        db,
        &format!("SELECT count(*) FROM dbstat WHERE name = '{index}'"),
    );
    assert!(indexed > 0, "no index pages");
    let mut log = path.as_os_str().to_owned();
    log.push("-wal");
    let Ok(log) = std::fs::read(log) else {
        return;
    };
    // the log started again after the checkpoint, so its header counts one
    assert!(u32::from_be_bytes(log[12..16].try_into().unwrap()) >= 1);
    // the file alone holds the checkpointed rows, not those the log adds and deletes
    let folder = tempfile::tempdir().unwrap();
    let alone = folder.path().join("alone.sqlite");
    std::fs::copy(path, &alone).unwrap();
    let rows = |db: &Connection| count(db, &format!("SELECT count(*) FROM {table}"));
    assert_ne!(rows(&Connection::open(&alone).unwrap()), rows(db));
}

/// the cookies SQLite's rows of `table` give `HOST`, in rowid order, from a query that matches
/// domains as RFC 6265 does: the host itself, or a domain with a leading dot above it.
fn sqlite_cookies(
    db: &Connection,
    sql: &str,
    open: &dyn Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Cookie>,
) -> Vec<Cookie> {
    let now = SystemTime::now();
    let mut statement = db.prepare(sql).unwrap();
    statement
        .query_map(rusqlite::named_params! {":host": HOST}, |row| open(row))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .filter(|cookie| {
            cookie.expires.is_none_or(|expires| {
                UNIX_EPOCH + Duration::from_secs(u64::try_from(expires).unwrap()) > now
            })
        })
        .collect()
}

/// where SQL matches a cookie's `domain` column to `:host`.
fn covering(domain: &str) -> String {
    format!(
        "({domain} = :host OR (substr({domain}, 1, 1) = '.' AND (:host = substr({domain}, 2) \
         OR substr(:host, -length({domain})) = {domain})))"
    )
}

/// firefox's `moz_cookies` as a profile migrated through its schema versions holds it: three
/// columns added after rows were written, and an index on the domain beside the table's own.
fn firefox_profile(path: &Path, page_size: u32, wal: bool) -> Connection {
    let db = opened(path, page_size, wal);
    db.execute_batch(
        "PRAGMA user_version = 17; \
         CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, \
         originAttributes TEXT NOT NULL DEFAULT '', name TEXT, value TEXT, host TEXT, path TEXT, \
         expiry INTEGER, lastAccessed INTEGER, creationTime INTEGER, isSecure INTEGER, \
         isHttpOnly INTEGER, inBrowserElement INTEGER DEFAULT 0, sameSite INTEGER DEFAULT 0, \
         CONSTRAINT moz_uniqueid UNIQUE (name, host, path, originAttributes)); \
         CREATE INDEX moz_basedomain ON moz_cookies (host, originAttributes);",
    )
    .unwrap();
    let insert = |db: &Connection, row: i64| {
        db.execute(
            "INSERT INTO moz_cookies (id, originAttributes, name, value, host, path, expiry, \
             lastAccessed, creationTime, isSecure, isHttpOnly, sameSite) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                row + 1,
                apart(row),
                format!("c{row}"),
                value(row),
                domain_of(row),
                path_of(row),
                expires(row) * 1_000,
                1_700_000_000_000_000_i64 + row,
                1_600_000_000_000_000_i64 + row,
                i64::from(row % 5 == 0),
                i64::from(row % 2 == 0),
                row % 3,
            ],
        )
        .unwrap();
    };
    // the first thousand rows predate the columns firefox's later schema versions add
    let migrate = |db: &Connection| {
        db.execute_batch(
            "ALTER TABLE moz_cookies ADD schemeMap INTEGER DEFAULT 0; \
             ALTER TABLE moz_cookies ADD isPartitionedAttributeSet INTEGER DEFAULT 0; \
             ALTER TABLE moz_cookies ADD updateTime INTEGER;",
        )
        .unwrap();
    };
    let change = |db: &Connection, row: i64| {
        db.execute(
            "UPDATE moz_cookies SET value = ?1, schemeMap = 2 WHERE id = ?2",
            rusqlite::params![value(row + 7 * (row % 2)), row + 1],
        )
        .unwrap();
    };
    aged(&db, "moz_cookies", &insert, &migrate, &change);
    db
}

/// the values `chrome_profile` sealed for one row with another's value, kept for each shape.
type Sealed = RefCell<HashMap<(i64, i64), Vec<u8>>>;

/// chromium's v24 cookies table with its unique index and meta table, each value sealed under
/// `KEY` with the SHA-256 of its domain before it, once for every shape in `sealed`. the values in
/// the clear, by name.
fn chrome_profile(
    path: &Path,
    page_size: u32,
    wal: bool,
    sealed: &Sealed,
) -> (Connection, HashMap<String, String>) {
    let db = opened(path, page_size, wal);
    db.execute_batch(
        "CREATE TABLE meta(key LONGVARCHAR NOT NULL UNIQUE PRIMARY KEY, value LONGVARCHAR); \
         INSERT INTO meta VALUES ('version', '24'), ('last_compatible_version', '24'); \
         CREATE TABLE cookies(creation_utc INTEGER NOT NULL,host_key TEXT NOT NULL,\
         top_frame_site_key TEXT NOT NULL,name TEXT NOT NULL,value TEXT NOT NULL,\
         encrypted_value BLOB NOT NULL,path TEXT NOT NULL,expires_utc INTEGER NOT NULL,\
         is_secure INTEGER NOT NULL,is_httponly INTEGER NOT NULL,last_access_utc INTEGER NOT NULL,\
         has_expires INTEGER NOT NULL,is_persistent INTEGER NOT NULL,priority INTEGER NOT NULL,\
         samesite INTEGER NOT NULL,source_scheme INTEGER NOT NULL,source_port INTEGER NOT NULL,\
         last_update_utc INTEGER NOT NULL,source_type INTEGER NOT NULL,\
         has_cross_site_ancestor INTEGER NOT NULL); \
         CREATE UNIQUE INDEX cookies_unique_index ON cookies(host_key, top_frame_site_key, \
         has_cross_site_ancestor, name, path, source_scheme, source_port);",
    )
    .unwrap();
    let seal = |row: i64, seed: i64| {
        let mut sealed = sealed.borrow_mut();
        let sealed = sealed.entry((row, seed)).or_insert_with(|| {
            let domain = domain_of(row);
            let plain = [
                sha2::Sha256::digest(domain.as_bytes()).as_slice(),
                value(seed).as_bytes(),
            ]
            .concat();
            let sealed = cbc::Encryptor::<aes::Aes128>::new(&KEY.into(), &[b' '; 16].into())
                .encrypt_padded_vec::<Pkcs7>(&plain);
            [b"v10".as_slice(), &sealed].concat()
        });
        sealed.clone()
    };
    let insert =
        |db: &Connection, row: i64| {
            let expires = expires(row);
            let frame = if row % 13 == 0 {
                "https://example.com"
            } else {
                ""
            };
            db.execute(
            "INSERT INTO cookies VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, \
             1, ?13, 2, 443, ?14, 0, ?15)",
            rusqlite::params![
                (1_600_000_000 + EPOCHS_APART) * 1_000_000 + row,
                domain_of(row),
                frame,
                format!("c{row}"),
                seal(row, row),
                path_of(row),
                if expires == 0 { 0 } else { (expires + EPOCHS_APART) * 1_000_000 },
                i64::from(row % 5 == 0),
                i64::from(row % 2 == 0),
                (1_700_000_000 + EPOCHS_APART) * 1_000_000 + row,
                i64::from(expires != 0),
                i64::from(expires != 0),
                row % 3,
                (1_700_000_000 + EPOCHS_APART) * 1_000_000 + row,
                i64::from(row % 11 == 0),
            ],
        )
        .unwrap();
        };
    let change = |db: &Connection, row: i64| {
        db.execute(
            "UPDATE cookies SET encrypted_value = ?1 WHERE name = ?2",
            rusqlite::params![seal(row, row + 7 * (row % 2)), format!("c{row}")],
        )
        .unwrap();
    };
    aged(&db, "cookies", &insert, &|_| {}, &change);
    let mut statement = db.prepare("SELECT name FROM cookies").unwrap();
    let names: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(statement);
    // the value each kept row holds now: changed twice over by `aged`, or as inserted
    let plain = names
        .into_iter()
        .map(|name| {
            let row: i64 = name[1..].parse().unwrap();
            let changed = [9, 10]
                .iter()
                .rev()
                .find(|step| row < ROWS && row % *step == 1)
                .map_or(row, |_| row + 7 * (row % 2));
            (name, value(changed))
        })
        .collect();
    (db, plain)
}

/// where each shape's database goes.
fn located(folder: &Path, browser: &str, (page_size, wal): (u32, bool)) -> PathBuf {
    folder.join(format!(
        "{browser}-{page_size}-{}.sqlite",
        if wal { "wal" } else { "rollback" }
    ))
}

#[test]
fn a_firefox_profile_reads_as_sqlite_reads_it() {
    let folder = tempfile::tempdir().unwrap();
    for shape in SHAPES {
        let path = located(folder.path(), "firefox", shape);
        let db = firefox_profile(&path, shape.0, shape.1);
        shaped(
            &db,
            &path,
            ("moz_cookies", "moz_basedomain", "value"),
            shape.0,
        );
        let (names, theirs) = sqlite_rows(&db, "moz_cookies");
        let columns: Vec<&str> = names.iter().map(String::as_str).collect();
        let ours = Database::open(&path)
            .unwrap()
            .rows("moz_cookies", &columns)
            .unwrap();
        assert_eq!(ours.len(), theirs.len(), "{shape:?}");
        let first = ours.iter().zip(&theirs).position(|(a, b)| a != b);
        if let Some(row) = first {
            let differs: Vec<String> = names
                .iter()
                .zip(ours[row].iter().zip(&theirs[row]))
                .filter(|(_, (a, b))| a != b)
                .map(|(name, (a, b))| format!("{name}: ours {a:?}, sqlite {b:?}"))
                .collect();
            let rows = ours.iter().zip(&theirs).filter(|(a, b)| a != b).count();
            panic!("{shape:?}: {rows} rows differ, the first {row}: {differs:?}");
        }
        let found = firefox::read(&path).unwrap().found;
        let ours = pick(found, HOST, SystemTime::now()).kept;
        let theirs = sqlite_cookies(
            &db,
            &format!(
                "SELECT name, value, host, path, isSecure, expiry FROM moz_cookies \
                 WHERE originAttributes = '' AND {} ORDER BY rowid",
                covering("host")
            ),
            &|row| {
                let expiry: i64 = row.get(5)?;
                Ok(Cookie {
                    name: row.get(0)?,
                    value: row.get(1)?,
                    domain: row.get(2)?,
                    path: row.get(3)?,
                    secure: row.get::<_, i64>(4)? != 0,
                    expires: (expiry > 0).then_some(expiry / 1_000),
                })
            },
        );
        assert!(theirs.len() > 1_000, "{shape:?}: {}", theirs.len());
        assert_eq!(ours, theirs, "{shape:?}");
    }
}

#[test]
fn a_chrome_profile_reads_as_sqlite_reads_it() {
    let folder = tempfile::tempdir().unwrap();
    let sealed = Sealed::default();
    for shape in SHAPES {
        let path = located(folder.path(), "chrome", shape);
        let (db, plain) = chrome_profile(&path, shape.0, shape.1, &sealed);
        shaped(
            &db,
            &path,
            ("cookies", "cookies_unique_index", "encrypted_value"),
            shape.0,
        );
        let (names, theirs) = sqlite_rows(&db, "cookies");
        let columns: Vec<&str> = names.iter().map(String::as_str).collect();
        let ours = Database::open(&path)
            .unwrap()
            .rows("cookies", &columns)
            .unwrap();
        assert_eq!(ours.len(), theirs.len(), "{shape:?}");
        assert!(ours == theirs, "{shape:?}: the rows differ");
        let key = |tag: &str| (tag == "v10").then_some(KEY);
        let found = chromium::read(&path, HOST, &key).unwrap().found;
        let ours = pick(found, HOST, SystemTime::now()).kept;
        let theirs = sqlite_cookies(
            &db,
            &format!(
                "SELECT name, host_key, path, is_secure, has_expires, expires_utc FROM cookies \
                 WHERE top_frame_site_key = '' AND {} ORDER BY rowid",
                covering("host_key")
            ),
            &|row| {
                let name: String = row.get(0)?;
                let micros: i64 = row.get(5)?;
                Ok(Cookie {
                    value: plain[&name].clone(),
                    name,
                    domain: row.get(1)?,
                    path: row.get(2)?,
                    secure: row.get::<_, i64>(3)? == 1,
                    expires: (row.get::<_, i64>(4)? != 0 && micros > 0)
                        .then(|| micros / 1_000_000 - EPOCHS_APART),
                })
            },
        );
        assert!(theirs.len() > 1_000, "{shape:?}: {}", theirs.len());
        assert_eq!(ours, theirs, "{shape:?}");
    }
}

use rusqlite::Connection;

use super::*;

/// a database SQLite writes at `path` with pages of `page_size` bytes, in `journal` mode.
fn written(path: &Path, page_size: u32, journal: &str) -> Connection {
    let db = Connection::open(path).unwrap();
    db.execute_batch(&format!(
        "PRAGMA page_size = {page_size}; PRAGMA journal_mode = {journal}; PRAGMA wal_autocheckpoint = 0; PRAGMA user_version = 17;"
    ))
    .unwrap();
    db.execute_batch(
        "CREATE TABLE \"cookies\" (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT '', \
         value TEXT, raw BLOB, number INTEGER, ratio REAL, CONSTRAINT one UNIQUE (name, value))",
    )
    .unwrap();
    db
}

/// a row of the test table: a name, a value of `length` bytes, a blob, and numbers of each
/// width SQLite stores.
fn row(at: i64) -> (String, String, Vec<u8>, i64, f64) {
    let length = match at % 5 {
        0 => 0,
        1 => 7,
        2 => 300,
        3 => 2_000,
        _ => 9_000,
    };
    let number = match at % 7 {
        0 => 0,
        1 => 1,
        2 => -128,
        3 => 40_000,
        4 => -8_000_000,
        5 => 3_000_000_000,
        _ => i64::MIN + at,
    };
    let value = (0..length)
        .map(|i| char::from(b'a' + u8::try_from((i + at) % 26).unwrap()))
        .collect();
    (
        format!("cookie-{at}"),
        value,
        at.to_be_bytes().to_vec(),
        number,
        // a whole number in a REAL column is stored as an integer, which SQLite's affinity reads
        // back as REAL; cookie tables keep no reals, so the reader takes values as stored
        (f64::from(u32::try_from(at).unwrap()) + 0.5) / 3.0,
    )
}

/// inserts `rows` in one commit.
fn insert(db: &Connection, rows: std::ops::Range<i64>) {
    let commit = db.unchecked_transaction().unwrap();
    {
        let mut statement = commit
            .prepare(
                "INSERT INTO cookies (name, value, raw, number, ratio) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .unwrap();
        for at in rows {
            let (name, value, raw, number, ratio) = row(at);
            statement
                .execute(rusqlite::params![name, value, raw, number, ratio])
                .unwrap();
        }
    }
    commit.commit().unwrap();
}

/// the rows as this reader reads them, and as SQLite does.
fn both(path: &Path, db: &Connection) -> (Vec<Vec<Value>>, Vec<Vec<Value>>) {
    let ours = Database::open(path)
        .unwrap()
        .rows(
            "Cookies",
            &["id", "name", "value", "raw", "number", "ratio", "absent"],
        )
        .unwrap();
    let mut statement = db
        .prepare("SELECT id, name, value, raw, number, ratio FROM cookies ORDER BY id")
        .unwrap();
    let theirs = statement
        .query_map([], |found| {
            Ok(vec![
                Value::Integer(found.get(0)?),
                Value::Text(found.get(1)?),
                Value::Text(found.get(2)?),
                Value::Blob(found.get(3)?),
                Value::Integer(found.get(4)?),
                Value::Real(found.get(5)?),
                Value::Null,
            ])
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (ours, theirs)
}

/// small pages put long values on overflow pages and many rows on interior ones.
#[test]
fn a_table_reads_as_sqlite_wrote_it() {
    let folder = tempfile::tempdir().unwrap();
    for (page_size, journal) in [(512, "DELETE"), (4096, "DELETE"), (1024, "WAL")] {
        let path = folder.path().join(format!("{page_size}-{journal}.sqlite"));
        let db = written(&path, page_size, journal);
        insert(&db, 0..1_500);
        let (ours, theirs) = both(&path, &db);
        assert_eq!(ours.len(), 1_500);
        let first = ours.iter().zip(&theirs).position(|(a, b)| a != b);
        assert_eq!(first, None, "{page_size} {journal}");
        assert_eq!(ours, theirs, "{page_size} {journal}");
        assert_eq!(Database::open(&path).unwrap().user_version, 17);
    }
}

/// a browser that holds its database open keeps new rows in the log until it checkpoints.
#[test]
fn rows_still_in_the_log_count_up_to_its_last_whole_commit() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("cookies.sqlite");
    let db = written(&path, 1024, "WAL");
    insert(&db, 0..10);
    db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    insert(&db, 10..20);
    insert(&db, 20..30);
    let log = std::fs::read(folder.path().join("cookies.sqlite-wal")).unwrap();
    assert!(log.len() > 32, "the rows must still be in the log");
    let (ours, theirs) = both(&path, &db);
    assert_eq!(ours.len(), 30);
    assert_eq!(ours, theirs);
    let file = std::fs::read(&path).unwrap();
    let count = |log: &[u8]| {
        Database::parse(file.clone(), Some(log))
            .unwrap()
            .rows("cookies", &["name"])
            .unwrap()
            .len()
    };
    assert_eq!(count(&[]), 10);
    // a frame cut short, or one whose bytes changed, ends the log at the commit before it
    assert_eq!(count(&log[..log.len() - 1]), 20);
    let mut changed = log.clone();
    let last = changed.len() - 1;
    changed[last] ^= 1;
    assert_eq!(count(&changed), 20);
    // a log of another database, whose salts differ, adds nothing
    let mut other = log.clone();
    other[16] ^= 1;
    assert_eq!(count(&other), 10);
}

#[test]
fn what_is_no_database_is_refused_and_a_missing_table_named() {
    for bytes in [
        b"".to_vec(),
        [b"not a database at all".as_slice(), &[b'.'; 100]].concat(),
    ] {
        assert_eq!(
            Database::parse(bytes, None).err(),
            Some(Broken("it is not an SQLite database".to_owned()))
        );
    }
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("cookies.sqlite");
    let _db = written(&path, 4096, "DELETE");
    assert_eq!(
        Database::open(&path)
            .unwrap()
            .rows("moz_cookies", &["name"])
            .err(),
        Some(Broken("it has no table moz_cookies".to_owned()))
    );
}

#[test]
fn columns_are_read_from_the_statement_that_made_the_table() {
    assert_eq!(
        columns(
            "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL DEFAULT '', \
             name TEXT, \"value\" TEXT, [host] TEXT, expiry INTEGER DEFAULT (1 + 2), \
             CONSTRAINT moz_uniqueid UNIQUE (name, host, path, originAttributes))"
        ),
        [
            ("id".to_owned(), true),
            ("originAttributes".to_owned(), false),
            ("name".to_owned(), false),
            ("value".to_owned(), false),
            ("host".to_owned(), false),
            ("expiry".to_owned(), false),
        ]
    );
}

/// a database with any of its bytes changed, and any log beside it, never panics the reader:
/// it reads, or refuses.
#[test]
fn fuzz_cookie_databases() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("cookies.sqlite");
    let db = written(&path, 512, "DELETE");
    insert(&db, 0..40);
    drop(db);
    let file = std::fs::read(&path).unwrap();
    bolero::check!()
        .with_type::<(Vec<(u16, u8)>, Vec<u8>)>()
        .for_each(|(changes, log)| {
            let mut bytes = file.clone();
            for (at, byte) in changes {
                let at = usize::from(*at) % bytes.len();
                bytes[at] = *byte;
            }
            if let Ok(database) = Database::parse(bytes, Some(log)) {
                let _ = database.rows("cookies", &["name", "value", "number"]);
            }
        });
}

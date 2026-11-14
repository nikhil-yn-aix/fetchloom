use rusqlite::Connection;

use super::*;

#[cfg(test)]
mod built;

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
        // SQLite keeps a whole number in a REAL column as an integer, and reads it as a real
        if at % 4 == 0 {
            f64::from(u32::try_from(at).unwrap())
        } else {
            (f64::from(u32::try_from(at).unwrap()) + 0.5) / 3.0
        },
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

/// sums `log`'s header and frames of `page_size` bytes again, as SQLite's writer does.
fn summed(log: &mut [u8], page_size: usize) {
    let big = be32(log, 0) == 0x377f_0683;
    let mut sums = checksum((0, 0), &log[..24], big);
    log[24..28].copy_from_slice(&sums.0.to_be_bytes());
    log[28..32].copy_from_slice(&sums.1.to_be_bytes());
    let frame = 24 + page_size;
    for at in (32..=log.len() - frame).step_by(frame) {
        sums = checksum(
            checksum(sums, &log[at..at + 8], big),
            &log[at + 24..at + frame],
            big,
        );
        log[at + 16..at + 20].copy_from_slice(&sums.0.to_be_bytes());
        log[at + 20..at + 24].copy_from_slice(&sums.1.to_be_bytes());
    }
}

/// a log whose page size is not the database's adds nothing: one SQLite wrote for a database
/// of other pages, or this database's own frames under a header that names another size. SQLite
/// before 3.54.0 read such a pair into memory it had not filled (CVE-2026-119375).
#[test]
fn a_log_of_another_page_size_adds_nothing() {
    let folder = tempfile::tempdir().unwrap();
    let logged = |name: &str, page_size: u32| {
        let path = folder.path().join(name);
        let db = written(&path, page_size, "WAL");
        insert(&db, 0..10);
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        insert(&db, 10..30);
        let mut log = path.as_os_str().to_owned();
        log.push("-wal");
        (std::fs::read(&path).unwrap(), std::fs::read(log).unwrap())
    };
    let (file, log) = logged("small.sqlite", 1024);
    let (_, larger) = logged("large.sqlite", 4096);
    let count = |log: &[u8]| {
        Database::parse(file.clone(), Some(log))
            .unwrap()
            .rows("cookies", &["name"])
            .unwrap()
            .len()
    };
    assert_eq!(count(&log), 30);
    assert_eq!(count(&larger), 10);
    let mut renamed = log.clone();
    renamed[8..12].copy_from_slice(&2048_u32.to_be_bytes());
    summed(&mut renamed, 1024);
    assert_eq!(count(&renamed), 10);
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
    let read: Vec<_> = columns(
        "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL DEFAULT '', \
         name TEXT, \"value\" TEXT, [host] TEXT, expiry INTEGER DEFAULT (1 + 2), \
         CONSTRAINT moz_uniqueid UNIQUE (name, host, path, originAttributes))",
    )
    .unwrap()
    .into_iter()
    .map(|column| (column.name, column.rowid, column.default))
    .collect();
    let text = |text: &str| Some(Value::Text(text.to_owned()));
    assert_eq!(
        read,
        [
            ("id".to_owned(), true, Some(Value::Null)),
            ("originAttributes".to_owned(), false, text("")),
            ("name".to_owned(), false, Some(Value::Null)),
            ("value".to_owned(), false, Some(Value::Null)),
            ("host".to_owned(), false, Some(Value::Null)),
            // an expression SQLite works out, which ALTER TABLE never adds a column with
            ("expiry".to_owned(), false, None),
        ]
    );
}

/// a row written before `ALTER TABLE ADD COLUMN` lacks the columns added since, which SQLite
/// reads as their defaults, by the affinity each column's type gives it, and a REAL column's whole
/// numbers, which SQLite keeps as integers, read as reals.
#[test]
fn a_row_older_than_a_column_reads_its_default() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("cookies.sqlite");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, kept TEXT); INSERT INTO t (kept) VALUES ('old');",
    )
    .unwrap();
    let added = [
        ("a", "INTEGER DEFAULT 0"),
        ("b", "TEXT DEFAULT ''"),
        ("c", "TEXT DEFAULT 7"),
        ("d", "INTEGER DEFAULT '7'"),
        ("e", "INTEGER DEFAULT 'x'"),
        ("f", "REAL DEFAULT 2"),
        ("g", "REAL DEFAULT 1.5"),
        ("h", "INTEGER DEFAULT 2.0"),
        ("i", "NUMERIC DEFAULT '3.5'"),
        ("j", "DEFAULT 'abc'"),
        ("k", "BLOB DEFAULT X'00fF'"),
        ("l", "INTEGER DEFAULT -3"),
        ("m", "INTEGER DEFAULT 0x10"),
        ("n", "TEXT DEFAULT NULL"),
        ("o", "INTEGER DEFAULT TRUE"),
        ("p", "INTEGER"),
        ("q", "VARCHAR(20) DEFAULT 'it''s'"),
        ("r", "DOUBLE PRECISION DEFAULT +4"),
        ("s", "INTEGER NOT NULL DEFAULT 5"),
        ("t u", "TEXT COLLATE NOCASE DEFAULT 'v'"),
    ];
    for (name, definition) in added {
        db.execute_batch(&format!("ALTER TABLE t ADD \"{name}\" {definition}"))
            .unwrap();
    }
    db.execute_batch("INSERT INTO t (kept, f, g) VALUES ('new', 9, 2.5)")
        .unwrap();
    let names: Vec<&str> = ["id", "kept"]
        .into_iter()
        .chain(added.iter().map(|(name, _)| *name))
        .collect();
    let ours = Database::open(&path).unwrap().rows("t", &names).unwrap();
    let mut statement = db.prepare("SELECT * FROM t ORDER BY rowid").unwrap();
    let theirs: Vec<Vec<Value>> = statement
        .query_map([], |row| {
            (0..names.len())
                .map(|at| {
                    Ok(match row.get_ref(at)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(integer) => Value::Integer(integer),
                        rusqlite::types::ValueRef::Real(real) => Value::Real(real),
                        rusqlite::types::ValueRef::Text(text) => {
                            Value::Text(String::from_utf8(text.to_vec()).unwrap())
                        }
                        rusqlite::types::ValueRef::Blob(blob) => Value::Blob(blob.to_vec()),
                    })
                })
                .collect()
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (row, (ours, theirs)) in ours.iter().zip(&theirs).enumerate() {
        for (name, (a, b)) in names.iter().zip(ours.iter().zip(theirs)) {
            assert_eq!(a, b, "row {row}, column {name}");
        }
    }
    assert_eq!(ours.len(), 2);
}

/// a statement whose last `)` comes before its `(`, and a row whose header claims more than
/// memory, are refused rather than panic.
#[test]
fn a_broken_statement_or_row_is_refused_without_a_panic() {
    for sql in [")(", "CREATE TABLE t ) (", ")x(a TEXT", "CREATE TABLE t(a"] {
        let read: Vec<String> = columns(sql)
            .unwrap()
            .into_iter()
            .map(|column| column.name)
            .collect();
        let expected: &[&str] = if sql.ends_with("(a") || sql.ends_with("TEXT") {
            &["a"]
        } else {
            &[]
        };
        assert_eq!(read, expected, "{sql}");
    }
    // a header of the largest length a varint gives, then a value of one byte
    let record = [[0xff; 9].as_slice(), &[1]].concat();
    assert!(decode(&record).is_err());
}

/// a default in parentheses of any depth, as a crafted statement nests one a hundred thousand
/// deep, reads in a loop rather than a call for each, so it never runs out of stack; one whose
/// parentheses never close is no constant.
#[test]
fn a_default_in_parentheses_of_any_depth_reads_without_running_out_of_stack() {
    let deep = 100_000;
    let nested = format!(
        "CREATE TABLE t(a INTEGER DEFAULT {}7 {}, b)",
        "( ".repeat(deep),
        ") ".repeat(deep)
    );
    let read = columns(&nested).unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].default, Some(Value::Integer(7)));
    let unclosed = format!("CREATE TABLE t(a INTEGER DEFAULT {}7)", "(".repeat(deep));
    assert_eq!(columns(&unclosed).unwrap()[0].default, None);
}

/// a table's statement of more than 1 MiB, or of more columns than SQLite lets a table have,
/// 32,767 at its highest setting, is refused before it is taken apart: one of 128 MiB would
/// make some 64 million columns of 6 GB.
#[test]
fn a_statement_of_more_bytes_or_columns_than_a_table_takes_is_refused() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("cookies.sqlite");
    let db = written(&path, 4096, "DELETE");
    insert(&db, 0..3);
    let read = |sql: &str| {
        db.execute_batch("PRAGMA writable_schema = ON").unwrap();
        db.execute(
            "UPDATE sqlite_master SET sql = ?1 WHERE name = 'cookies'",
            [sql],
        )
        .unwrap();
        Database::open(&path)
            .unwrap()
            .rows("cookies", &["name"])
            .map(|rows| rows.len())
    };
    let refused = |read: Result<usize, Broken>, why: &str| {
        assert!(
            read.as_ref().is_err_and(|Broken(said)| said.contains(why)),
            "{why}: {read:?}"
        );
    };
    let made = "CREATE TABLE cookies (id INTEGER PRIMARY KEY, name TEXT, value TEXT, raw BLOB, \
                number INTEGER, ratio REAL";
    let padded = |length: usize| format!("{made}{})", " ".repeat(length - made.len() - 1));
    assert_eq!(read(&padded(1 << 20)), Ok(3));
    refused(read(&padded((1 << 20) + 1)), "longer");
    let more = |count: usize| {
        let added: Vec<String> = (0..count).map(|at| format!("c{at}")).collect();
        format!("{made}, {})", added.join(", "))
    };
    assert_eq!(read(&more(32_767 - 6)), Ok(3));
    refused(read(&more(32_768 - 6)), "columns");
}

/// SQLite's variable-length integer for `value`, as `varint` reads it.
fn put_varint(value: u64) -> Vec<u8> {
    if value > 0x00ff_ffff_ffff_ffff {
        let mut bytes: Vec<u8> = (0..8)
            .rev()
            .map(|at| 0x80 | u8::try_from((value >> (8 + 7 * at)) & 0x7f).unwrap())
            .collect();
        bytes.push(u8::try_from(value & 0xff).unwrap());
        return bytes;
    }
    let mut groups = vec![u8::try_from(value & 0x7f).unwrap()];
    let mut rest = value >> 7;
    while rest > 0 {
        groups.push(0x80 | u8::try_from(rest & 0x7f).unwrap());
        rest >>= 7;
    }
    groups.reverse();
    groups
}

/// a record of text values, as a table's row holds them.
fn texts(values: &[&str]) -> Vec<u8> {
    let kinds: Vec<u8> = values
        .iter()
        .flat_map(|value| put_varint(13 + 2 * value.len() as u64))
        .collect();
    let mut record = put_varint(1 + kinds.len() as u64);
    record.extend(kinds);
    for value in values {
        record.extend_from_slice(value.as_bytes());
    }
    record
}

/// a table leaf page of `page_size` bytes holding `cells`, each a rowid and its record, with
/// the cell pointers `order` gives, by index, or each once in turn.
fn leaf(
    page_size: usize,
    head: usize,
    cells: &[(i64, Vec<u8>)],
    order: Option<&[usize]>,
) -> Vec<u8> {
    let mut page = vec![0; page_size];
    let mut end = page_size;
    let mut offsets = Vec::new();
    for (rowid, record) in cells {
        let mut cell = put_varint(record.len() as u64);
        cell.extend(put_varint(rowid.cast_unsigned()));
        cell.extend_from_slice(record);
        end -= cell.len();
        page[end..end + cell.len()].copy_from_slice(&cell);
        offsets.push(u16::try_from(end).unwrap());
    }
    let order: Vec<usize> = order.map_or_else(|| (0..cells.len()).collect(), <[usize]>::to_vec);
    page[head] = 0x0d;
    page[head + 3..head + 5].copy_from_slice(&u16::try_from(order.len()).unwrap().to_be_bytes());
    page[head + 5..head + 7].copy_from_slice(&u16::try_from(end).unwrap().to_be_bytes());
    for (at, index) in order.iter().enumerate() {
        let pointer = head + 8 + 2 * at;
        page[pointer..pointer + 2].copy_from_slice(&offsets[*index].to_be_bytes());
    }
    page
}

/// a table interior page whose cells point at `children`, then at `right`.
fn interior(page_size: usize, children: &[u32], right: u32) -> Vec<u8> {
    let mut page = vec![0; page_size];
    let mut end = page_size;
    page[0] = 0x05;
    page[3..5].copy_from_slice(&u16::try_from(children.len()).unwrap().to_be_bytes());
    page[8..12].copy_from_slice(&right.to_be_bytes());
    for (at, child) in children.iter().enumerate() {
        let mut cell = child.to_be_bytes().to_vec();
        cell.extend(put_varint(at as u64 + 1));
        end -= cell.len();
        page[end..end + cell.len()].copy_from_slice(&cell);
        page[12 + 2 * at..14 + 2 * at].copy_from_slice(&u16::try_from(end).unwrap().to_be_bytes());
    }
    page[5..7].copy_from_slice(&u16::try_from(end).unwrap().to_be_bytes());
    page
}

/// a database laid out page by page: page 1 holds the schema, whose one table `t` has one
/// text column and its tree at page 2, then `pages` from page 2 on.
fn laid_out(page_size: usize, pages: &[Vec<u8>]) -> Vec<u8> {
    let schema = [(1, {
        let mut record = texts(&["table", "t", "t"]);
        // the root page, an integer of one byte, then the statement
        let sql = "CREATE TABLE t(a)";
        let header = record[0];
        record.insert(usize::from(header), 1);
        record.insert(
            usize::from(header) + 1,
            u8::try_from(13 + 2 * sql.len()).unwrap(),
        );
        record[0] = header + 2;
        record.push(2);
        record.extend_from_slice(sql.as_bytes());
        record
    })];
    let mut first = leaf(page_size, 100, &schema, None);
    first[..16].copy_from_slice(b"SQLite format 3\0");
    let size = if page_size == 65_536 {
        1
    } else {
        u16::try_from(page_size).unwrap()
    };
    first[16..18].copy_from_slice(&size.to_be_bytes());
    first[18] = 1;
    first[19] = 1;
    first[21..24].copy_from_slice(&[64, 32, 32]);
    first[28..32].copy_from_slice(&u32::try_from(pages.len() + 1).unwrap().to_be_bytes());
    first[44..48].copy_from_slice(&4_u32.to_be_bytes());
    first[56..60].copy_from_slice(&1_u32.to_be_bytes());
    [vec![first], pages.to_vec()].concat().concat()
}

/// a table whose tree reaches a page more than once, by two cells, a cell and the last
/// pointer, or a cell pointer listed twice, is refused rather than walked once a path: a tree of
/// a dozen such pages would otherwise give 3 to the 12th power rows.
#[test]
fn a_page_reached_twice_is_refused() {
    let size = 512;
    let row = |rowid| (rowid, texts(&["a cookie"]));
    let one = leaf(size, 0, &[row(1)], None);
    let shared = laid_out(size, &[interior(size, &[3, 3], 3), one.clone()]);
    let mut levels: Vec<Vec<u8>> = (0..12)
        .map(|level| interior(size, &[3 + level, 3 + level], 3 + level))
        .collect();
    levels.push(one);
    let deep = laid_out(size, &levels);
    let listed_twice = laid_out(size, &[leaf(size, 0, &[row(1), row(2)], Some(&[0, 1, 0]))]);
    for (case, bytes) in [
        ("shared", shared),
        ("deep", deep),
        ("listed twice", listed_twice),
    ] {
        let read = Database::parse(bytes, None).unwrap().rows("t", &["a"]);
        assert!(
            read.as_ref()
                .is_err_and(|Broken(why)| why.contains("twice")),
            "{case}: {:?}",
            read.map(|rows| rows.len())
        );
    }
    // a row longer than its page, whose overflow chain runs back into the page that holds it
    let record = texts(&["x".repeat(2_000).as_str()]);
    let local = Database::parse(laid_out(size, &[leaf(size, 0, &[row(1)], None)]), None)
        .unwrap()
        .local(record.len());
    let mut cell = put_varint(record.len() as u64);
    cell.extend(put_varint(1));
    cell.extend_from_slice(&record[..local]);
    cell.extend_from_slice(&2_u32.to_be_bytes());
    let mut spilled = vec![0; size];
    let start = size - cell.len();
    spilled[0] = 0x0d;
    spilled[3..5].copy_from_slice(&1_u16.to_be_bytes());
    spilled[5..7].copy_from_slice(&u16::try_from(start).unwrap().to_be_bytes());
    spilled[8..10].copy_from_slice(&u16::try_from(start).unwrap().to_be_bytes());
    spilled[start..].copy_from_slice(&cell);
    let read = Database::parse(laid_out(size, &[spilled]), None)
        .unwrap()
        .rows("t", &["a"]);
    assert!(
        read.as_ref()
            .is_err_and(|Broken(why)| why.contains("twice")),
        "{read:?}"
    );
}

/// a log's commit may claim more pages than the file and the log hold, as a crafted one does
/// to make a loop of overflow pages run for ever; the reader counts only those it holds. and a
/// table of more bytes than the reader's budget is refused.
#[test]
fn pages_and_bytes_are_kept_to_what_the_files_hold() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("cookies.sqlite");
    let db = written(&path, 1024, "WAL");
    insert(&db, 0..30);
    let file = std::fs::read(&path).unwrap();
    let mut log = std::fs::read(folder.path().join("cookies.sqlite-wal")).unwrap();
    // the last frame commits; claim the most pages there are, and sum the frames again
    let frame = 24 + 1024;
    let last = 32 + (log.len() - 32) / frame * frame - frame;
    log[last + 4..last + 8].copy_from_slice(&u32::MAX.to_be_bytes());
    summed(&mut log, 1024);
    let held = u32::try_from(file.len() / 1024 + (log.len() - 32) / frame).unwrap();
    let database = Database::parse(file.clone(), Some(&log)).unwrap();
    assert!(database.pages <= held, "{} pages", database.pages);
    let mut database = Database::parse(file, Some(&log)).unwrap();
    database.budget = 1_000;
    let read = database.rows("cookies", &["name", "value"]);
    assert!(read.is_err_and(|Broken(why)| why.contains("larger than")));
}

/// cells that share bytes, as a run of 01 bytes is a cell at every offset, are refused: one leaf
/// of 64 KiB would otherwise give 21,800 rows.
#[test]
fn cells_that_overlap_are_refused() {
    let size = 65_536;
    let count = 21_800;
    let start = 8 + 2 * count;
    let mut page = vec![0; size];
    page[0] = 0x0d;
    page[3..5].copy_from_slice(&u16::try_from(count).unwrap().to_be_bytes());
    page[5..7].copy_from_slice(&u16::try_from(start).unwrap().to_be_bytes());
    for at in 0..count {
        let offset = u16::try_from(start + at).unwrap().to_be_bytes();
        page[8 + 2 * at..10 + 2 * at].copy_from_slice(&offset);
    }
    page[start..].fill(1);
    let read = Database::parse(laid_out(size, &[page]), None)
        .unwrap()
        .rows("t", &["a"]);
    assert!(
        read.as_ref()
            .is_err_and(|Broken(why)| why.contains("overlap")),
        "{:?}",
        read.map(|rows| rows.len())
    );
}

/// the budget counts what a read builds, not only the bytes it reads: a value for each byte of
/// a row's header, so a header of nothing but nulls is refused before it is decoded, and each
/// row handed back with each column asked for.
#[test]
fn the_budget_counts_the_values_a_read_builds() {
    let size = 65_536;
    // one row whose header holds 60,000 nulls, in 60 KB of the file
    let mut record = put_varint(60_003);
    record.extend(vec![0; 60_000]);
    let nulls = laid_out(size, &[leaf(size, 0, &[(1, record)], None)]);
    // 5,000 rows of no values, each read for nine columns
    let empty: Vec<(i64, Vec<u8>)> = (1..=5_000).map(|rowid| (rowid, texts(&[]))).collect();
    let many = laid_out(size, &[leaf(size, 0, &empty, None)]);
    let wanted = ["a"; 9];
    for (case, bytes, stored) in [("nulls", nulls, 60_003), ("rows", many, 5_000)] {
        assert!(bytes.len() < 1 << 20, "{case}: {stored}");
        let mut database = Database::parse(bytes, None).unwrap();
        database.budget = 1 << 20;
        let read = database.rows("t", &wanted);
        assert!(
            read.as_ref()
                .is_err_and(|Broken(why)| why.contains("larger than")),
            "{case}: {:?}",
            read.map(|rows| rows.len())
        );
    }
}

/// a database a fuzzer plans, built as SQLite writes one and then bent as a crafted file would
/// be, never panics the reader or the browsers' readers over it, nor makes them give more rows
/// than its pages hold; unbent, it reads as it was built.
#[test]
fn fuzz_cookie_databases() {
    bolero::check!().for_each(built::exercise);
}

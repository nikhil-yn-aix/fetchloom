//! a reader for the SQLite files browsers keep cookies in: the rows of one table, read from the
//! file and the write-ahead log beside it, as SQLite's file format describes them
//! (<https://www.sqlite.org/fileformat.html>). it takes no lock and runs no SQL, so a browser
//! that holds its files open is never disturbed, and it refuses what it cannot read rather than
//! guess.

use std::collections::HashMap;
use std::path::Path;

/// the most of a browser's cookie file read, with its log: a profile of years takes a few MiB.
const LARGEST: u64 = 256 << 20;
/// the deepest a table's tree may go: a cookie table of millions of rows is five pages deep.
const DEEPEST: usize = 32;

/// a value a column holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    /// the value as text, or none for a value of another kind.
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    /// the value as an integer, or none for a value of another kind.
    pub(crate) fn integer(&self) -> Option<i64> {
        match self {
            Value::Integer(integer) => Some(*integer),
            _ => None,
        }
    }
}

/// how SQLite reads a column's values, by the affinity its declared type gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Affinity {
    Blob,
    Text,
    Numeric,
    Integer,
    Real,
}

/// a column as the statement that made its table, and any `ALTER TABLE` since, declares it.
#[derive(Clone, Debug, PartialEq)]
struct Column {
    name: String,
    /// whether it is the table's `INTEGER PRIMARY KEY`, which SQLite keeps as the rowid.
    rowid: bool,
    affinity: Affinity,
    /// what a row written before the column was added holds in it: its default, or none for a
    /// default this reader cannot work out, as a time is.
    default: Option<Value>,
}

/// why a file could not be read as a database.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Broken(pub(crate) String);

/// what a walk of a table's tree does with each row: its rowid and its record.
type Each<'a> = dyn FnMut(i64, &[u8]) -> Result<(), Broken> + 'a;

fn broken(why: impl Into<String>) -> Broken {
    Broken(why.into())
}

/// an SQLite database as its file and the committed pages of its log hold it.
pub(crate) struct Database {
    file: Vec<u8>,
    page_size: usize,
    /// what each page leaves for its content, past the bytes an extension reserved.
    usable: usize,
    /// the newest committed copy of each page the log holds.
    logged: HashMap<u32, Vec<u8>>,
    /// the pages in the database, after the log's last commit.
    pages: u32,
    /// what `PRAGMA user_version` says: firefox numbers its cookie schema with it.
    pub(crate) user_version: u32,
}

impl Database {
    /// the database at `path`, with the committed pages of the log beside it, `<path>-wal`.
    ///
    /// # Errors
    ///
    /// when the file cannot be read, or is no SQLite database this reader takes.
    pub(crate) fn open(path: &Path) -> Result<Database, std::io::Error> {
        let file = read(path)?.ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        let mut log = path.as_os_str().to_owned();
        log.push("-wal");
        let log = read(Path::new(&log))?;
        Database::parse(file, log.as_deref())
            .map_err(|Broken(why)| std::io::Error::new(std::io::ErrorKind::InvalidData, why))
    }

    /// the database `file` holds, with the committed pages of its write-ahead `log`.
    pub(crate) fn parse(file: Vec<u8>, log: Option<&[u8]>) -> Result<Database, Broken> {
        if file.len() < 100 || !file.starts_with(b"SQLite format 3\0") {
            return Err(broken("it is not an SQLite database"));
        }
        let page_size = match u16::from_be_bytes([file[16], file[17]]) {
            1 => 65_536,
            size if size >= 512 && size.is_power_of_two() => usize::from(size),
            _ => return Err(broken("its page size is not one SQLite writes")),
        };
        let usable = page_size - usize::from(file[20]);
        if usable < 480 || file[21..24] != [64, 32, 32] {
            return Err(broken(
                "its pages are laid out in a way SQLite does not write",
            ));
        }
        if be32(&file, 56) > 1 {
            return Err(broken(
                "it keeps its text in UTF-16, which browsers do not use",
            ));
        }
        let user_version = be32(&file, 60);
        let mut database = Database {
            pages: u32::try_from(file.len() / page_size).unwrap_or(u32::MAX),
            file,
            page_size,
            usable,
            logged: HashMap::new(),
            user_version,
        };
        if let Some(log) = log {
            database.replay(log);
        }
        if let Some(first) = database.logged.get(&1) {
            database.user_version = be32(first, 60);
        }
        Ok(database)
    }

    /// takes the pages of `log` up to its last valid commit, as SQLite's readers do: frames
    /// whose salts match the log's header and whose running checksum holds. a log another page
    /// size wrote, or one past its last commit, adds nothing.
    fn replay(&mut self, log: &[u8]) {
        if log.len() < 32 {
            return;
        }
        let big = match be32(log, 0) {
            0x377f_0682 => false,
            0x377f_0683 => true,
            _ => return,
        };
        if usize::try_from(be32(log, 8)).ok() != Some(self.page_size) {
            return;
        }
        let salts = &log[16..24];
        let mut sums = checksum((0, 0), &log[..24], big);
        if sums != (be32(log, 24), be32(log, 28)) {
            return;
        }
        let mut pending: HashMap<u32, &[u8]> = HashMap::new();
        let mut at = 32;
        while at + 24 + self.page_size <= log.len() {
            let frame = &log[at..at + 24];
            if &frame[8..16] != salts {
                break;
            }
            let page = &log[at + 24..at + 24 + self.page_size];
            sums = checksum(checksum(sums, &frame[..8], big), page, big);
            if sums != (be32(frame, 16), be32(frame, 20)) {
                break;
            }
            pending.insert(be32(frame, 0), page);
            let after = be32(frame, 4);
            if after > 0 {
                self.logged.extend(
                    pending
                        .drain()
                        .map(|(number, page)| (number, page.to_vec())),
                );
                self.pages = after;
            }
            at += 24 + self.page_size;
        }
    }

    /// page `number`, counting from 1, as the last commit left it.
    fn page(&self, number: u32) -> Result<&[u8], Broken> {
        if number == 0 || number > self.pages {
            return Err(broken(format!(
                "it points at page {number}, which it does not hold"
            )));
        }
        if let Some(page) = self.logged.get(&number) {
            return Ok(page);
        }
        let start = (number as usize - 1) * self.page_size;
        self.file
            .get(start..start + self.page_size)
            .ok_or_else(|| broken(format!("it ends before page {number}")))
    }

    /// the rows of `table`, each holding the columns `wanted` names, in that order. a column
    /// the table lacks reads as null.
    ///
    /// # Errors
    ///
    /// when the table is not there or its pages cannot be read.
    pub(crate) fn rows(&self, table: &str, wanted: &[&str]) -> Result<Vec<Vec<Value>>, Broken> {
        let mut found = None;
        self.walk(1, 0, &mut |rowid, record| {
            let _ = rowid;
            let fields = decode(record)?;
            let field = |at: usize| fields.get(at).cloned().unwrap_or(Value::Null);
            let is_table = field(0).text() == Some("table");
            if is_table
                && field(1)
                    .text()
                    .is_some_and(|name| name.eq_ignore_ascii_case(table))
            {
                let root = field(3)
                    .integer()
                    .and_then(|root| u32::try_from(root).ok())
                    .ok_or_else(|| broken(format!("the table {table} has no root page")))?;
                let sql = field(4).text().unwrap_or_default().to_owned();
                found = Some((root, sql));
            }
            Ok(())
        })?;
        let (root, sql) = found.ok_or_else(|| broken(format!("it has no table {table}")))?;
        let columns = columns(&sql);
        if sql.to_ascii_lowercase().contains("without rowid") {
            return Err(broken(format!("the table {table} has no rowid")));
        }
        let at: Vec<Option<&Column>> = wanted
            .iter()
            .map(|name| {
                columns
                    .iter()
                    .find(|column| column.name.eq_ignore_ascii_case(name))
            })
            .collect();
        let positions: Vec<Option<usize>> = at
            .iter()
            .map(|column| column.and_then(|column| columns.iter().position(|each| each == column)))
            .collect();
        let mut rows = Vec::new();
        self.walk(root, 0, &mut |rowid, record| {
            let fields = decode(record)?;
            let mut row = Vec::with_capacity(at.len());
            for (column, position) in at.iter().zip(&positions) {
                row.push(match (column, position) {
                    (Some(column), _) if column.rowid => Value::Integer(rowid),
                    (Some(column), Some(position)) => match fields.get(*position) {
                        Some(value) => read_as(column.affinity, value.clone()),
                        None => column.default.clone().ok_or_else(|| {
                            broken(format!(
                                "a row predates the column {}, whose default is no constant",
                                column.name
                            ))
                        })?,
                    },
                    _ => Value::Null,
                });
            }
            rows.push(row);
            Ok(())
        })?;
        Ok(rows)
    }

    /// calls `each` with the rowid and record of every row in the table tree whose root is page
    /// `number`, `depth` pages below the root.
    fn walk(&self, number: u32, depth: usize, each: &mut Each<'_>) -> Result<(), Broken> {
        if depth > DEEPEST {
            return Err(broken("its table tree goes deeper than any SQLite writes"));
        }
        let page = self.page(number)?;
        let head = if number == 1 { 100 } else { 0 };
        let kind = *page
            .get(head)
            .ok_or_else(|| broken("a page has no header"))?;
        let cells = usize::from(be16(page, head + 3));
        let (interior, pointers) = match kind {
            0x05 => (true, head + 12),
            0x0d => (false, head + 8),
            _ => return Err(broken(format!("page {number} is not part of a table"))),
        };
        for cell in 0..cells {
            let offset = usize::from(be16(page, pointers + 2 * cell));
            if offset >= page.len() {
                return Err(broken(format!("page {number} points past its end")));
            }
            if interior {
                self.walk(be32(page, offset), depth + 1, each)?;
            } else {
                let (rowid, record) = self.cell(page, offset)?;
                each(rowid, &record)?;
            }
        }
        if interior {
            self.walk(be32(page, head + 8), depth + 1, each)?;
        }
        Ok(())
    }

    /// the rowid and whole record of the table leaf cell at `offset` of `page`, its payload
    /// joined from the overflow pages it spills onto.
    fn cell(&self, page: &[u8], offset: usize) -> Result<(i64, Vec<u8>), Broken> {
        let (size, read) = varint(page, offset)?;
        let (rowid, read_id) = varint(page, offset + read)?;
        let size = usize::try_from(size).map_err(|_| broken("a row is larger than memory"))?;
        let start = offset + read + read_id;
        let local = self.local(size);
        let mut payload = page
            .get(start..start + local)
            .ok_or_else(|| broken("a row runs past its page"))?
            .to_vec();
        if local < size {
            let mut next = be32(page, start + local);
            let mut hops = 0;
            while payload.len() < size {
                hops += 1;
                if hops > self.pages {
                    return Err(broken("a row's overflow pages run in a circle"));
                }
                let overflow = self.page(next)?;
                let take = (size - payload.len()).min(self.usable - 4);
                payload.extend_from_slice(&overflow[4..4 + take]);
                next = be32(overflow, 0);
            }
        }
        Ok((rowid.cast_signed(), payload))
    }

    /// how much of a payload of `size` bytes a table leaf page holds itself.
    fn local(&self, size: usize) -> usize {
        let usable = self.usable;
        let most = usable - 35;
        if size <= most {
            return size;
        }
        let least = (usable - 12) * 32 / 255 - 23;
        let spill = least + (size - least) % (usable - 4);
        if spill <= most { spill } else { least }
    }
}

/// the file at `path`, or none when there is none. one larger than any browser's is refused.
fn read(path: &Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut bytes = Vec::new();
    file.take(LARGEST + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LARGEST {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "it is larger than any browser's cookie file",
        ));
    }
    Ok(Some(bytes))
}

/// the values of a record: its header's serial types, then each value they describe.
fn decode(record: &[u8]) -> Result<Vec<Value>, Broken> {
    let (header, mut at) = varint(record, 0)?;
    let header = usize::try_from(header).map_err(|_| broken("a row's header is too long"))?;
    let mut body = header;
    let mut values = Vec::new();
    while at < header {
        let (kind, read) = varint(record, at)?;
        at += read;
        let length = match kind {
            0 | 8 | 9 => 0,
            1 => 1,
            2 => 2,
            3 => 3,
            4 => 4,
            5 => 6,
            6 | 7 => 8,
            10 | 11 => return Err(broken("a row holds a value of a reserved kind")),
            _ => usize::try_from((kind - 12) / 2).map_err(|_| broken("a value is too long"))?,
        };
        let end = body
            .checked_add(length)
            .ok_or_else(|| broken("a row ends before its values do"))?;
        let bytes = record
            .get(body..end)
            .ok_or_else(|| broken("a row ends before its values do"))?;
        body = end;
        values.push(match kind {
            0 => Value::Null,
            8 => Value::Integer(0),
            9 => Value::Integer(1),
            1..=6 => {
                let mut integer = if bytes[0] & 0x80 == 0 { 0 } else { -1_i64 };
                for byte in bytes {
                    integer = (integer << 8) | i64::from(*byte);
                }
                Value::Integer(integer)
            }
            7 => Value::Real(f64::from_be_bytes(bytes.try_into().unwrap_or_default())),
            _ if kind % 2 == 0 => Value::Blob(bytes.to_vec()),
            _ => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
        });
    }
    Ok(values)
}

/// SQLite's variable-length integer at `at`: up to nine bytes, high bit first, and how many
/// bytes it took.
fn varint(bytes: &[u8], at: usize) -> Result<(u64, usize), Broken> {
    let mut value = 0_u64;
    for read in 0..9 {
        let byte = *bytes
            .get(at + read)
            .ok_or_else(|| broken("a number runs past the end of its page"))?;
        if read == 8 {
            return Ok(((value << 8) | u64::from(byte), 9));
        }
        value = (value << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok((value, read + 1));
        }
    }
    Ok((value, 9))
}

/// a value stored in a column of `affinity`, as SQLite reads it back: a REAL column keeps a whole
/// number as an integer to save room, and reads it as a real.
fn read_as(affinity: Affinity, value: Value) -> Value {
    match (affinity, value) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "SQLite converts the stored integer just so"
        )]
        (Affinity::Real, Value::Integer(integer)) => Value::Real(integer as f64),
        (_, value) => value,
    }
}

/// the affinity SQLite gives a column of the declared type `kind`, by its rules in order.
fn affinity(kind: &str) -> Affinity {
    let kind = kind.to_ascii_uppercase();
    let has = |part: &str| kind.contains(part);
    if has("INT") {
        Affinity::Integer
    } else if has("CHAR") || has("CLOB") || has("TEXT") {
        Affinity::Text
    } else if has("BLOB") || kind.trim().is_empty() {
        Affinity::Blob
    } else if has("REAL") || has("FLOA") || has("DOUB") {
        Affinity::Real
    } else {
        Affinity::Numeric
    }
}

/// the words that end a column's declared type and start its constraints.
const CONSTRAINTS: [&str; 11] = [
    "constraint",
    "primary",
    "not",
    "null",
    "unique",
    "check",
    "default",
    "collate",
    "references",
    "generated",
    "as",
];

/// the value of the literal `text` starts with, and what follows it: a number, a string, a
/// blob, `NULL`, `TRUE` or `FALSE`, or one of those in parentheses. none for anything else, as an
/// expression or a time is.
fn literal(text: &str) -> Option<(Value, &str)> {
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix('\'') {
        let mut value = String::new();
        let mut chars = rest.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c == '\'' {
                if chars.peek().map(|(_, next)| *next) != Some('\'') {
                    return Some((Value::Text(value), &rest[at + 1..]));
                }
                chars.next();
            }
            value.push(c);
        }
        return None;
    }
    if let Some(rest) = text.strip_prefix("X'").or_else(|| text.strip_prefix("x'")) {
        let end = rest.find('\'')?;
        let hex = &rest[..end];
        if hex.len() % 2 != 0 {
            return None;
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        return Some((Value::Blob(bytes), &rest[end + 1..]));
    }
    if let Some(rest) = text.strip_prefix('(') {
        let (value, after) = literal(rest)?;
        let after = after.trim_start().strip_prefix(')')?;
        return Some((value, after));
    }
    let length = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.' | '_')))
        .unwrap_or(text.len());
    let (word, rest) = text.split_at(length);
    let value = match word.to_ascii_lowercase().as_str() {
        "null" => Value::Null,
        "true" => Value::Integer(1),
        "false" => Value::Integer(0),
        _ => number(word)?,
    };
    Some((value, rest))
}

/// the number `word` is, as SQLite reads a numeric literal: an integer, in hex too, or a real.
fn number(word: &str) -> Option<Value> {
    let (negative, digits) = match word.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, word.strip_prefix('+').unwrap_or(word)),
    };
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        let bits = u64::from_str_radix(hex, 16).ok()?.cast_signed();
        return Some(Value::Integer(if negative {
            bits.wrapping_neg()
        } else {
            bits
        }));
    }
    if !digits.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let unsigned = word.trim_start_matches('+');
    if let Ok(integer) = unsigned.parse::<i64>() {
        return Some(Value::Integer(integer));
    }
    unsigned
        .parse::<f64>()
        .ok()
        .filter(|real| real.is_finite())
        .map(Value::Real)
}

/// `value`, a column's default, given the column's `affinity`, as SQLite reads it for a row that
/// predates the column. none where this reader would have to guess at SQLite's text for a real.
fn defaulted(value: Value, affinity: Affinity) -> Option<Value> {
    let numeric = |text: &str| -> Option<Value> {
        let text = text.trim();
        if let Ok(integer) = text.parse::<i64>() {
            return Some(Value::Integer(integer));
        }
        text.parse::<f64>()
            .ok()
            .filter(|real| real.is_finite() && !text.is_empty())
            .map(Value::Real)
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::float_cmp,
        reason = "a real converts only when it holds the integer exactly, as SQLite checks"
    )]
    let whole = |real: f64| {
        let integer = real as i64;
        (integer as f64 == real && integer > i64::MIN && integer < i64::MAX)
            .then_some(Value::Integer(integer))
    };
    Some(match (affinity, value) {
        (Affinity::Blob, value)
        | (_, value @ (Value::Null | Value::Blob(_)))
        | (Affinity::Text, value @ Value::Text(_)) => value,
        (Affinity::Text, Value::Integer(integer)) => Value::Text(integer.to_string()),
        (Affinity::Text, Value::Real(_)) => return None,
        (Affinity::Real, Value::Text(text)) => match numeric(&text) {
            Some(number) => read_as(Affinity::Real, number),
            None => Value::Text(text),
        },
        (Affinity::Real, value) => read_as(Affinity::Real, value),
        (Affinity::Numeric | Affinity::Integer, Value::Text(text)) => match numeric(&text) {
            Some(Value::Real(real)) => whole(real).unwrap_or(Value::Real(real)),
            Some(number) => number,
            None => Value::Text(text),
        },
        (Affinity::Numeric | Affinity::Integer, Value::Real(real)) => {
            whole(real).unwrap_or(Value::Real(real))
        }
        (Affinity::Numeric | Affinity::Integer, value) => value,
    })
}

/// the column a part of a `CREATE TABLE` statement declares, or none for a table constraint.
fn column(part: &str) -> Option<Column> {
    let part = part.trim();
    let first = part.split_whitespace().next()?;
    let constraint = ["constraint", "primary", "unique", "check", "foreign"]
        .iter()
        .any(|word| first.eq_ignore_ascii_case(word));
    if constraint {
        return None;
    }
    // a name in quotes may hold spaces, so it ends where its quote closes
    let (name, rest) = match part.chars().next()? {
        open @ ('"' | '`' | '[') => {
            let close = if open == '[' { ']' } else { open };
            let end = part[1..].find(close)? + 1;
            (&part[1..end], &part[end + 1..])
        }
        _ => part.split_at(first.len()),
    };
    let words: Vec<&str> = rest.split_whitespace().collect();
    let typed = words
        .iter()
        .take_while(|word| {
            !CONSTRAINTS
                .iter()
                .any(|constraint| word.eq_ignore_ascii_case(constraint))
        })
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let affinity = affinity(&typed);
    let lowered: Vec<String> = words.iter().map(|word| word.to_ascii_lowercase()).collect();
    let rowid = lowered.first().is_some_and(|kind| kind == "integer")
        && lowered.windows(2).any(|pair| pair == ["primary", "key"]);
    let default = match find_word(rest, "default") {
        Some(after) => literal(after).and_then(|(value, _)| defaulted(value, affinity)),
        None => Some(Value::Null),
    };
    Some(Column {
        name: name.to_owned(),
        rowid,
        affinity,
        default,
    })
}

/// what follows the word `word` in `text`, outside quotes, ignoring case.
fn find_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let mut quote = None;
    for (at, c) in text.char_indices() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'' | '`') => quote = Some(c),
            (None, '[') => quote = Some(']'),
            (None, _)
                if text[at..]
                    .get(..word.len())
                    .is_some_and(|here| here.eq_ignore_ascii_case(word))
                    && text[..at]
                        .chars()
                        .next_back()
                        .is_none_or(|before| !before.is_ascii_alphanumeric() && before != '_')
                    && text[at + word.len()..]
                        .chars()
                        .next()
                        .is_none_or(|after| !after.is_ascii_alphanumeric() && after != '_') =>
            {
                return Some(&text[at + word.len()..]);
            }
            _ => {}
        }
    }
    None
}

/// the columns a `CREATE TABLE` statement names, in order.
fn columns(sql: &str) -> Vec<Column> {
    let Some(open) = sql.find('(') else {
        return Vec::new();
    };
    // the last `)` after the first `(`, which a broken statement may hold before it
    let close = sql[open..].rfind(')').map_or(sql.len(), |at| open + at);
    let body = &sql[open + 1..close];
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut start) = (0_usize, None, 0);
    for (at, c) in body.char_indices() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'' | '`') => quote = Some(c),
            (None, '[') => quote = Some(']'),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.saturating_sub(1),
            (None, ',') if depth == 0 => {
                parts.push(&body[start..at]);
                start = at + 1;
            }
            // inside quotes, or a character that changes nothing
            _ => {}
        }
    }
    parts.push(&body[start..]);
    parts.into_iter().filter_map(column).collect()
}

/// SQLite's running checksum of a log, over `bytes` as 32-bit words in the log's byte order.
fn checksum((mut first, mut second): (u32, u32), bytes: &[u8], big: bool) -> (u32, u32) {
    for pair in bytes.as_chunks::<8>().0 {
        let word = |at: usize| {
            let four = [pair[at], pair[at + 1], pair[at + 2], pair[at + 3]];
            if big {
                u32::from_be_bytes(four)
            } else {
                u32::from_le_bytes(four)
            }
        };
        first = first.wrapping_add(word(0)).wrapping_add(second);
        second = second.wrapping_add(word(4)).wrapping_add(first);
    }
    (first, second)
}

fn be16(bytes: &[u8], at: usize) -> u16 {
    bytes
        .get(at..at + 2)
        .map_or(0, |two| u16::from_be_bytes([two[0], two[1]]))
}

fn be32(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 4).map_or(0, |four| {
        u32::from_be_bytes([four[0], four[1], four[2], four[3]])
    })
}

#[cfg(test)]
mod tests;

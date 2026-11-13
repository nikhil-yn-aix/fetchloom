//! databases a fuzzer plans, built page by page as SQLite lays them out: pages of 512 bytes to
//! 64 KiB, thousands of rows, values that spill onto overflow pages, an index, free pages that
//! still hold old rows, and a write-ahead log of later commits whose checksums hold. edits then
//! bend what was built, as a crafted file would: pointers turned into cycles and shared pages, a
//! cell pointer listed twice, a commit claiming pages it lacks, a statement or a row broken, a
//! byte changed, a file cut short.

use std::time::{Duration, SystemTime};

use super::super::{Database, Value, be16, be32, checksum, varint};
use super::put_varint;

/// the most row bytes one version of a planned table holds, so one input stays quick.
const MOST: usize = 128 << 10;
/// the most row bytes the reader takes for one input: past any planned table, and far short of
/// what pages shared by many parents would make it take.
const BUDGET: u64 = 4 << 20;
/// the moment the browsers' readers judge expiry at.
const NOW: u64 = 1_700_000_000;
/// the hosts a planned cookie belongs to, the one the readers ask for among them.
const HOSTS: [&str; 5] = [".data.lab", "data.lab", "sub.data.lab", ".other.lab", "lab"];

/// a fuzzer's bytes read as choices, in order, with zeros once they run out.
struct Take<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Take<'_> {
    fn byte(&mut self) -> u8 {
        let byte = self.bytes.get(self.at).copied().unwrap_or(0);
        self.at = self.at.saturating_add(1);
        byte
    }

    fn u16(&mut self) -> u16 {
        u16::from_be_bytes([self.byte(), self.byte()])
    }

    fn u32(&mut self) -> u32 {
        u32::from_be_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }

    fn u64(&mut self) -> u64 {
        (u64::from(self.u32()) << 32) | u64::from(self.u32())
    }

    fn more(&self) -> bool {
        self.at < self.bytes.len()
    }

    /// text of the length the next byte gives.
    fn text(&mut self) -> String {
        let length = usize::from(self.byte());
        let start = self.at.min(self.bytes.len());
        let end = start.saturating_add(length).min(self.bytes.len());
        self.at = self.at.saturating_add(length);
        String::from_utf8_lossy(&self.bytes[start..end]).into_owned()
    }
}

/// a small generator a plan seeds, so a few bytes plan thousands of rows.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n.max(1) as u64)).unwrap_or(0)
    }

    fn letters(&mut self, length: usize) -> String {
        (0..length)
            .map(|_| char::from(b"abcdefghijklmnopqrstuvwxyz0123456789"[self.below(36)]))
            .collect()
    }

    fn bytes(&mut self, length: usize) -> Vec<u8> {
        (0..length).map(|_| self.next().to_le_bytes()[0]).collect()
    }
}

/// whose cookie file a plan builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Browser {
    Firefox,
    Chromium,
    /// a table named `cookies` made by any statement at all, as no browser writes one.
    Raw,
}

/// a change made to a built database, as a crafted file would differ from one SQLite wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Edit {
    /// points a child pointer of `page` at `target`: the right one from `slot` 128 on, else
    /// the cell's at `slot`.
    Point { page: u16, slot: u8, target: u16 },
    /// makes `page` an interior page whose cells point at `children`, then at `right`.
    Interior {
        page: u16,
        children: Vec<u16>,
        right: u16,
    },
    /// points a row's first overflow page at `target`.
    Spill { pick: u16, target: u16 },
    /// lists the cell pointer at `from` again at `to`.
    Repeat { page: u16, from: u8, to: u8 },
    /// makes the log's last commit claim `pages` pages.
    Claim(u32),
    /// sets the byte at `at` of the file, or of the log.
    Byte { log: bool, at: u32, value: u8 },
    /// gives a row a payload of the most bytes a number can say.
    Size(u16),
    /// gives a row's record a header of the most bytes a number can say.
    Header(u16),
    /// sets a character of the table's statement.
    Sql { at: u8, value: u8 },
    /// cuts the file, or the log, to `length` bytes.
    Cut { log: bool, length: u32 },
    /// points a row's first overflow page at itself.
    Loop(u16),
    /// leaves the log's checksums as the edits left them.
    Unsum,
}

/// what a fuzzer's bytes plan: a database, its log and the edits that bend them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub(super) browser: Browser,
    /// pages of 512 bytes doubled this many times.
    pub(super) size: u8,
    /// past 224, the bytes each page leaves at its end for an extension.
    pub(super) reserved: u8,
    /// how many rows, on a scale that keeps most plans small and quick: below 128, up to 31;
    /// below 240, up to 444; from 240, up to 3,840.
    pub(super) rows: u8,
    pub(super) seed: u64,
    /// every this many rows, one holds a value of `3 * long + 1` bytes; none at 0.
    pub(super) long_every: u8,
    pub(super) long: u16,
    /// whether a chromium file holds an index on its `meta` table.
    pub(super) index: bool,
    /// free pages, which hold old rows.
    pub(super) stale: u8,
    /// commits in the log: none, one, two, or two and one never committed.
    pub(super) wal: u8,
    /// whether the log sums its words big-endian.
    pub(super) big: bool,
    pub(super) salts: u64,
    /// what each commit changes: every `2 + change % 8`th row, and `change / 8` rows added.
    pub(super) change: u8,
    pub(super) user_version: u8,
    /// a raw table's statement and its number of columns.
    pub(super) sql: String,
    pub(super) columns: u8,
    pub(super) edits: Vec<Edit>,
}

impl Plan {
    /// the plan `bytes` give, each field from the next bytes in turn.
    pub(super) fn read(bytes: &[u8]) -> Plan {
        let mut take = Take { bytes, at: 0 };
        let browser = match take.byte() % 3 {
            0 => Browser::Firefox,
            1 => Browser::Chromium,
            _ => Browser::Raw,
        };
        let mut plan = Plan {
            browser,
            size: take.byte() % 8,
            reserved: take.byte(),
            rows: take.byte(),
            seed: take.u64(),
            long_every: take.byte(),
            long: take.u16(),
            index: take.byte() % 2 == 1,
            stale: take.byte() % 4,
            wal: take.byte() % 4,
            big: take.byte() % 2 == 1,
            salts: take.u64(),
            change: take.byte(),
            user_version: take.byte(),
            sql: String::new(),
            columns: 0,
            edits: Vec::new(),
        };
        if browser == Browser::Raw {
            plan.sql = take.text();
            plan.columns = 1 + take.byte() % 6;
        }
        while take.more() {
            let op = take.byte();
            plan.edits.push(match op % 11 {
                _ if op == 255 => Edit::Unsum,
                0 => Edit::Point {
                    page: take.u16(),
                    slot: take.byte(),
                    target: take.u16(),
                },
                1 => {
                    let page = take.u16();
                    let count = 1 + take.byte() % 3;
                    Edit::Interior {
                        page,
                        children: (0..count).map(|_| take.u16()).collect(),
                        right: take.u16(),
                    }
                }
                2 => Edit::Spill {
                    pick: take.u16(),
                    target: take.u16(),
                },
                3 => Edit::Repeat {
                    page: take.u16(),
                    from: take.byte(),
                    to: take.byte(),
                },
                4 => Edit::Claim(take.u32()),
                5 => Edit::Byte {
                    log: take.byte() % 2 == 1,
                    at: take.u32(),
                    value: take.byte(),
                },
                6 => Edit::Size(take.u16()),
                7 => Edit::Header(take.u16()),
                8 => Edit::Sql {
                    at: take.byte(),
                    value: take.byte(),
                },
                9 => Edit::Cut {
                    log: take.byte() % 2 == 1,
                    length: take.u32(),
                },
                _ => Edit::Loop(take.u16()),
            });
        }
        plan
    }

    /// the bytes [`Plan::read`] reads back as this plan.
    pub(super) fn bytes(&self) -> Vec<u8> {
        let mut bytes = vec![
            match self.browser {
                Browser::Firefox => 0,
                Browser::Chromium => 1,
                Browser::Raw => 2,
            },
            self.size,
            self.reserved,
        ];
        bytes.push(self.rows);
        bytes.extend(self.seed.to_be_bytes());
        bytes.push(self.long_every);
        bytes.extend(self.long.to_be_bytes());
        bytes.extend([
            u8::from(self.index),
            self.stale,
            self.wal,
            u8::from(self.big),
        ]);
        bytes.extend(self.salts.to_be_bytes());
        bytes.extend([self.change, self.user_version]);
        if self.browser == Browser::Raw {
            bytes.push(u8::try_from(self.sql.len()).unwrap_or(u8::MAX));
            bytes.extend(self.sql.as_bytes());
            bytes.push(self.columns - 1);
        }
        for edit in &self.edits {
            match edit {
                Edit::Point { page, slot, target } => {
                    bytes.push(0);
                    bytes.extend(page.to_be_bytes());
                    bytes.push(*slot);
                    bytes.extend(target.to_be_bytes());
                }
                Edit::Interior {
                    page,
                    children,
                    right,
                } => {
                    bytes.push(1);
                    bytes.extend(page.to_be_bytes());
                    bytes.push(u8::try_from(children.len() - 1).unwrap_or(0));
                    for child in children {
                        bytes.extend(child.to_be_bytes());
                    }
                    bytes.extend(right.to_be_bytes());
                }
                Edit::Spill { pick, target } => {
                    bytes.push(2);
                    bytes.extend(pick.to_be_bytes());
                    bytes.extend(target.to_be_bytes());
                }
                Edit::Repeat { page, from, to } => {
                    bytes.push(3);
                    bytes.extend(page.to_be_bytes());
                    bytes.extend([*from, *to]);
                }
                Edit::Claim(pages) => {
                    bytes.push(4);
                    bytes.extend(pages.to_be_bytes());
                }
                Edit::Byte { log, at, value } => {
                    bytes.extend([5, u8::from(*log)]);
                    bytes.extend(at.to_be_bytes());
                    bytes.push(*value);
                }
                Edit::Size(pick) => {
                    bytes.push(6);
                    bytes.extend(pick.to_be_bytes());
                }
                Edit::Header(pick) => {
                    bytes.push(7);
                    bytes.extend(pick.to_be_bytes());
                }
                Edit::Sql { at, value } => bytes.extend([8, *at, *value]),
                Edit::Cut { log, length } => {
                    bytes.extend([9, u8::from(*log)]);
                    bytes.extend(length.to_be_bytes());
                }
                Edit::Loop(pick) => {
                    bytes.push(10);
                    bytes.extend(pick.to_be_bytes());
                }
                Edit::Unsum => bytes.push(255),
            }
        }
        bytes
    }

    fn page_size(&self) -> usize {
        512 << self.size.min(7)
    }

    fn row_count(&self) -> usize {
        let rows = usize::from(self.rows);
        match rows {
            0..128 => rows % 32,
            128..240 => (rows - 128) * 4,
            _ => (rows - 240) * 256,
        }
    }

    /// the bytes left unused at each page's end, when the page keeps the 480 SQLite needs.
    fn reserved(&self) -> usize {
        let reserved = usize::from(self.reserved.saturating_sub(224));
        if self.page_size() - reserved < 480 {
            0
        } else {
            reserved
        }
    }
}

/// how a column's values are made up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    /// the `INTEGER PRIMARY KEY`, kept as the rowid and stored as null.
    Rowid,
    Flag,
    Number,
    Time,
    Name,
    Host,
    Path,
    /// what keeps a cookie apart: a container, or another site's frames. mostly empty.
    Apart,
    Text,
    Empty,
    /// a chromium value: a version tag, then what AES sealed.
    Sealed,
    Any,
}

/// a table's name, statement and columns, the column a long value goes in, and the columns a
/// reader asks for.
struct Layout {
    table: &'static str,
    sql: String,
    columns: Vec<(String, Fill)>,
    long: usize,
    wanted: Vec<&'static str>,
}

const FIREFOX: [(&str, Fill); 13] = [
    ("id", Fill::Rowid),
    ("originAttributes", Fill::Apart),
    ("name", Fill::Name),
    ("value", Fill::Text),
    ("host", Fill::Host),
    ("path", Fill::Path),
    ("expiry", Fill::Time),
    ("lastAccessed", Fill::Time),
    ("creationTime", Fill::Time),
    ("isSecure", Fill::Flag),
    ("isHttpOnly", Fill::Flag),
    ("inBrowserElement", Fill::Flag),
    ("sameSite", Fill::Flag),
];

const CHROMIUM: [(&str, Fill); 20] = [
    ("creation_utc", Fill::Time),
    ("host_key", Fill::Host),
    ("top_frame_site_key", Fill::Apart),
    ("name", Fill::Name),
    ("value", Fill::Empty),
    ("encrypted_value", Fill::Sealed),
    ("path", Fill::Path),
    ("expires_utc", Fill::Time),
    ("is_secure", Fill::Flag),
    ("is_httponly", Fill::Flag),
    ("last_access_utc", Fill::Time),
    ("has_expires", Fill::Flag),
    ("is_persistent", Fill::Flag),
    ("priority", Fill::Flag),
    ("samesite", Fill::Flag),
    ("source_scheme", Fill::Flag),
    ("source_port", Fill::Number),
    ("last_update_utc", Fill::Time),
    ("source_type", Fill::Flag),
    ("has_cross_site_ancestor", Fill::Flag),
];

const META: &str = "CREATE TABLE meta(key LONGVARCHAR NOT NULL, value LONGVARCHAR)";
const INDEX: &str = "CREATE INDEX meta_key ON meta(key)";

fn layout(plan: &Plan) -> Layout {
    let named = |columns: &[(&str, Fill)]| {
        columns
            .iter()
            .map(|(name, fill)| ((*name).to_owned(), *fill))
            .collect::<Vec<_>>()
    };
    match plan.browser {
        Browser::Firefox => Layout {
            table: "moz_cookies",
            sql: "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, originAttributes TEXT NOT \
                  NULL DEFAULT '', name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, \
                  lastAccessed INTEGER, creationTime INTEGER, isSecure INTEGER, isHttpOnly \
                  INTEGER, inBrowserElement INTEGER DEFAULT 0, sameSite INTEGER DEFAULT 0)"
                .to_owned(),
            columns: named(&FIREFOX),
            long: 3,
            wanted: vec![
                "name",
                "value",
                "host",
                "path",
                "expiry",
                "isSecure",
                "originAttributes",
            ],
        },
        Browser::Chromium => Layout {
            table: "cookies",
            sql: format!(
                "CREATE TABLE cookies({})",
                CHROMIUM
                    .iter()
                    .map(|(name, fill)| {
                        let kind = match fill {
                            Fill::Host | Fill::Apart | Fill::Name | Fill::Empty | Fill::Path => {
                                "TEXT"
                            }
                            Fill::Sealed => "BLOB",
                            _ => "INTEGER",
                        };
                        format!("{name} {kind} NOT NULL")
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            columns: named(&CHROMIUM),
            long: 5,
            wanted: vec![
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
        },
        Browser::Raw => Layout {
            table: "cookies",
            sql: plan.sql.clone(),
            columns: (0..plan.columns)
                .map(|at| (format!("c{at}"), Fill::Any))
                .collect(),
            long: 0,
            wanted: vec!["name", "value", "host"],
        },
    }
}

/// a number of each size SQLite stores in its own way.
fn number(rng: &mut Rng) -> i64 {
    let random = rng.next().cast_signed();
    match rng.below(9) {
        0 => 0,
        1 => 1,
        2 => random % 128,
        3 => random % 32_768,
        4 => random % 8_388_608,
        5 => random % 2_147_483_648,
        6 => random % 140_737_488_355_328,
        7 => random,
        _ => -(random % 1_000).abs() - 2,
    }
}

/// a time as a browser keeps one, in seconds, milliseconds or microseconds since 1601, or one at
/// the ends of what a number holds.
fn time(rng: &mut Rng) -> i64 {
    let random = i64::try_from(rng.below(100_000_000)).unwrap_or(0);
    match rng.below(8) {
        0 => i64::MAX,
        1 => i64::MIN,
        2 => -1,
        3 => 0,
        4 => 1_700_000_000 + random,
        5 => 1_700_000_000_000 + random,
        6 => 13_340_000_000_000_000 + random,
        _ => rng.next().cast_signed(),
    }
}

/// a value of any kind, or text of `long` bytes.
fn any(rng: &mut Rng, long: Option<usize>) -> Value {
    if let Some(length) = long {
        return Value::Text(rng.letters(length));
    }
    match rng.below(6) {
        0 => Value::Null,
        1 => Value::Integer(number(rng)),
        2 => Value::Real(f64::from(i32::try_from(number(rng) % 1_000_000).unwrap_or(0)) / 7.0),
        3 => {
            let length = rng.below(40);
            Value::Text(rng.letters(length))
        }
        _ => {
            let length = rng.below(40);
            Value::Blob(rng.bytes(length))
        }
    }
}

/// a value for a column filled as `fill`, or one of `long` bytes.
fn fill(fill: Fill, rng: &mut Rng, long: Option<usize>) -> Value {
    match fill {
        Fill::Rowid => Value::Null,
        Fill::Flag => Value::Integer(i64::from(rng.below(2) == 1)),
        Fill::Number => Value::Integer(number(rng)),
        Fill::Time => Value::Integer(time(rng)),
        Fill::Name => Value::Text(format!("c{}", rng.below(60))),
        Fill::Host => Value::Text(HOSTS[rng.below(HOSTS.len())].to_owned()),
        Fill::Path => Value::Text(if rng.below(4) == 0 { "/a/b" } else { "/" }.to_owned()),
        Fill::Apart => Value::Text(
            if rng.below(8) == 0 {
                "^userContextId=2"
            } else {
                ""
            }
            .to_owned(),
        ),
        Fill::Text => {
            let length = long.unwrap_or_else(|| rng.below(40));
            Value::Text(rng.letters(length))
        }
        Fill::Empty => Value::Text(String::new()),
        Fill::Sealed => {
            let length = long.unwrap_or_else(|| 16 * (1 + rng.below(3)));
            let tag: &[u8] = if rng.below(3) == 0 { b"v11" } else { b"v10" };
            Value::Blob([tag, &rng.bytes(length)].concat())
        }
        Fill::Any => any(rng, long),
    }
}

/// a row of a table.
#[derive(Clone)]
struct Row {
    rowid: i64,
    values: Vec<Value>,
}

/// SQLite's record of `values`: a header of each value's serial type, then the values.
fn record(values: &[Value]) -> Vec<u8> {
    let mut kinds = Vec::new();
    let mut body = Vec::new();
    for value in values {
        let kind = match value {
            Value::Null => 0,
            Value::Integer(0) => 8,
            Value::Integer(1) => 9,
            Value::Integer(integer) => {
                let (kind, size) = match integer {
                    -128..=127 => (1, 1),
                    -32_768..=32_767 => (2, 2),
                    -8_388_608..=8_388_607 => (3, 3),
                    -2_147_483_648..=2_147_483_647 => (4, 4),
                    -140_737_488_355_328..=140_737_488_355_327 => (5, 6),
                    _ => (6, 8),
                };
                body.extend_from_slice(&integer.to_be_bytes()[8 - size..]);
                kind
            }
            Value::Real(real) => {
                body.extend_from_slice(&real.to_be_bytes());
                7
            }
            Value::Text(text) => {
                body.extend_from_slice(text.as_bytes());
                13 + 2 * text.len() as u64
            }
            Value::Blob(blob) => {
                body.extend_from_slice(blob);
                12 + 2 * blob.len() as u64
            }
        };
        kinds.extend(put_varint(kind));
    }
    // the header's length counts the number that says it
    let mut header = kinds.len() + 1;
    while put_varint(header as u64).len() + kinds.len() != header {
        header = put_varint(header as u64).len() + kinds.len();
    }
    [put_varint(header as u64), kinds, body].concat()
}

/// a big-endian `u16` of `value`, which a page's offsets and counts fit in.
fn short(value: usize) -> [u8; 2] {
    u16::try_from(value).unwrap_or(u16::MAX).to_be_bytes()
}

/// a leaf cell as it is being laid out: its rowid, its bytes, and where its first overflow
/// page's number sits in it, when it spills.
type Laid = (i64, Vec<u8>, Option<usize>);

/// where a table tree's pages and cells are.
#[derive(Default)]
struct Tree {
    leaves: Vec<u32>,
    interiors: Vec<u32>,
    /// each row's cell, by page and offset, and whether it spills.
    cells: Vec<(u32, usize, bool)>,
    /// each spilling row's first overflow page number, by page and offset.
    spills: Vec<(u32, usize)>,
}

impl Tree {
    fn leaf(&mut self, page: u32, offsets: &[usize], cells: &[Laid]) {
        self.leaves.push(page);
        for (offset, (_, _, spill)) in offsets.iter().zip(cells) {
            self.cells.push((page, *offset, spill.is_some()));
            if let Some(at) = spill {
                self.spills.push((page, offset + at));
            }
        }
    }
}

/// the pages of a database as it is built, each page's number its place in the list plus one.
struct Pages {
    size: usize,
    usable: usize,
    list: Vec<Vec<u8>>,
}

impl Pages {
    fn allocate(&mut self) -> u32 {
        self.list.push(vec![0; self.size]);
        u32::try_from(self.list.len()).unwrap_or(u32::MAX)
    }

    fn page(&mut self, number: u32) -> &mut [u8] {
        &mut self.list[number as usize - 1]
    }

    /// how much of a payload of `size` bytes a leaf page of a table, or of an index, keeps.
    fn local(&self, size: usize, table: bool) -> usize {
        let usable = self.usable;
        let most = if table {
            usable - 35
        } else {
            (usable - 12) * 64 / 255 - 23
        };
        let least = (usable - 12) * 32 / 255 - 23;
        if size <= most {
            return size;
        }
        let spill = least + (size - least) % (usable - 4);
        if spill <= most { spill } else { least }
    }

    /// puts `rest` on overflow pages, each naming the next. returns the first.
    fn spill(&mut self, rest: &[u8]) -> u32 {
        let chunks: Vec<&[u8]> = rest.chunks(self.usable - 4).collect();
        let numbers: Vec<u32> = chunks.iter().map(|_| self.allocate()).collect();
        for (at, chunk) in chunks.iter().enumerate() {
            let next = numbers.get(at + 1).copied().unwrap_or(0);
            let page = self.page(numbers[at]);
            page[..4].copy_from_slice(&next.to_be_bytes());
            page[4..4 + chunk.len()].copy_from_slice(chunk);
        }
        numbers[0]
    }

    /// a leaf cell of a table, for `rowid`, or of an index, holding `payload`, and where in the
    /// cell its first overflow page's number is, when it spills.
    fn cell(&mut self, rowid: Option<i64>, payload: &[u8]) -> (Vec<u8>, Option<usize>) {
        let local = self.local(payload.len(), rowid.is_some());
        let mut cell = put_varint(payload.len() as u64);
        if let Some(rowid) = rowid {
            cell.extend(put_varint(rowid.cast_unsigned()));
        }
        cell.extend_from_slice(&payload[..local]);
        if local == payload.len() {
            return (cell, None);
        }
        let at = cell.len();
        let first = self.spill(&payload[local..]);
        cell.extend_from_slice(&first.to_be_bytes());
        (cell, Some(at))
    }

    /// whether `cells` fit on a page of `header` bytes, `head` bytes into it.
    fn fit<'a>(
        &self,
        head: usize,
        header: usize,
        cells: impl Iterator<Item = &'a Vec<u8>>,
    ) -> bool {
        head + header + cells.map(|cell| cell.len() + 2).sum::<usize>() <= self.usable
    }

    /// lays page `number`, `head` bytes into it, out as a b-tree page of `kind` holding
    /// `cells`, with `right` the last child of an interior page. returns each cell's offset.
    fn lay(
        &mut self,
        number: u32,
        head: usize,
        kind: u8,
        cells: &[Vec<u8>],
        right: Option<u32>,
    ) -> Vec<usize> {
        let usable = self.usable;
        let page = self.page(number);
        let header = if right.is_some() { 12 } else { 8 };
        let mut end = usable;
        let mut offsets = Vec::new();
        for (at, cell) in cells.iter().enumerate() {
            end -= cell.len();
            page[end..end + cell.len()].copy_from_slice(cell);
            offsets.push(end);
            let pointer = head + header + 2 * at;
            page[pointer..pointer + 2].copy_from_slice(&short(end));
        }
        page[head] = kind;
        page[head + 1..head + 3].copy_from_slice(&[0, 0]);
        page[head + 3..head + 5].copy_from_slice(&short(cells.len()));
        page[head + 5..head + 7].copy_from_slice(&short(end % 65_536));
        page[head + 7] = 0;
        if let Some(right) = right {
            page[head + 8..head + 12].copy_from_slice(&right.to_be_bytes());
        }
        offsets
    }

    /// lays a full leaf of `group` out on a new page, as one child of the level above.
    fn leaf(&mut self, group: &mut Vec<Laid>, level: &mut Vec<(u32, i64)>, tree: &mut Tree) {
        let number = self.allocate();
        let cells: Vec<Vec<u8>> = group.iter().map(|(_, cell, _)| cell.clone()).collect();
        let offsets = self.lay(number, 0, 0x0d, &cells, None);
        tree.leaf(number, &offsets, group);
        level.push((number, group.last().map_or(0, |(rowid, _, _)| *rowid)));
        group.clear();
    }

    /// lays an interior page over `children` out on page `number`, `head` bytes into it.
    fn interior(&mut self, number: u32, head: usize, children: &[(u32, i64)], tree: &mut Tree) {
        let right = children.last().map_or(0, |(page, _)| *page);
        self.lay(number, head, 0x05, &pointers(children), Some(right));
        tree.interiors.push(number);
    }

    /// a table tree of `rows`, each a rowid and its record, whose root is page `root`, `head`
    /// bytes into it, built as a bulk load fills one: leaves full from the left, and each level
    /// of interior pages above them keyed by the largest rowid below each child.
    fn table(&mut self, root: u32, head: usize, rows: &[(i64, Vec<u8>)]) -> Tree {
        let mut tree = Tree::default();
        let cells: Vec<Laid> = rows
            .iter()
            .map(|(rowid, record)| {
                let (cell, spill) = self.cell(Some(*rowid), record);
                (*rowid, cell, spill)
            })
            .collect();
        if self.fit(head, 8, cells.iter().map(|(_, cell, _)| cell)) {
            let laid: Vec<Vec<u8>> = cells.iter().map(|(_, cell, _)| cell.clone()).collect();
            let offsets = self.lay(root, head, 0x0d, &laid, None);
            tree.leaf(root, &offsets, &cells);
            return tree;
        }
        let mut level = Vec::new();
        let mut group: Vec<Laid> = Vec::new();
        let mut used = 8;
        for cell in cells {
            let size = cell.1.len() + 2;
            if !group.is_empty() && used + size > self.usable {
                self.leaf(&mut group, &mut level, &mut tree);
                used = 8;
            }
            used += size;
            group.push(cell);
        }
        self.leaf(&mut group, &mut level, &mut tree);
        let cell = |(_, key): &(u32, i64)| 4 + put_varint(key.cast_unsigned()).len() + 2;
        loop {
            if self.fit(head, 12, pointers(&level).iter()) {
                self.interior(root, head, &level, &mut tree);
                return tree;
            }
            let mut groups: Vec<Vec<(u32, i64)>> = vec![Vec::new()];
            let mut used = 12;
            for child in level {
                let grown = groups
                    .last()
                    .and_then(|group| group.last())
                    .map(|last| used + cell(last));
                match grown {
                    Some(grown) if grown > self.usable => {
                        groups.push(Vec::new());
                        used = 12;
                    }
                    Some(grown) => used = grown,
                    None => {}
                }
                if let Some(group) = groups.last_mut() {
                    group.push(child);
                }
            }
            // a lone last child takes one from the page before, so no interior page is empty
            let count = groups.len();
            if count > 1
                && groups[count - 1].len() == 1
                && let Some(moved) = groups[count - 2].pop()
            {
                groups[count - 1].insert(0, moved);
            }
            level = groups
                .into_iter()
                .map(|children| {
                    let number = self.allocate();
                    self.interior(number, 0, &children, &mut tree);
                    (number, children.last().map_or(0, |(_, key)| *key))
                })
                .collect();
        }
    }
}

/// the cells of an interior page over `children`: each child but the last, which is the
/// page's right pointer, with the largest rowid below it.
fn pointers(children: &[(u32, i64)]) -> Vec<Vec<u8>> {
    children[..children.len().saturating_sub(1)]
        .iter()
        .map(|(page, key)| [page.to_be_bytes().to_vec(), put_varint(key.cast_unsigned())].concat())
        .collect()
}

/// one version of the database, page by page, and where its cookie table is.
struct Image {
    pages: Vec<Vec<u8>>,
    tree: Tree,
    /// where the table's statement is on page 1, when all of it is there.
    sql: Option<(usize, usize)>,
}

/// the record of a table or index named in the schema.
fn entry(kind: &str, name: &str, table: &str, root: u32, sql: &str) -> Vec<u8> {
    record(&[
        Value::Text(kind.to_owned()),
        Value::Text(name.to_owned()),
        Value::Text(table.to_owned()),
        Value::Integer(i64::from(root)),
        Value::Text(sql.to_owned()),
    ])
}

/// the pages of the database holding `rows` and, for chromium, `meta`, as commit `counter`
/// of `plan` leaves them.
fn image(plan: &Plan, layout: &Layout, rows: &[Row], meta: &[Row], counter: u32) -> Image {
    let size = plan.page_size();
    let mut pages = Pages {
        size,
        usable: size - plan.reserved(),
        list: Vec::new(),
    };
    pages.allocate();
    let chromium = plan.browser == Browser::Chromium;
    let meta_root = chromium.then(|| pages.allocate());
    let root = pages.allocate();
    let index_root = (chromium && plan.index).then(|| pages.allocate());
    let records: Vec<(i64, Vec<u8>)> = rows
        .iter()
        .map(|row| (row.rowid, record(&row.values)))
        .collect();
    let tree = pages.table(root, 0, &records);
    let mut schema = Vec::new();
    if let Some(meta_root) = meta_root {
        let records: Vec<(i64, Vec<u8>)> = meta
            .iter()
            .map(|row| (row.rowid, record(&row.values)))
            .collect();
        pages.table(meta_root, 0, &records);
        schema.push(entry("table", "meta", "meta", meta_root, META));
    }
    let at = schema.len();
    schema.push(entry(
        "table",
        layout.table,
        layout.table,
        root,
        &layout.sql,
    ));
    if let Some(index_root) = index_root {
        let mut keys: Vec<(String, i64)> = meta
            .iter()
            .map(|row| {
                (
                    row.values[0].text().unwrap_or_default().to_owned(),
                    row.rowid,
                )
            })
            .collect();
        keys.sort();
        let cells: Vec<Vec<u8>> = keys
            .into_iter()
            .map(|(key, rowid)| {
                let entry = record(&[Value::Text(key), Value::Integer(rowid)]);
                pages.cell(None, &entry).0
            })
            .collect();
        pages.lay(index_root, 0, 0x0a, &cells, None);
        schema.push(entry("index", "meta_key", "meta", index_root, INDEX));
    }
    let schema: Vec<(i64, Vec<u8>)> = (1..).zip(schema).collect();
    let listed = pages.table(1, 100, &schema);
    let sql = listed
        .cells
        .get(at)
        .filter(|(page, _, spills)| *page == 1 && !spills)
        .map(|(_, offset, _)| {
            let first = &pages.list[0];
            let (size, read) = varint(first, *offset).unwrap_or((0, 1));
            let (_, read_id) = varint(first, offset + read).unwrap_or((0, 1));
            let end = offset + read + read_id + usize::try_from(size).unwrap_or(0);
            (end - layout.sql.len(), layout.sql.len())
        });
    let free = free(&mut pages, plan.stale, &tree.leaves);
    header(&mut pages, plan, counter, &free);
    Image {
        pages: pages.list,
        tree,
        sql,
    }
}

/// `stale` free pages, a trunk listing the rest, which still hold the rows of `leaves`, as
/// pages a table gave up do.
fn free(pages: &mut Pages, stale: u8, leaves: &[u32]) -> Vec<u32> {
    let free: Vec<u32> = (0..stale).map(|_| pages.allocate()).collect();
    if let Some((&trunk, rest)) = free.split_first() {
        let numbers: Vec<u8> = rest.iter().flat_map(|leaf| leaf.to_be_bytes()).collect();
        let count = u32::try_from(rest.len()).unwrap_or(0);
        let page = pages.page(trunk);
        page[4..8].copy_from_slice(&count.to_be_bytes());
        page[8..8 + numbers.len()].copy_from_slice(&numbers);
        for (old, leaf) in leaves.iter().zip(rest) {
            let old = pages.list[*old as usize - 1].clone();
            pages.page(*leaf).copy_from_slice(&old);
        }
    }
    free
}

/// the database header on page 1, for commit `counter` of `plan` with the `free` pages.
fn header(pages: &mut Pages, plan: &Plan, counter: u32, free: &[u32]) {
    let size = pages.size;
    let count = u32::try_from(pages.list.len()).unwrap_or(u32::MAX);
    let first = pages.page(1);
    first[..16].copy_from_slice(b"SQLite format 3\0");
    let encoded = if size == 65_536 { [0, 1] } else { short(size) };
    first[16..18].copy_from_slice(&encoded);
    let mode = if plan.wal == 0 { 1 } else { 2 };
    first[18] = mode;
    first[19] = mode;
    first[20] = u8::try_from(plan.reserved()).unwrap_or(0);
    first[21..24].copy_from_slice(&[64, 32, 32]);
    first[24..28].copy_from_slice(&counter.to_be_bytes());
    first[28..32].copy_from_slice(&count.to_be_bytes());
    first[32..36].copy_from_slice(&free.first().copied().unwrap_or(0).to_be_bytes());
    first[36..40].copy_from_slice(&u32::try_from(free.len()).unwrap_or(0).to_be_bytes());
    first[40..44].copy_from_slice(&1_u32.to_be_bytes());
    first[44..48].copy_from_slice(&4_u32.to_be_bytes());
    first[56..60].copy_from_slice(&1_u32.to_be_bytes());
    let user_version = match plan.browser {
        Browser::Firefox => [12, 16, 17][usize::from(plan.user_version % 3)],
        _ => u32::from(plan.user_version),
    };
    first[60..64].copy_from_slice(&user_version.to_be_bytes());
    first[92..96].copy_from_slice(&counter.to_be_bytes());
    first[96..100].copy_from_slice(&3_046_000_u32.to_be_bytes());
}

/// the rows of each version a plan commits: the first, one with values changed and rows added,
/// one with rows deleted, and one changed again that is never committed.
fn versions(plan: &Plan, layout: &Layout, rng: &mut Rng) -> Vec<Vec<Row>> {
    let long = |at: usize| {
        (plan.long_every > 0 && at.is_multiple_of(usize::from(plan.long_every)))
            .then(|| (usize::from(plan.long) * 3 + 1).min(MOST / 2))
    };
    let row = |rng: &mut Rng, rowid: i64, at: usize| Row {
        rowid,
        values: layout
            .columns
            .iter()
            .enumerate()
            .map(|(column, (_, kind))| fill(*kind, rng, long(at).filter(|_| column == layout.long)))
            .collect(),
    };
    let mut first = Vec::new();
    let mut held = 0;
    let mut rowid = 0;
    for at in 0..plan.row_count() {
        rowid += 1 + i64::try_from(rng.below(3)).unwrap_or(0);
        let made = row(rng, rowid, at);
        held += record(&made.values).len();
        if held > MOST {
            break;
        }
        first.push(made);
    }
    let every = 2 + usize::from(plan.change % 8);
    let count = match plan.wal {
        0 => 1,
        1 => 2,
        2 => 3,
        _ => 4,
    };
    let mut versions = vec![first];
    while versions.len() < count {
        let mut next = versions[versions.len() - 1].clone();
        match versions.len() {
            1 => {
                for at in (0..next.len()).step_by(every) {
                    next[at] = row(rng, next[at].rowid, at);
                }
                for at in 0..usize::from(plan.change / 8) {
                    rowid += 1;
                    next.push(row(rng, rowid, at));
                }
            }
            2 => {
                let mut at = 0;
                next.retain(|_| {
                    at += 1;
                    at % (every + 1) != 0
                });
            }
            _ => {
                for at in (1..next.len()).step_by(every) {
                    next[at] = row(rng, next[at].rowid, at);
                }
            }
        }
        versions.push(next);
    }
    versions
}

/// a page of the log, with the database's size after its commit, or 0 for a frame that is no
/// commit.
struct Frame {
    page: u32,
    commit: u32,
    data: Vec<u8>,
}

/// the log a database of `page_size` bytes a page has with `frames`, its words summed
/// big-endian when `big`, its checksums not yet summed.
fn log(page_size: usize, big: bool, salts: u64, frames: &[Frame]) -> Vec<u8> {
    let magic: u32 = if big { 0x377f_0683 } else { 0x377f_0682 };
    let mut log = Vec::with_capacity(32 + frames.len() * (24 + page_size));
    log.extend(magic.to_be_bytes());
    log.extend(3_007_000_u32.to_be_bytes());
    log.extend(u32::try_from(page_size).unwrap_or(0).to_be_bytes());
    log.extend(0_u32.to_be_bytes());
    log.extend(salts.to_be_bytes());
    log.extend([0; 8]);
    for frame in frames {
        log.extend(frame.page.to_be_bytes());
        log.extend(frame.commit.to_be_bytes());
        log.extend(salts.to_be_bytes());
        log.extend([0; 8]);
        log.extend_from_slice(&frame.data);
    }
    log
}

/// sums `log`, as SQLite sums its header and then each frame, one after another.
fn resum(log: &mut [u8], page_size: usize, big: bool) {
    if log.len() < 32 {
        return;
    }
    let mut sums = checksum((0, 0), &log[..24], big);
    log[24..28].copy_from_slice(&sums.0.to_be_bytes());
    log[28..32].copy_from_slice(&sums.1.to_be_bytes());
    let mut at = 32;
    while at + 24 + page_size <= log.len() {
        sums = checksum(
            checksum(sums, &log[at..at + 8], big),
            &log[at + 24..at + 24 + page_size],
            big,
        );
        log[at + 16..at + 20].copy_from_slice(&sums.0.to_be_bytes());
        log[at + 20..at + 24].copy_from_slice(&sums.1.to_be_bytes());
        at += 24 + page_size;
    }
}

/// a database being bent: its file, its log's frames and how many of those are committed.
struct Bent {
    page_size: usize,
    file: Vec<u8>,
    frames: Vec<Frame>,
    committed: usize,
    pages: u32,
}

impl Bent {
    /// the copy of page `number` a reader takes: the newest committed in the log, else the
    /// file's.
    fn page(&mut self, number: u32) -> Option<&mut [u8]> {
        let logged = self.frames[..self.committed]
            .iter()
            .rposition(|frame| frame.page == number);
        if let Some(at) = logged {
            return Some(&mut self.frames[at].data);
        }
        let start = (number as usize).checked_sub(1)? * self.page_size;
        self.file.get_mut(start..start + self.page_size)
    }

    /// the page `pick` stands for, from 0 to one past the last.
    fn pick(&self, pick: u16) -> u32 {
        u32::from(pick) % (self.pages + 2)
    }

    /// sets the four bytes at `offset` of page `number` to `value`, where the page has them.
    fn put(&mut self, number: u32, offset: usize, value: u32) {
        if let Some(page) = self.page(number)
            && let Some(four) = page.get_mut(offset..offset + 4)
        {
            four.copy_from_slice(&value.to_be_bytes());
        }
    }

    /// where page `number` lists its cells, and how many: none for no page.
    fn pointers(&mut self, number: u32) -> Option<(usize, usize)> {
        let head = if number == 1 { 100 } else { 0 };
        let page = self.page(number)?;
        let interior = matches!(page.get(head), Some(0x05 | 0x02));
        Some((
            head + if interior { 12 } else { 8 },
            usize::from(be16(page, head + 3)),
        ))
    }

    /// points a child pointer of page `page` at `target`: the right one from `slot` 128 on, else
    /// the one in the cell at `slot`.
    fn point(&mut self, page: u16, slot: u8, target: u16) {
        let (number, target) = (self.pick(page), self.pick(target));
        let head = if number == 1 { 100 } else { 0 };
        let Some((pointers, count)) = self.pointers(number) else {
            return;
        };
        if slot >= 128 {
            if pointers == head + 12 {
                self.put(number, head + 8, target);
            }
        } else if count > 0 {
            let pointer = pointers + 2 * (usize::from(slot) % count);
            let offset = self
                .page(number)
                .map_or(0, |page| usize::from(be16(page, pointer)));
            self.put(number, offset, target);
        }
    }

    /// lists the cell pointer at `from` of page `page` again at `to`.
    fn repeat(&mut self, page: u16, from: u8, to: u8) {
        let number = self.pick(page);
        let Some((pointers, count)) = self.pointers(number).filter(|(_, count)| *count > 0) else {
            return;
        };
        let from = pointers + 2 * (usize::from(from) % count);
        let to = pointers + 2 * (usize::from(to) % count);
        if let Some(bytes) = self.page(number)
            && to + 2 <= bytes.len()
            && from + 2 <= bytes.len()
        {
            let pointer = [bytes[from], bytes[from + 1]];
            bytes[to..to + 2].copy_from_slice(&pointer);
        }
    }

    /// makes the cell at `offset` of page `page` claim the most bytes a number says: as its
    /// payload, or as its record's header when `header`.
    fn swell(&mut self, page: u32, offset: usize, header: bool) {
        let Some(bytes) = self.page(page) else {
            return;
        };
        let mut at = offset;
        if header {
            for _ in 0..2 {
                at += varint(bytes, at).map_or(1, |(_, read)| read);
            }
        }
        let end = at.saturating_add(9).min(bytes.len());
        bytes[at.min(end)..end].fill(0xff);
    }

    /// makes `edit` to the database, whose pages as a reader sees them `image` maps. edits
    /// of the log wait in `later` until it is written.
    fn bend(&mut self, edit: &Edit, image: &Image, later: &mut Vec<Edit>) {
        let cells = &image.tree.cells;
        let spills = &image.tree.spills;
        match edit {
            Edit::Point { page, slot, target } => self.point(*page, *slot, *target),
            Edit::Interior {
                page,
                children,
                right,
            } => {
                let number = self.pick(*page);
                let children: Vec<u32> = children.iter().map(|child| self.pick(*child)).collect();
                let right = self.pick(*right);
                let size = self.page_size;
                if number >= 2
                    && let Some(bytes) = self.page(number)
                {
                    bytes.copy_from_slice(&super::interior(size, &children, right));
                }
            }
            Edit::Spill { pick, target } => {
                if let Some((page, offset)) = spills.get(usize::from(*pick) % spills.len().max(1)) {
                    let target = self.pick(*target);
                    self.put(*page, *offset, target);
                }
            }
            Edit::Loop(pick) => {
                if let Some((page, offset)) = spills.get(usize::from(*pick) % spills.len().max(1)) {
                    let first = self.page(*page).map_or(0, |bytes| be32(bytes, *offset));
                    self.put(first, 0, first);
                }
            }
            Edit::Repeat { page, from, to } => self.repeat(*page, *from, *to),
            Edit::Claim(pages) => {
                if let Some(last) = self.committed.checked_sub(1) {
                    self.frames[last].commit = *pages;
                }
            }
            Edit::Byte { log: true, .. } | Edit::Cut { log: true, .. } | Edit::Unsum => {
                later.push(edit.clone());
            }
            Edit::Byte { at, value, .. } => {
                if !self.file.is_empty() {
                    let at = *at as usize % self.file.len();
                    self.file[at] = *value;
                }
            }
            Edit::Size(pick) | Edit::Header(pick) => {
                if let Some((page, offset, _)) = cells.get(usize::from(*pick) % cells.len().max(1))
                {
                    self.swell(*page, *offset, matches!(edit, Edit::Header(_)));
                }
            }
            Edit::Sql { at, value } => {
                let Some((offset, length)) = image.sql.filter(|(_, length)| *length > 0) else {
                    return;
                };
                let value = if *value < 128 {
                    b"()',\" x"[usize::from(*value) % 7]
                } else {
                    *value
                };
                if let Some(bytes) = self.page(1) {
                    bytes[offset + usize::from(*at) % length] = value;
                }
            }
            Edit::Cut { length, .. } => {
                let length = *length as usize % (self.file.len() + 1);
                self.file.truncate(length);
            }
        }
    }
}

/// a built database, with its log, and what reading it should give.
pub(super) struct Built {
    pub(super) file: Vec<u8>,
    pub(super) log: Option<Vec<u8>>,
    pub(super) browser: Browser,
    pub(super) table: &'static str,
    pub(super) wanted: Vec<&'static str>,
    /// the rows the reader must give, when no edit bent what was built.
    pub(super) expected: Option<Vec<Vec<Value>>>,
}

/// the frames of a log that takes each of `images` after the first, the pages each changed, each
/// up to `committed` ending in a commit; and how many frames are committed.
fn frames(images: &[Image], committed: usize) -> (Vec<Frame>, usize) {
    let mut frames = Vec::new();
    let mut last_commit = 0;
    for (version, image) in images.iter().enumerate().skip(1) {
        let before = &images[version - 1].pages;
        for (at, page) in image.pages.iter().enumerate() {
            if before.get(at) != Some(page) {
                frames.push(Frame {
                    page: u32::try_from(at + 1).unwrap_or(u32::MAX),
                    commit: 0,
                    data: page.clone(),
                });
            }
        }
        if version <= committed
            && let Some(frame) = frames.last_mut()
        {
            frame.commit = u32::try_from(image.pages.len()).unwrap_or(u32::MAX);
            last_commit = frames.len();
        }
    }
    (frames, last_commit)
}

/// the log of `frames` with the edits that waited for it, `later`, made, and summed again
/// unless one asked it not to be.
fn written(plan: &Plan, frames: &[Frame], later: &[Edit]) -> Vec<u8> {
    let mut bytes = log(plan.page_size(), plan.big, plan.salts, frames);
    let mut sum = true;
    for edit in later {
        match edit {
            Edit::Byte { at, value, .. } => {
                if !bytes.is_empty() {
                    let at = *at as usize % bytes.len();
                    bytes[at] = *value;
                }
            }
            Edit::Cut { length, .. } => bytes.truncate(*length as usize % (bytes.len() + 1)),
            _ => sum = false,
        }
    }
    if sum {
        resum(&mut bytes, plan.page_size(), plan.big);
    }
    bytes
}

/// the values of `rows` a reader asks `layout`'s table for, the rowid for its alias and null
/// for a column the table lacks.
fn expected(layout: &Layout, rows: &[Row]) -> Vec<Vec<Value>> {
    let at: Vec<Option<usize>> = layout
        .wanted
        .iter()
        .map(|name| {
            layout
                .columns
                .iter()
                .position(|(column, _)| column.eq_ignore_ascii_case(name))
        })
        .collect();
    rows.iter()
        .map(|row| {
            at.iter()
                .map(|at| match at {
                    Some(at) if layout.columns[*at].1 == Fill::Rowid => Value::Integer(row.rowid),
                    Some(at) => row.values[*at].clone(),
                    None => Value::Null,
                })
                .collect()
        })
        .collect()
}

/// the database `plan` builds and bends, and what reading it should give.
pub(super) fn build(plan: &Plan) -> Built {
    let layout = layout(plan);
    let mut rng = Rng(plan.seed | 1);
    let versions = versions(plan, &layout, &mut rng);
    let meta = [
        ("version", ["24", "23", "20", "x"][rng.below(4)]),
        ("last_compatible_version", "24"),
    ]
    .into_iter()
    .zip(1..)
    .map(|((key, value), rowid)| Row {
        rowid,
        values: vec![Value::Text(key.to_owned()), Value::Text(value.to_owned())],
    })
    .collect::<Vec<_>>();
    let images: Vec<Image> = versions
        .iter()
        .zip(1..)
        .map(|(rows, counter)| image(plan, &layout, rows, &meta, counter))
        .collect();
    // the last version of four is never committed
    let committed = versions.len().min(3) - 1;
    let (frames, last_commit) = frames(&images, committed);
    let seen = &images[committed];
    let mut bent = Bent {
        page_size: plan.page_size(),
        file: images[0].pages.concat(),
        frames,
        committed: last_commit,
        pages: u32::try_from(seen.pages.len()).unwrap_or(u32::MAX),
    };
    let mut later = Vec::new();
    for edit in &plan.edits {
        bent.bend(edit, seen, &mut later);
    }
    let log = (versions.len() > 1).then(|| written(plan, &bent.frames, &later));
    let expected = (plan.edits.is_empty() && plan.browser != Browser::Raw)
        .then(|| expected(&layout, &versions[committed]));
    Built {
        file: bent.file,
        log,
        browser: plan.browser,
        table: layout.table,
        wanted: layout.wanted,
        expected,
    }
}

/// how many cells the table leaf pages of `file` and `log` hold, each copy counted, at the page
/// size the file's header gives: no reader that reads each page once gives more rows.
fn leaf_cells(file: &[u8], log: Option<&[u8]>) -> usize {
    let page_size = match file
        .get(16..18)
        .map(|two| u16::from_be_bytes([two[0], two[1]]))
    {
        Some(1) => 65_536,
        Some(size) if size >= 512 && size.is_power_of_two() => usize::from(size),
        _ => return 0,
    };
    let cells = |number: u32, page: &[u8]| {
        let head = if number == 1 { 100 } else { 0 };
        if page.get(head) == Some(&0x0d) {
            usize::from(be16(page, head + 3))
        } else {
            0
        }
    };
    let mut total: usize = file
        .chunks(page_size)
        .zip(1..)
        .map(|(page, number)| cells(number, page))
        .sum();
    if let Some(log) = log {
        let mut at = 32;
        while at + 24 + page_size <= log.len() {
            total += cells(be32(log, at), &log[at + 24..at + 24 + page_size]);
            at += 24 + page_size;
        }
    }
    total
}

/// reads what `input` plans with the reader, and with the browsers' readers over it: it never
/// panics, never gives more rows than the file and its log hold, and gives what was built when
/// nothing bent it.
pub(super) fn exercise(input: &[u8]) {
    let built = build(&Plan::read(input));
    let bound = leaf_cells(&built.file, built.log.as_deref());
    let mut database = match Database::parse(built.file, built.log.as_deref()) {
        Ok(database) => database,
        Err(why) => {
            assert!(
                built.expected.is_none(),
                "a database built unbent was refused: {why:?}"
            );
            return;
        }
    };
    database.budget = BUDGET;
    let read = database.rows(built.table, &built.wanted);
    if let Ok(rows) = &read {
        assert!(
            rows.len() <= bound,
            "{} rows from {bound} cells",
            rows.len()
        );
    }
    if let Some(expected) = &built.expected {
        assert_eq!(
            read.as_ref().ok(),
            Some(expected),
            "{:?}",
            read.as_ref().err()
        );
    }
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(NOW);
    let found = match built.browser {
        Browser::Firefox => crate::auth::cookies::firefox::taken(&database).map(|read| read.found),
        Browser::Chromium => {
            crate::auth::cookies::chromium::taken(&database, "data.lab", &|_| Some([7; 16]))
                .map(|read| read.found)
        }
        Browser::Raw => return,
    };
    if let Ok(found) = found {
        let _ = crate::auth::cookies::pick(found, "data.lab", now);
    }
}

/// a plan of the rows `rows` scales to in pages of 512 bytes doubled `size` times, with nothing
/// else asked.
fn plain(browser: Browser, size: u8, rows: u8) -> Plan {
    Plan {
        browser,
        size,
        reserved: 0,
        rows,
        seed: 7,
        long_every: 0,
        long: 0,
        index: false,
        stale: 0,
        wal: 0,
        big: false,
        salts: 0x5eed_0001_5eed_0002,
        change: 0,
        user_version: 1,
        sql: String::new(),
        columns: 0,
        edits: Vec::new(),
    }
}

/// the plans the weekly fuzz pass starts from: two browser files as they are, and one for each
/// way a crafted file has broken the reader.
fn seeds() -> Vec<(&'static str, Plan)> {
    vec![
        (
            "firefox-in-a-log",
            Plan {
                long_every: 11,
                long: 1_000,
                stale: 2,
                wal: 3,
                change: 0x2d,
                ..plain(Browser::Firefox, 3, 203)
            },
        ),
        (
            "chromium-small-pages",
            Plan {
                reserved: 240,
                long_every: 7,
                long: 300,
                index: true,
                stale: 1,
                wal: 2,
                big: true,
                change: 0x13,
                ..plain(Browser::Chromium, 0, 238)
            },
        ),
        (
            "cycle",
            Plan {
                edits: vec![Edit::Point {
                    page: 3,
                    slot: 200,
                    target: 3,
                }],
                ..plain(Browser::Chromium, 0, 238)
            },
        ),
        (
            "shared",
            Plan {
                edits: (2..=25)
                    .map(|page| Edit::Interior {
                        page,
                        children: vec![page + 1, page + 1],
                        right: page + 1,
                    })
                    .collect(),
                ..plain(Browser::Firefox, 0, 203)
            },
        ),
        (
            "listed-twice",
            Plan {
                edits: vec![Edit::Repeat {
                    page: 2,
                    from: 0,
                    to: 1,
                }],
                ..plain(Browser::Firefox, 3, 30)
            },
        ),
        (
            "statement",
            Plan {
                sql: ")(".to_owned(),
                columns: 3,
                ..plain(Browser::Raw, 0, 5)
            },
        ),
        (
            "row-header",
            Plan {
                edits: vec![Edit::Header(0)],
                ..plain(Browser::Firefox, 3, 3)
            },
        ),
        (
            "overflow-loop",
            Plan {
                long_every: 1,
                long: 2_000,
                wal: 1,
                edits: vec![Edit::Claim(u32::MAX), Edit::Loop(0)],
                ..plain(Browser::Firefox, 1, 20)
            },
        ),
    ]
}

fn seeds_folder() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fuzz/cookie-databases")
}

/// the rows the reader gives for what `plan` builds.
fn read(plan: &Plan) -> Result<Vec<Vec<Value>>, super::super::Broken> {
    let built = build(plan);
    let mut database = Database::parse(built.file, built.log.as_deref())?;
    database.budget = BUDGET;
    database.rows(built.table, &built.wanted)
}

/// the seeds in tests/fixtures/fuzz/cookie-databases are the plans [`seeds`] makes, and each
/// reaches what it was made for: the reader refuses a page reached twice, a cell listed twice
/// and an overflow page that leads back to itself, reads a table whose statement is broken
/// without a panic, and reads what was left unbent as it was built.
#[test]
fn the_seeds_reach_what_they_were_made_for() {
    for (name, plan) in seeds() {
        let bytes = std::fs::read(seeds_folder().join(name)).unwrap_or_default();
        assert_eq!(
            bytes,
            plan.bytes(),
            "{name}: run write_the_seeds once a plan's bytes change"
        );
        assert_eq!(Plan::read(&bytes), plan, "{name}");
        exercise(&bytes);
        let read = read(&plan);
        match name {
            "cycle" | "shared" | "listed-twice" | "overflow-loop" => assert!(
                read.as_ref()
                    .is_err_and(|super::super::Broken(why)| why.contains("twice")),
                "{name}: {:?}",
                read.map(|rows| rows.len())
            ),
            "statement" => assert!(
                read.as_ref().is_ok_and(|rows| rows.len() == 5
                    && rows.iter().flatten().all(|value| *value == Value::Null)),
                "{name}: {read:?}"
            ),
            "row-header" => assert!(read.is_err(), "{name}"),
            _ => assert_eq!(read.ok(), build(&plan).expected, "{name}"),
        }
    }
}

/// a log cut to nothing, then changed, is no log: the plan builds without a panic of its own.
#[test]
fn a_log_cut_to_nothing_takes_later_edits() {
    let plan = Plan {
        wal: 1,
        edits: vec![
            Edit::Cut {
                log: true,
                length: 0,
            },
            Edit::Byte {
                log: true,
                at: 5,
                value: 1,
            },
        ],
        ..plain(Browser::Firefox, 0, 20)
    };
    assert_eq!(build(&plan).log, Some(Vec::new()));
    exercise(&plan.bytes());
}

/// writes the seeds [`seeds`] plans where the weekly fuzz pass finds them.
#[test]
#[ignore = "writes tests/fixtures/fuzz/cookie-databases"]
fn write_the_seeds() {
    let folder = seeds_folder();
    std::fs::create_dir_all(&folder).unwrap();
    for (name, plan) in seeds() {
        std::fs::write(folder.join(name), plan.bytes()).unwrap();
    }
}

/// a value as SQLite gives it back.
fn sqlite_value(value: rusqlite::types::ValueRef<'_>) -> Value {
    match value {
        rusqlite::types::ValueRef::Null => Value::Null,
        rusqlite::types::ValueRef::Integer(integer) => Value::Integer(integer),
        rusqlite::types::ValueRef::Real(real) => Value::Real(real),
        rusqlite::types::ValueRef::Text(text) => {
            Value::Text(String::from_utf8_lossy(text).into_owned())
        }
        rusqlite::types::ValueRef::Blob(blob) => Value::Blob(blob.to_vec()),
    }
}

/// what the builder writes is a database SQLite itself opens, finds whole by its own check, and
/// reads the rows of that the reader is to give, its log's last commit included and what follows
/// it left out: so the fuzz target bends real databases, not ones only this reader takes.
#[test]
fn built_databases_read_as_sqlite_reads_them() {
    let plans = [
        Plan {
            long_every: 9,
            long: 1_000,
            stale: 2,
            ..plain(Browser::Firefox, 3, 246)
        },
        Plan {
            reserved: 240,
            long_every: 5,
            long: 300,
            index: true,
            stale: 1,
            wal: 3,
            big: true,
            change: 0x2b,
            ..plain(Browser::Chromium, 0, 238)
        },
        Plan {
            long_every: 3,
            long: 30_000,
            wal: 2,
            change: 0x0f,
            ..plain(Browser::Firefox, 7, 228)
        },
        Plan {
            reserved: 255,
            stale: 3,
            wal: 1,
            ..plain(Browser::Chromium, 1, 252)
        },
        plain(Browser::Firefox, 0, 0),
    ];
    for plan in plans {
        let built = build(&plan);
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("cookies.sqlite");
        std::fs::write(&path, &built.file).unwrap();
        if let Some(log) = &built.log {
            std::fs::write(folder.path().join("cookies.sqlite-wal"), log).unwrap();
        }
        let db = rusqlite::Connection::open(&path).unwrap();
        let check: String = db
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(check, "ok", "{plan:?}");
        let kept: Vec<usize> = (0..built.wanted.len())
            .filter(|at| built.wanted[*at] != "secure")
            .collect();
        let columns: Vec<&str> = kept.iter().map(|at| built.wanted[*at]).collect();
        let sql = format!(
            "SELECT {} FROM {} ORDER BY rowid",
            columns.join(", "),
            built.table
        );
        let mut statement = db.prepare(&sql).unwrap();
        let read: Vec<Vec<Value>> = statement
            .query_map([], |row| {
                Ok((0..kept.len())
                    .map(|column| sqlite_value(row.get_ref(column).unwrap()))
                    .collect())
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let expected: Vec<Vec<Value>> = built
            .expected
            .unwrap()
            .iter()
            .map(|row| kept.iter().map(|at| row[*at].clone()).collect())
            .collect();
        assert_eq!(read.len(), expected.len(), "{plan:?}");
        assert!(read == expected, "{plan:?}");
        exercise(&plan.bytes());
    }
}

use std::borrow::Cow;
use std::fmt::Display;
use std::str::FromStr;

use toml_parser::decoder::{Encoding, IntegerRadix, ScalarKind};
use toml_parser::parser::{EventReceiver, parse_document};
use toml_parser::{ErrorSink, ParseError, Raw, Source, Span};

use super::{LockError, LockedDataset, LockedFile, LockedStep, Role};
use crate::digest::{Algorithm, DigestError};
use crate::hex;
use crate::path::DataPath;
use crate::toml_error::unknown_key;

const ROOT_KEYS: &[&str] = &["version"];
const TABLES: &[&str] = &["dataset", "step"];
const DATASET_KEYS: &[&str] = &[
    "name",
    "ref",
    "spec",
    "resolved",
    "title",
    "license",
    "doi",
    "retrieved",
    "tree",
    "files",
];
const STEP_KEYS: &[&str] = &["name", "key", "items", "tree"];
const FILE_KEYS: &[&str] = &[
    "path", "size", "blake3", "sha256", "sha1", "md5", "at", "from", "role",
];

/// Reads `data.lock` text into datasets and steps in the order written, checking every key and
/// value but none of the rules across entries.
pub(super) fn read(text: &str) -> Result<(Vec<LockedDataset>, Vec<LockedStep>), LockError> {
    let source = Source::new(text);
    let tokens = source.lex().into_vec();
    let mut reader = Reader::new(source);
    let mut syntax: Option<ParseError> = None;
    parse_document(&tokens, &mut reader, &mut syntax);
    if let Some(err) = syntax {
        let span = err.unexpected().or_else(|| err.context());
        return Err(LockError {
            message: err.description().to_owned(),
            span: span.map(|span| span.start()..span.end()),
            help: None,
        });
    }
    reader.finish()
}

#[derive(Default)]
struct DatasetFields {
    header: Span,
    name: Option<Spanned>,
    reference: Option<Spanned>,
    spec: Option<Spanned>,
    resolved: Option<Spanned>,
    title: Option<Spanned>,
    license: Option<Spanned>,
    doi: Option<Spanned>,
    retrieved: Option<Spanned>,
    tree: Option<Spanned>,
    files: Option<Vec<LockedFile>>,
}

#[derive(Default)]
struct StepFields {
    header: Span,
    name: Option<Spanned>,
    key: Option<Spanned>,
    items: Option<u64>,
    tree: Option<Spanned>,
}

#[derive(Default)]
struct FileFields {
    open: Span,
    path: Option<Spanned>,
    size: Option<u64>,
    blake3: Option<[u8; 32]>,
    sha256: Option<[u8; 32]>,
    sha1: Option<[u8; 20]>,
    md5: Option<[u8; 16]>,
    at: Option<Vec<String>>,
    from: Option<Spanned>,
    role: Option<Role>,
}

enum Table {
    Root,
    Dataset(Box<DatasetFields>),
    Step(StepFields),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Table,
    Header,
    Files,
    File,
    At,
}

struct Reader<'s> {
    source: Source<'s>,
    version: Option<Span>,
    table: Table,
    place: Place,
    key: Option<(Cow<'s, str>, Span)>,
    file: FileFields,
    at: Vec<String>,
    datasets: Vec<LockedDataset>,
    steps: Vec<LockedStep>,
    error: Option<LockError>,
}

type Fallible<T> = Result<T, LockError>;

type Spanned = (String, Span);

fn at(span: Span, message: String) -> LockError {
    LockError {
        message,
        span: Some(span.start()..span.end()),
        help: None,
    }
}

impl<'s> Reader<'s> {
    fn new(source: Source<'s>) -> Self {
        Self {
            source,
            version: None,
            table: Table::Root,
            place: Place::Table,
            key: None,
            file: FileFields::default(),
            at: Vec::new(),
            datasets: Vec::new(),
            steps: Vec::new(),
            error: None,
        }
    }

    fn raw(&self, span: Span, encoding: Option<Encoding>) -> Raw<'s> {
        let text = self
            .source
            .input()
            .get(span.start()..span.end())
            .unwrap_or("");
        Raw::new_unchecked(text, encoding, span)
    }

    fn fail(&mut self, err: LockError) {
        if self.error.is_none() {
            self.error = Some(err);
        }
    }

    fn run(&mut self, step: impl FnOnce(&mut Self) -> Fallible<()>) {
        if self.error.is_none()
            && let Err(err) = step(self)
        {
            self.fail(err);
        }
    }

    fn take_key(&mut self, span: Span) -> Fallible<(Cow<'s, str>, Span)> {
        self.key
            .take()
            .ok_or_else(|| at(span, "a value needs a key".to_owned()))
    }

    fn close_table(&mut self) -> Fallible<()> {
        match std::mem::replace(&mut self.table, Table::Root) {
            Table::Root => Ok(()),
            Table::Dataset(fields) => {
                self.datasets.push(dataset(*fields)?);
                Ok(())
            }
            Table::Step(fields) => {
                self.steps.push(step(fields)?);
                Ok(())
            }
        }
    }

    fn finish(mut self) -> Fallible<(Vec<LockedDataset>, Vec<LockedStep>)> {
        if let Some(err) = self.error.take() {
            return Err(err);
        }
        self.close_table()?;
        if self.version.is_none() {
            return Err(LockError::plain("missing key `version`".to_owned()));
        }
        Ok((self.datasets, self.steps))
    }

    fn open_header(&mut self, name: &str, span: Span) -> Fallible<()> {
        self.close_table()?;
        self.table = match name {
            "dataset" => Table::Dataset(Box::new(DatasetFields {
                header: span,
                ..DatasetFields::default()
            })),
            "step" => Table::Step(StepFields {
                header: span,
                ..StepFields::default()
            }),
            _ => {
                return Err(LockError {
                    message: format!("unknown table `[[{name}]]`"),
                    span: Some(span.start()..span.end()),
                    help: Some(unknown_key(name, TABLES).1),
                });
            }
        };
        Ok(())
    }

    fn key_in(&mut self, span: Span, encoding: Option<Encoding>) -> Fallible<()> {
        let mut name = Cow::Borrowed("");
        let mut sink: Option<ParseError> = None;
        self.raw(span, encoding).decode_key(&mut name, &mut sink);
        if let Some(err) = sink {
            return Err(at(span, err.description().to_owned()));
        }
        if self.place == Place::Header {
            return self.open_header(&name, span);
        }
        let valid = match (&self.table, self.place) {
            (_, Place::File) => FILE_KEYS,
            (Table::Root, _) => ROOT_KEYS,
            (Table::Dataset(_), _) => DATASET_KEYS,
            (Table::Step(_), _) => STEP_KEYS,
        };
        if !valid.contains(&name.as_ref()) {
            let (message, help) = unknown_key(&name, valid);
            return Err(LockError {
                message,
                span: Some(span.start()..span.end()),
                help: Some(help),
            });
        }
        self.key = Some((name, span));
        Ok(())
    }

    fn scalar_in(&mut self, span: Span, encoding: Option<Encoding>) -> Fallible<()> {
        let raw = self.raw(span, encoding);
        let mut value = Cow::Borrowed("");
        let mut sink: Option<ParseError> = None;
        let kind = raw.decode_scalar(&mut value, &mut sink);
        if let Some(err) = sink {
            return Err(at(span, err.description().to_owned()));
        }
        let scalar = Scalar {
            kind,
            value,
            raw: raw.as_str(),
            span,
        };
        if self.place == Place::At {
            self.at.push(scalar.string("at")?);
            return Ok(());
        }
        if self.place == Place::Files {
            return Err(at(
                span,
                "each entry of `files` must be an inline table".to_owned(),
            ));
        }
        let (key, key_span) = self.take_key(span)?;
        let duplicate = || at(key_span, format!("key `{key}` appears twice"));
        if self.place == Place::File {
            return self.file_value(&key, scalar).map_err(|err| match err {
                Assign::Twice => duplicate(),
                Assign::Invalid(err) => err,
            });
        }
        let assigned = match &mut self.table {
            Table::Root => {
                if self.version.is_some() {
                    Err(Assign::Twice)
                } else {
                    let version = scalar.integer(&key)?;
                    if version != 1 {
                        return Err(at(
                            span,
                            format!(
                                "data.lock version {version} is not supported, this fetchloom reads version 1"
                            ),
                        ));
                    }
                    self.version = Some(span);
                    Ok(())
                }
            }
            Table::Dataset(fields) => {
                let slot = match key.as_ref() {
                    "name" => &mut fields.name,
                    "ref" => &mut fields.reference,
                    "spec" => &mut fields.spec,
                    "resolved" => &mut fields.resolved,
                    "title" => &mut fields.title,
                    "license" => &mut fields.license,
                    "doi" => &mut fields.doi,
                    "retrieved" => &mut fields.retrieved,
                    "tree" => &mut fields.tree,
                    _ => {
                        return Err(at(
                            span,
                            "`files` must be an array of inline tables".to_owned(),
                        ));
                    }
                };
                let span = scalar.span;
                set(slot, (scalar.string(&key)?, span))
            }
            Table::Step(fields) => match key.as_ref() {
                "items" => set(&mut fields.items, scalar.integer(&key)?),
                "name" => set(&mut fields.name, scalar.spanned(&key)?),
                "key" => set(&mut fields.key, scalar.spanned(&key)?),
                _ => set(&mut fields.tree, scalar.spanned(&key)?),
            },
        };
        assigned.map_err(|_| duplicate())
    }

    fn file_value(&mut self, key: &str, scalar: Scalar<'s>) -> Result<(), Assign> {
        let file = &mut self.file;
        match key {
            "path" => set(&mut file.path, scalar.spanned(key)?),
            "size" => set(&mut file.size, scalar.integer(key)?),
            "blake3" => set(&mut file.blake3, scalar.hex(Algorithm::Blake3)?),
            "sha256" => set(&mut file.sha256, scalar.hex(Algorithm::Sha256)?),
            "sha1" => set(&mut file.sha1, scalar.hex(Algorithm::Sha1)?),
            "md5" => set(&mut file.md5, scalar.hex(Algorithm::Md5)?),
            "from" => set(&mut file.from, scalar.spanned(key)?),
            "role" => match scalar.spanned(key)? {
                (role, _) if role == "archive" => set(&mut file.role, Role::Archive),
                (other, span) => Err(Assign::Invalid(at(
                    span,
                    format!("role `{other}` is not known, expected `archive`"),
                ))),
            },
            _ => Err(Assign::Invalid(at(
                scalar.span,
                "`at` must be an array of strings".to_owned(),
            ))),
        }
    }

    fn array_in(&mut self, span: Span) -> Fallible<()> {
        let (key, key_span) = self.take_key(span)?;
        match (&mut self.table, self.place, key.as_ref()) {
            (Table::Dataset(fields), Place::Table, "files") => {
                if fields.files.is_some() {
                    return Err(at(key_span, "key `files` appears twice".to_owned()));
                }
                fields.files = Some(Vec::new());
                self.place = Place::Files;
                Ok(())
            }
            (_, Place::File, "at") => {
                if self.file.at.is_some() {
                    return Err(at(key_span, "key `at` appears twice".to_owned()));
                }
                self.place = Place::At;
                Ok(())
            }
            _ => Err(at(span, format!("`{key}` does not take an array"))),
        }
    }

    fn array_out(&mut self) {
        match self.place {
            Place::At => {
                self.file.at = Some(std::mem::take(&mut self.at));
                self.place = Place::File;
            }
            _ => self.place = Place::Table,
        }
    }

    fn inline_in(&mut self, span: Span) -> Fallible<()> {
        if self.place != Place::Files {
            let what = self
                .key
                .take()
                .map_or_else(|| "this value".to_owned(), |(key, _)| format!("`{key}`"));
            return Err(at(span, format!("{what} does not take an inline table")));
        }
        self.file = FileFields {
            open: span,
            ..FileFields::default()
        };
        self.place = Place::File;
        Ok(())
    }

    fn inline_out(&mut self) -> Fallible<()> {
        let fields = std::mem::take(&mut self.file);
        let locked = file(fields)?;
        if let Table::Dataset(fields) = &mut self.table
            && let Some(files) = &mut fields.files
        {
            files.push(locked);
        }
        self.place = Place::Files;
        Ok(())
    }
}

enum Assign {
    Twice,
    Invalid(LockError),
}

impl From<LockError> for Assign {
    fn from(err: LockError) -> Self {
        Self::Invalid(err)
    }
}

fn set<T>(slot: &mut Option<T>, value: T) -> Result<(), Assign> {
    if slot.is_some() {
        return Err(Assign::Twice);
    }
    *slot = Some(value);
    Ok(())
}

struct Scalar<'s> {
    kind: ScalarKind,
    value: Cow<'s, str>,
    raw: &'s str,
    span: Span,
}

impl Scalar<'_> {
    fn spanned(self, key: &str) -> Fallible<Spanned> {
        let span = self.span;
        Ok((self.string(key)?, span))
    }

    fn string(self, key: &str) -> Fallible<String> {
        match self.kind {
            ScalarKind::String => Ok(self.value.into_owned()),
            _ => Err(at(
                self.span,
                format!("`{key}` must be a string, got `{}`", self.raw),
            )),
        }
    }

    fn integer(self, key: &str) -> Fallible<u64> {
        match (self.kind, self.value.parse()) {
            (ScalarKind::Integer(IntegerRadix::Dec), Ok(number)) => Ok(number),
            _ => Err(at(
                self.span,
                format!("`{key}` must be a whole number, got `{}`", self.raw),
            )),
        }
    }

    fn hex<const N: usize>(self, algorithm: Algorithm) -> Fallible<[u8; N]> {
        let decoded = match self.kind {
            ScalarKind::String => hex::decode(&self.value),
            _ => None,
        };
        decoded.ok_or_else(|| {
            at(
                self.span,
                DigestError::Hex {
                    algorithm,
                    length: 2 * N,
                    text: self.value.to_string(),
                }
                .to_string(),
            )
        })
    }
}

fn parsed<T: FromStr>((text, span): Spanned) -> Fallible<T>
where
    T::Err: Display,
{
    text.parse()
        .map_err(|err: T::Err| at(span, err.to_string()))
}

fn path((text, span): Spanned) -> Fallible<DataPath> {
    DataPath::try_from(text).map_err(|err| at(span, err.to_string()))
}

fn required<T>(slot: Option<T>, key: &str, table: &str, span: Span) -> Fallible<T> {
    slot.ok_or_else(|| at(span, format!("missing key `{key}` in a `{table}`")))
}

fn dataset(fields: DatasetFields) -> Fallible<LockedDataset> {
    let span = fields.header;
    let text = |slot: Option<Spanned>, key: &str| required(slot, key, "[[dataset]]", span);
    let plain = |slot: Option<Spanned>| slot.map(|(text, _)| text);
    Ok(LockedDataset {
        name: parsed(text(fields.name, "name")?)?,
        reference: parsed(text(fields.reference, "ref")?)?,
        spec: parsed(text(fields.spec, "spec")?)?,
        resolved: parsed(text(fields.resolved, "resolved")?)?,
        title: plain(fields.title),
        license: plain(fields.license),
        doi: plain(fields.doi),
        retrieved: parsed(text(fields.retrieved, "retrieved")?)?,
        tree: parsed(text(fields.tree, "tree")?)?,
        files: fields.files,
    })
}

fn step(fields: StepFields) -> Fallible<LockedStep> {
    let span = fields.header;
    let text = |slot: Option<Spanned>, key: &str| required(slot, key, "[[step]]", span);
    Ok(LockedStep {
        name: parsed(text(fields.name, "name")?)?,
        key: parsed(text(fields.key, "key")?)?,
        items: fields.items,
        tree: parsed(text(fields.tree, "tree")?)?,
    })
}

fn file(fields: FileFields) -> Fallible<LockedFile> {
    let span = fields.open;
    Ok(LockedFile {
        path: path(required(fields.path, "path", "file", span)?)?,
        size: required(fields.size, "size", "file", span)?,
        blake3: required(fields.blake3, "blake3", "file", span)?,
        sha256: fields.sha256,
        sha1: fields.sha1,
        md5: fields.md5,
        at: fields.at.unwrap_or_default(),
        from: fields.from.map(parsed).transpose()?,
        role: fields.role,
    })
}

impl EventReceiver for Reader<'_> {
    fn std_table_open(&mut self, span: Span, _error: &mut dyn ErrorSink) {
        self.run(|_| {
            Err(at(
                span,
                "data.lock holds only `[[dataset]]` and `[[step]]` tables".to_owned(),
            ))
        });
    }

    fn array_table_open(&mut self, _span: Span, _error: &mut dyn ErrorSink) {
        self.place = Place::Header;
    }

    fn array_table_close(&mut self, _span: Span, _error: &mut dyn ErrorSink) {
        self.place = Place::Table;
    }

    fn inline_table_open(&mut self, span: Span, _error: &mut dyn ErrorSink) -> bool {
        self.run(|reader| reader.inline_in(span));
        self.error.is_none()
    }

    fn inline_table_close(&mut self, _span: Span, _error: &mut dyn ErrorSink) {
        self.run(Reader::inline_out);
    }

    fn array_open(&mut self, span: Span, _error: &mut dyn ErrorSink) -> bool {
        self.run(|reader| reader.array_in(span));
        self.error.is_none()
    }

    fn array_close(&mut self, _span: Span, _error: &mut dyn ErrorSink) {
        self.array_out();
    }

    fn simple_key(&mut self, span: Span, kind: Option<Encoding>, _error: &mut dyn ErrorSink) {
        self.run(|reader| reader.key_in(span, kind));
    }

    fn key_sep(&mut self, span: Span, _error: &mut dyn ErrorSink) {
        self.run(|_| Err(at(span, "data.lock uses no dotted keys".to_owned())));
    }

    fn scalar(&mut self, span: Span, kind: Option<Encoding>, _error: &mut dyn ErrorSink) {
        self.run(|reader| reader.scalar_in(span, kind));
    }
}

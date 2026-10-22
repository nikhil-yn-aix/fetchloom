//! Patterns that take a reference body apart, and templates that put URLs, references and
//! paths together from those parts and from values read out of JSON.

use std::collections::BTreeMap;

use serde_json::Value;
use serde_json_path::JsonPath;

/// The parts a pattern took out of a reference body, by name.
pub(crate) type Parts = BTreeMap<String, String>;

/// A `JSONPath` with the text it was written as.
#[derive(Debug)]
pub(crate) struct Query {
    pub(crate) text: String,
    path: JsonPath,
}

impl Query {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        JsonPath::parse(text)
            .map(|path| Self {
                text: text.to_owned(),
                path,
            })
            .map_err(|error| format!("`{text}` is not a JSONPath: {error}"))
    }

    pub(crate) fn all<'v>(&self, value: &'v Value) -> Vec<&'v Value> {
        self.path.query(value).all()
    }

    pub(crate) fn optional<'v>(&self, value: &'v Value) -> Result<Option<&'v Value>, &'static str> {
        self.path
            .query(value)
            .at_most_one()
            .map_err(|_| "more than one value")
    }
}

/// How a part of a pattern may be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Word,
    Rest,
    Host,
}

#[derive(Debug)]
enum Piece {
    Literal(String),
    Part(String, Kind),
}

/// A pattern such as `{host}/{id}` or `{owner}/{repo}@{tag}`. `{host}` takes a host name with
/// an optional port, `{name...}` may hold slashes, and any other `{name}` takes letters, digits
/// and `.`, `_`, `-`, `~` and `:`.
#[derive(Debug)]
pub(crate) struct Pattern {
    text: String,
    pieces: Vec<Piece>,
}

impl Pattern {
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut pieces = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            let end = rest[start..]
                .find('}')
                .map(|end| start + end)
                .ok_or_else(|| format!("the pattern `{text}` has a `{{` that is never closed"))?;
            if start > 0 {
                pieces.push(Piece::Literal(rest[..start].to_owned()));
            }
            let inner = &rest[start + 1..end];
            let (name, kind) = match inner.strip_suffix("...") {
                Some(name) => (name, Kind::Rest),
                None if inner == "host" => (inner, Kind::Host),
                None => (inner, Kind::Word),
            };
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                return Err(format!(
                    "the pattern `{text}` names a part `{inner}` that is not lowercase letters"
                ));
            }
            if matches!(pieces.last(), Some(Piece::Part(..))) {
                return Err(format!(
                    "the pattern `{text}` has two parts with nothing between them"
                ));
            }
            pieces.push(Piece::Part(name.to_owned(), kind));
            rest = &rest[end + 1..];
        }
        if !rest.is_empty() {
            pieces.push(Piece::Literal(rest.to_owned()));
        }
        Ok(Self {
            text: text.to_owned(),
            pieces,
        })
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.pieces.iter().filter_map(|piece| match piece {
            Piece::Part(name, _) => Some(name.as_str()),
            Piece::Literal(_) => None,
        })
    }

    pub(crate) fn has_host(&self) -> bool {
        self.pieces
            .iter()
            .any(|piece| matches!(piece, Piece::Part(_, Kind::Host)))
    }

    /// The parts of `body`, when the whole of it matches.
    pub(crate) fn matches(&self, body: &str) -> Option<Parts> {
        let mut parts = Parts::new();
        let mut rest = body;
        let mut pieces = self.pieces.iter().peekable();
        while let Some(piece) = pieces.next() {
            match piece {
                Piece::Literal(literal) => rest = rest.strip_prefix(literal.as_str())?,
                Piece::Part(name, kind) => {
                    let end = match pieces.peek() {
                        Some(Piece::Literal(next)) => rest.find(next.as_str())?,
                        _ => rest.len(),
                    };
                    let value = &rest[..end];
                    if !valid(value, *kind) {
                        return None;
                    }
                    let value = if *kind == Kind::Host {
                        value.to_ascii_lowercase()
                    } else {
                        value.to_owned()
                    };
                    parts.insert(name.clone(), value);
                    rest = &rest[end..];
                }
            }
        }
        rest.is_empty().then_some(parts)
    }

    /// The body this pattern matches for `parts`.
    pub(crate) fn render(&self, parts: &Parts) -> Option<String> {
        let mut text = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Literal(literal) => text.push_str(literal),
                Piece::Part(name, _) => text.push_str(parts.get(name)?),
            }
        }
        Some(text)
    }
}

/// Whether `value` may stand as a part of a reference: letters, digits and `. _ - ~ :`, not
/// `.` or `..`.
pub(crate) fn word(value: &str) -> bool {
    valid(value, Kind::Word)
}

fn valid(value: &str, kind: Kind) -> bool {
    let word = |segment: &str| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~' | b':'))
    };
    match kind {
        Kind::Word => word(value),
        Kind::Rest => value.split('/').all(word),
        Kind::Host => {
            let (name, port) = value.split_once(':').unwrap_or((value, "1"));
            !name.is_empty()
                && !name.starts_with(['.', '-'])
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
                && port.parse::<u16>().is_ok_and(|port| port > 0)
        }
    }
}

#[derive(Debug)]
enum Segment {
    Literal(String),
    Part(String, bool),
    Value(Query, bool),
}

/// Where a template's text goes, which decides how inserted values are escaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Context {
    /// A URL: a value that is the whole template is taken as it is, a value inside one is
    /// percent encoded.
    Url,
    /// Plain text, such as a reference or a path.
    Text,
}

/// Text with `{name}` parts of a pattern and `{$.path}` values of a JSON document in it. A
/// template that starts with `$` is one `JSONPath` alone. In a URL, `{%$.path}` and `{%name}`
/// escape the value as one path segment, slashes included.
#[derive(Debug)]
pub(crate) struct Template {
    text: String,
    segments: Vec<Segment>,
    context: Context,
}

/// Why a template could not be filled.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Missing {
    /// A `JSONPath` found no value, or more than one, or one that is not text or a number.
    Value(String, &'static str),
    /// A part the pattern did not take.
    Part(String),
}

impl Template {
    pub(crate) fn parse(text: &str, context: Context) -> Result<Self, String> {
        if text.starts_with('$') {
            return Ok(Self {
                text: text.to_owned(),
                segments: vec![Segment::Value(Query::parse(text)?, false)],
                context,
            });
        }
        let mut segments = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            let end = rest[start..]
                .find('}')
                .map(|end| start + end)
                .ok_or_else(|| format!("`{text}` has a `{{` that is never closed"))?;
            if start > 0 {
                segments.push(Segment::Literal(rest[..start].to_owned()));
            }
            let inner = &rest[start + 1..end];
            let (single, inner) = match inner.strip_prefix('%') {
                Some(inner) => (true, inner),
                None => (false, inner),
            };
            if inner.starts_with('$') {
                segments.push(Segment::Value(Query::parse(inner)?, single));
            } else {
                segments.push(Segment::Part(inner.to_owned(), single));
            }
            rest = &rest[end + 1..];
        }
        if !rest.is_empty() {
            segments.push(Segment::Literal(rest.to_owned()));
        }
        Ok(Self {
            text: text.to_owned(),
            segments,
            context,
        })
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The pattern parts this template names.
    pub(crate) fn parts(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().filter_map(|segment| match segment {
            Segment::Part(name, _) => Some(name.as_str()),
            _ => None,
        })
    }

    /// Whether the template reads values out of a document.
    pub(crate) fn reads(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| matches!(segment, Segment::Value(..)))
    }

    /// The text for `parts` and the document `value`.
    pub(crate) fn render(&self, parts: &Parts, value: &Value) -> Result<String, Missing> {
        let alone = self.segments.len() == 1;
        let mut text = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Literal(literal) => text.push_str(literal),
                Segment::Part(name, single) => {
                    let part = parts.get(name).ok_or_else(|| Missing::Part(name.clone()))?;
                    if self.context == Context::Url && *single {
                        escape_segment(part, &mut text);
                    } else {
                        text.push_str(part);
                    }
                }
                Segment::Value(query, single) => {
                    let found = query
                        .optional(value)
                        .map_err(|problem| Missing::Value(query.text.clone(), problem))?
                        .ok_or_else(|| Missing::Value(query.text.clone(), "no value"))?;
                    let plain = plain(found).ok_or_else(|| {
                        Missing::Value(query.text.clone(), "a value that is not text or a number")
                    })?;
                    if self.context == Context::Url && *single {
                        escape_segment(&plain, &mut text);
                    } else if self.context == Context::Url && !alone {
                        escape(&plain, &mut text);
                    } else {
                        text.push_str(&plain);
                    }
                }
            }
        }
        Ok(text)
    }
}

/// A JSON string or number as text.
pub(crate) fn plain(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Appends `text` to `out` with every byte but the unreserved ones and `/`, `:` and `@`
/// percent encoded.
pub(crate) fn escape(text: &str, out: &mut String) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':' | b'@')
        {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 15)]));
        }
    }
}

/// Appends `text` to `out` as one path segment: like [`escape`], with `/` encoded too.
pub(crate) fn escape_segment(text: &str, out: &mut String) {
    for piece in text.split('/').enumerate() {
        if piece.0 > 0 {
            out.push_str("%2F");
        }
        escape(piece.1, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(pairs: &[(&str, &str)]) -> Parts {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn patterns_take_bodies_apart_and_put_them_back() {
        let pattern = Pattern::parse("{owner}/{repo}@{tag}").unwrap();
        let found = pattern.matches("lab/tool@v1.2").unwrap();
        assert_eq!(
            found,
            parts(&[("owner", "lab"), ("repo", "tool"), ("tag", "v1.2")])
        );
        assert_eq!(pattern.render(&found).as_deref(), Some("lab/tool@v1.2"));
        assert_eq!(pattern.matches("lab/tool"), None);
        assert_eq!(pattern.matches("lab/to@ol@v1"), None);
    }

    #[test]
    fn hosts_and_rests_take_what_words_refuse() {
        let pattern = Pattern::parse("{host}/{pid...}").unwrap();
        assert_eq!(
            pattern
                .matches("Data.Lab.org:8443/doi:10.7910/DVN/X")
                .unwrap(),
            parts(&[("host", "data.lab.org:8443"), ("pid", "doi:10.7910/DVN/X")])
        );
        for body in [
            "-x.org/a",
            "x.org:0/a",
            "x.org/a/../b",
            "x.org/a//b",
            "x.org/a?b",
        ] {
            assert_eq!(pattern.matches(body), None, "{body}");
        }
        let word = Pattern::parse("{id}").unwrap();
        for body in ["", "a/b", "..", "a b", "a%2F", "a#b"] {
            assert_eq!(word.matches(body), None, "{body}");
        }
    }

    #[test]
    fn patterns_without_separators_or_names_are_refused() {
        assert!(Pattern::parse("{a}{b}").is_err());
        assert!(Pattern::parse("{Id}").is_err());
        assert!(Pattern::parse("{id").is_err());
    }

    #[test]
    fn templates_escape_values_inside_urls_and_keep_whole_ones() {
        let value = serde_json::json!({"name": "a b&c.csv", "next": "https://x.org/p?page=2"});
        let inside = Template::parse("https://x.org/{id}/{$.name}", Context::Url).unwrap();
        assert_eq!(
            inside.render(&parts(&[("id", "7")]), &value).unwrap(),
            "https://x.org/7/a%20b%26c.csv"
        );
        let segment = Template::parse("https://x.org/f/{%$.path}", Context::Url).unwrap();
        let nested = serde_json::json!({"path": "a b/c.csv"});
        assert_eq!(
            segment.render(&Parts::new(), &nested).unwrap(),
            "https://x.org/f/a%20b%2Fc.csv"
        );
        let part = Template::parse("https://x.org/d/{%doi}", Context::Url).unwrap();
        assert_eq!(
            part.render(&parts(&[("doi", "doi:10.5/x")]), &nested)
                .unwrap(),
            "https://x.org/d/doi:10.5%2Fx"
        );
        let whole = Template::parse("{$.next}", Context::Url).unwrap();
        assert_eq!(
            whole.render(&Parts::new(), &value).unwrap(),
            "https://x.org/p?page=2"
        );
        let text = Template::parse("$.name", Context::Text).unwrap();
        assert_eq!(text.render(&Parts::new(), &value).unwrap(), "a b&c.csv");
        assert_eq!(
            inside.render(&Parts::new(), &value),
            Err(Missing::Part("id".to_owned()))
        );
        let missing = Template::parse("{$.size}", Context::Text).unwrap();
        assert_eq!(
            missing.render(&Parts::new(), &value),
            Err(Missing::Value("$.size".to_owned(), "no value"))
        );
    }
}

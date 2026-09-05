//! The one model every accepted surface syntax parses into, and the canonical
//! forms written back out.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Map, Value};

use crate::error::{Error, ErrorKind};
use crate::limits::Limits;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bound {
    Foreign,
    Own,
}

impl Bound {
    #[must_use]
    fn of(self, limits: &Limits) -> (u64, u64) {
        match self {
            Self::Foreign => (limits.manifest_size, limits.manifest_nodes),
            Self::Own => (limits.record_size, limits.record_nodes),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Syntax {
    Yaml,
    Toml,
    Json,
}

impl Syntax {
    #[must_use]
    pub fn of_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "yaml" | "yml" => Some(Self::Yaml),
            "toml" => Some(Self::Toml),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Json => "json",
        }
    }
}

/// # Errors
/// `manifest.invalid` when the document is longer than the limit allows, is
/// not UTF-8, does not parse as the syntax named, or nests past the limit.
pub fn parse(bytes: &[u8], syntax: Syntax, limits: &Limits, bound: Bound) -> Result<Value, Error> {
    let (size, nodes) = bound.of(limits);
    if bytes.len() as u64 > size {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "shorten the document, because it is {} bytes and the limit is {size}",
                bytes.len(),
            ),
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|reason| invalid(format!("write the document in UTF-8, because {reason}")))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let value = match syntax {
        Syntax::Yaml => crate::yaml::parse(text, limits)?,
        Syntax::Toml => toml::from_str::<Value>(text)
            .map_err(|reason| invalid(format!("correct the TOML, because {reason}")))?,
        Syntax::Json => serde_json::from_str::<Value>(text)
            .map_err(|reason| invalid(format!("correct the JSON, because {reason}")))?,
    };
    check_bounds(&value, limits, nodes)?;
    Ok(value)
}

#[must_use]
pub fn canonical_json(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_json(value, &mut out);
    out
}

#[must_use]
pub fn render(value: &Value) -> String {
    let mut out = String::new();
    render_node(value, 0, &mut out);
    out
}

fn invalid(action: impl Into<String>) -> Error {
    Error::new(ErrorKind::ManifestInvalid, action)
}

fn check_bounds(value: &Value, limits: &Limits, ceiling: u64) -> Result<(), Error> {
    let mut nodes = 0u64;
    walk(value, 0, &mut nodes, limits, ceiling)
}

fn walk(
    value: &Value,
    depth: u32,
    nodes: &mut u64,
    limits: &Limits,
    ceiling: u64,
) -> Result<(), Error> {
    *nodes += 1;
    if *nodes > ceiling {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!("shorten the document, because it holds more than {ceiling} nodes"),
        ));
    }
    if depth > limits.nesting_depth {
        return Err(invalid(format!(
            "flatten the document, because it nests deeper than {}",
            limits.nesting_depth
        )));
    }
    match value {
        Value::Array(items) => {
            for item in items {
                walk(item, depth + 1, nodes, limits, ceiling)?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for field in fields.values() {
                walk(field, depth + 1, nodes, limits, ceiling)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn write_json(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(number) => out.extend_from_slice(number.to_string().as_bytes()),
        Value::String(text) => out.extend_from_slice(quote(text).as_bytes()),
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_json(item, out);
            }
            out.push(b']');
        }
        Value::Object(fields) => {
            out.push(b'{');
            for (index, (key, field)) in ordered(fields).into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(quote(key).as_bytes());
                out.push(b':');
                write_json(field, out);
            }
            out.push(b'}');
        }
    }
}

fn ordered(fields: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut entries: Vec<(&String, &Value)> = fields.iter().collect();
    entries.sort_by(|left, right| crate::canonical::compare(left.0.as_bytes(), right.0.as_bytes()));
    entries
}

#[must_use]
pub(crate) fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if (other as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", other as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn render_node(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            for (key, field) in ordered(fields) {
                push_indent(indent, out);
                out.push_str(&render_key(key));
                out.push(':');
                render_child(field, indent, out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for item in items {
                push_indent(indent, out);
                out.push('-');
                render_child(item, indent, out);
            }
        }
        scalar => {
            push_indent(indent, out);
            out.push_str(&render_scalar(scalar));
            out.push('\n');
        }
    }
}

fn render_child(value: &Value, indent: usize, out: &mut String) {
    match value {
        Value::Object(fields) if fields.is_empty() => out.push_str(" {}\n"),
        Value::Array(items) if items.is_empty() => out.push_str(" []\n"),
        Value::Object(_) | Value::Array(_) => {
            out.push('\n');
            render_node(value, indent + 1, out);
        }
        scalar => {
            out.push(' ');
            out.push_str(&render_scalar(scalar));
            out.push('\n');
        }
    }
}

fn push_indent(indent: usize, out: &mut String) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn render_scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(true) => "true".to_owned(),
        Value::Bool(false) => "false".to_owned(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => quote(text),
        Value::Array(_) | Value::Object(_) => String::new(),
    }
}

fn render_key(key: &str) -> String {
    let plain = !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        && !key.as_bytes()[0].is_ascii_digit();
    if plain { key.to_owned() } else { quote(key) }
}

pub(crate) fn read_model<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    called: &str,
    limits: &Limits,
    bound: Bound,
) -> Result<T, Error> {
    read_model_in(bytes, Syntax::Yaml, called, limits, bound)
}

pub(crate) fn read_model_in<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    syntax: Syntax,
    called: &str,
    limits: &Limits,
    bound: Bound,
) -> Result<T, Error> {
    from_document(parse(bytes, syntax, limits, bound)?, called)
}

fn from_document<T: serde::de::DeserializeOwned>(
    document: Value,
    called: &str,
) -> Result<T, Error> {
    refuse_reserved_keys(&document, called)?;
    serde_json::from_value(document)
        .map_err(|reason| invalid(format!("correct the {called}, because {reason}")))
}

fn refuse_reserved_keys(document: &Value, called: &str) -> Result<(), Error> {
    match document {
        Value::Object(fields) => {
            for (key, field) in fields {
                if key.starts_with("x-") {
                    return Err(invalid(format!(
                        "remove {key} from the {called}, because the x- prefix is reserved and \
                         nothing reads it yet"
                    )));
                }
                refuse_reserved_keys(field, called)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                refuse_reserved_keys(item, called)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// # Errors
/// `manifest.invalid` when the model cannot be written as a value.
pub fn canonical_json_of<T: serde::Serialize>(model: &T) -> Result<Vec<u8>, Error> {
    Ok(canonical_json(&to_value(model)?))
}

/// # Errors
/// `manifest.invalid` when the model cannot be written as a value.
pub fn render_model<T: serde::Serialize>(model: &T) -> Result<String, Error> {
    Ok(render(&to_value(model)?))
}

fn to_value<T: serde::Serialize>(model: &T) -> Result<Value, Error> {
    serde_json::to_value(model)
        .map_err(|reason| invalid(format!("the canonical form could not be written: {reason}")))
}

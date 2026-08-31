//! The one model every accepted surface syntax parses into, and the canonical
//! forms written back out.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Map, Value};

use crate::error::{Error, ErrorKind};
use crate::limits::Limits;

/// A surface syntax a document may be written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Syntax {
    /// The restricted block subset of YAML this build reads.
    Yaml,
    /// TOML.
    Toml,
    /// JSON.
    Json,
}

impl Syntax {
    /// Returns the syntax a file name states, when it states one this build
    /// reads.
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

    /// Returns the name this syntax is reported by.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Json => "json",
        }
    }
}

/// Reads a document in one of the three accepted syntaxes into the model.
///
/// # Errors
///
/// Fails with `manifest.invalid` when the bytes are not valid text, when the
/// syntax rejects them, when the document uses a construct this build refuses,
/// and when it exceeds the size, node, or depth bounds.
pub fn parse(bytes: &[u8], syntax: Syntax, limits: &Limits) -> Result<Value, Error> {
    if bytes.len() as u64 > limits.manifest_size {
        return Err(invalid(format!(
            "shorten the document, because it is {} bytes and the limit is {}",
            bytes.len(),
            limits.manifest_size
        )));
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
    check_bounds(&value, limits)?;
    Ok(value)
}

/// Returns the canonical JSON bytes of a model.
#[must_use]
pub fn canonical_json(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_json(value, &mut out);
    out
}

/// Renders a model as the canonical text this build writes a lock, a receipt,
/// and a plan in.
#[must_use]
pub fn render(value: &Value) -> String {
    let mut out = String::new();
    render_node(value, 0, &mut out);
    out
}

fn invalid(action: impl Into<String>) -> Error {
    Error::new(ErrorKind::ManifestInvalid, action)
}

fn check_bounds(value: &Value, limits: &Limits) -> Result<(), Error> {
    let mut nodes = 0u64;
    walk(value, 0, &mut nodes, limits)
}

fn walk(value: &Value, depth: u32, nodes: &mut u64, limits: &Limits) -> Result<(), Error> {
    *nodes += 1;
    if *nodes > limits.manifest_nodes {
        return Err(invalid(format!(
            "shorten the document, because it holds more than {} nodes",
            limits.manifest_nodes
        )));
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
                walk(item, depth + 1, nodes, limits)?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for field in fields.values() {
                walk(field, depth + 1, nodes, limits)?;
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

/// Returns a mapping's entries ordered by the raw bytes of their keys.
fn ordered(fields: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut entries: Vec<(&String, &Value)> = fields.iter().collect();
    entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    entries
}

/// Returns the one double-quoted form of a string.
#[must_use]
pub fn quote(text: &str) -> String {
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

/// Returns a mapping key as it is written, quoted only when it would not read
/// back as itself.
fn render_key(key: &str) -> String {
    let plain = !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        && !key.as_bytes()[0].is_ascii_digit();
    if plain { key.to_owned() } else { quote(key) }
}

/// Reads a model from a document written in the syntax this build writes.
///
/// # Errors
///
/// Fails with `manifest.invalid` naming the line or the key that broke.
pub fn read_model<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    called: &str,
    limits: &Limits,
) -> Result<T, Error> {
    read_model_in(bytes, Syntax::Yaml, called, limits)
}

/// Reads a model from a document written in one of the accepted syntaxes.
///
/// # Errors
///
/// Fails with `manifest.invalid` naming the line or the key that broke.
pub fn read_model_in<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    syntax: Syntax,
    called: &str,
    limits: &Limits,
) -> Result<T, Error> {
    from_document(parse(bytes, syntax, limits)?, called)
}

/// Reads a parsed document into a model, refusing every key the model does not
/// name.
///
/// # Errors
///
/// Fails with `manifest.invalid` naming the key or the field that broke.
pub fn from_document<T: serde::de::DeserializeOwned>(
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

/// Returns the canonical JSON bytes of a model.
///
/// # Errors
///
/// Fails when the model cannot be written.
pub fn canonical_json_of<T: serde::Serialize>(model: &T) -> Result<Vec<u8>, Error> {
    Ok(canonical_json(&to_value(model)?))
}

/// Renders a model as the canonical text this build writes an artifact in.
///
/// # Errors
///
/// Fails when the model cannot be written.
pub fn render_model<T: serde::Serialize>(model: &T) -> Result<String, Error> {
    Ok(render(&to_value(model)?))
}

fn to_value<T: serde::Serialize>(model: &T) -> Result<Value, Error> {
    serde_json::to_value(model)
        .map_err(|reason| invalid(format!("the canonical form could not be written: {reason}")))
}

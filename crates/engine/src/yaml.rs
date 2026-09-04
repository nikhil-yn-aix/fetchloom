//! The restricted block subset of YAML this build reads.

use serde_json::{Map, Number, Value};

use crate::error::{Error, ErrorKind};
use crate::limits::Limits;

struct Line {
    number: usize,
    indent: usize,
    content: String,
}

pub(crate) fn parse(text: &str, limits: &Limits) -> Result<Value, Error> {
    let lines = significant(text)?;
    if lines.is_empty() {
        return Err(invalid(
            "write at least one key, because the document is empty",
        ));
    }
    if lines[0].indent != 0 {
        return Err(at(lines[0].number, "start the first line in column one"));
    }
    let mut reader = Reader {
        lines,
        at: 0,
        depth_limit: limits.nesting_depth,
    };
    let value = reader.block(0, 0)?;
    if reader.at < reader.lines.len() {
        let line = &reader.lines[reader.at];
        return Err(at(
            line.number,
            "remove the line, because nothing contains it",
        ));
    }
    Ok(value)
}

fn invalid(action: impl Into<String>) -> Error {
    Error::new(ErrorKind::ManifestInvalid, action)
}

fn at(line: usize, action: &str) -> Error {
    invalid(format!("line {line}: {action}"))
}

fn significant(text: &str) -> Result<Vec<Line>, Error> {
    let mut lines = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let number = index + 1;
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        let body = &raw[indent..];
        if body.starts_with('\t') || raw[..indent].contains('\t') {
            return Err(at(
                number,
                "indent with spaces, because a tab is not indentation",
            ));
        }
        let content = strip_comment(body).trim_end();
        if content.is_empty() {
            continue;
        }
        if content == "---" || content == "..." {
            return Err(at(
                number,
                "write one document, because a document separator is not read here",
            ));
        }
        if content.starts_with('%') {
            return Err(at(
                number,
                "remove the directive, because none is read here",
            ));
        }
        lines.push(Line {
            number,
            indent,
            content: content.to_owned(),
        });
    }
    Ok(lines)
}

fn strip_comment(body: &str) -> &str {
    let bytes = body.as_bytes();
    let mut quote = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(b'"') if byte == b'\\' => index += 1,
            Some(open) if byte == open => quote = None,
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'#' && (index == 0 || bytes[index - 1] == b' ') => {
                return &body[..index];
            }
            Some(_) | None => {}
        }
        index += 1;
    }
    body
}

struct Reader {
    lines: Vec<Line>,
    at: usize,
    depth_limit: u32,
}

impl Reader {
    fn block(&mut self, indent: usize, depth: u32) -> Result<Value, Error> {
        if depth > self.depth_limit {
            let line = self.lines.get(self.at).map_or(0, |line| line.number);
            return Err(at(
                line,
                "flatten the document, because it nests too deeply",
            ));
        }
        if self.starts_item(indent) {
            self.sequence(indent, depth)
        } else {
            self.mapping(indent, depth)
        }
    }

    fn starts_item(&self, indent: usize) -> bool {
        self.lines.get(self.at).is_some_and(|line| {
            line.indent == indent && (line.content == "-" || line.content.starts_with("- "))
        })
    }

    fn sequence(&mut self, indent: usize, depth: u32) -> Result<Value, Error> {
        let mut items = Vec::new();
        while self.starts_item(indent) {
            let number = self.lines[self.at].number;
            let content = self.lines[self.at].content.clone();
            let after_dash = &content[1..];
            let spaces = after_dash.len() - after_dash.trim_start_matches(' ').len();
            let rest = after_dash[spaces..].to_owned();
            if rest.is_empty() {
                self.at += 1;
                let Some(next) = self.lines.get(self.at) else {
                    return Err(at(number, "give the item a value"));
                };
                if next.indent <= indent {
                    return Err(at(number, "give the item a value"));
                }
                let inner = next.indent;
                items.push(self.block(inner, depth + 1)?);
                continue;
            }
            if opens_block(&rest) {
                let inner = indent + 1 + spaces;
                self.lines[self.at] = Line {
                    number,
                    indent: inner,
                    content: rest,
                };
                items.push(self.block(inner, depth + 1)?);
                continue;
            }
            items.push(scalar_or_flow(&rest, number)?);
            self.at += 1;
        }
        Ok(Value::Array(items))
    }

    fn mapping(&mut self, indent: usize, depth: u32) -> Result<Value, Error> {
        let mut fields = Map::new();
        while let Some(line) = self.lines.get(self.at) {
            if line.indent < indent {
                break;
            }
            if line.indent > indent {
                return Err(at(line.number, "line up this line with the one above it"));
            }
            if line.content == "-" || line.content.starts_with("- ") {
                break;
            }
            let number = line.number;
            let Some((key, rest)) = split_key(&line.content) else {
                return Err(at(number, "write the line as a key and a value"));
            };
            if key == "<<" {
                return Err(at(
                    number,
                    "write the keys out, because a merge key is not read here",
                ));
            }
            self.at += 1;
            let value = if rest.is_empty() {
                self.nested(indent, depth, number)?
            } else {
                scalar_or_flow(&rest, number)?
            };
            if fields.insert(key.clone(), value).is_some() {
                return Err(at(
                    number,
                    &format!("remove one of the two keys named {key}"),
                ));
            }
        }
        Ok(Value::Object(fields))
    }

    fn nested(&mut self, indent: usize, depth: u32, number: usize) -> Result<Value, Error> {
        let Some(next) = self.lines.get(self.at) else {
            return Err(at(number, "give the key a value"));
        };
        if next.indent > indent {
            let inner = next.indent;
            return self.block(inner, depth + 1);
        }
        if next.indent == indent && (next.content == "-" || next.content.starts_with("- ")) {
            return self.sequence(indent, depth + 1);
        }
        Err(at(number, "give the key a value"))
    }
}

fn opens_block(rest: &str) -> bool {
    rest == "-" || rest.starts_with("- ") || split_key(rest).is_some()
}

fn split_key(content: &str) -> Option<(String, String)> {
    let bytes = content.as_bytes();
    if bytes
        .first()
        .is_some_and(|byte| *byte == b'[' || *byte == b'{')
    {
        return None;
    }
    let mut quote = None;
    let mut nesting = 0u32;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(b'"') if byte == b'\\' => index += 1,
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'[' | b'{' => nesting += 1,
                b']' | b'}' => nesting = nesting.saturating_sub(1),
                b':' if nesting == 0 => {
                    let following = bytes.get(index + 1);
                    if following.is_none() || following == Some(&b' ') {
                        let key = content[..index].trim_end();
                        let rest = content[index + 1..].trim();
                        return Some((unquote_key(key)?, rest.to_owned()));
                    }
                }
                _ => {}
            },
        }
        index += 1;
    }
    None
}

fn unquote_key(key: &str) -> Option<String> {
    if key.is_empty() {
        return None;
    }
    if key.starts_with('"') || key.starts_with('\'') {
        return read_quoted(key, 0).ok().and_then(|(text, used)| {
            if used == key.len() { Some(text) } else { None }
        });
    }
    if key.contains(['[', ']', '{', '}', ',']) {
        return None;
    }
    Some(key.to_owned())
}

fn scalar_or_flow(text: &str, number: usize) -> Result<Value, Error> {
    let (value, used) = read_node(text, 0, number)?;
    if text[used..].trim().is_empty() {
        Ok(value)
    } else {
        Err(at(
            number,
            "write one value, because more than one was found",
        ))
    }
}

fn read_node(text: &str, from: usize, number: usize) -> Result<(Value, usize), Error> {
    let bytes = text.as_bytes();
    let mut index = from;
    while bytes.get(index) == Some(&b' ') {
        index += 1;
    }
    match bytes.get(index) {
        None => Err(at(number, "write a value")),
        Some(b'[') => read_flow_sequence(text, index, number),
        Some(b'{') => read_flow_mapping(text, index, number),
        Some(b'"' | b'\'') => {
            let (text, used) = read_quoted(text, index)?;
            Ok((Value::String(text), used))
        }
        Some(_) => read_plain(text, index, number),
    }
}

fn read_flow_sequence(text: &str, from: usize, number: usize) -> Result<(Value, usize), Error> {
    let bytes = text.as_bytes();
    let mut index = from + 1;
    let mut items = Vec::new();
    loop {
        while bytes.get(index) == Some(&b' ') {
            index += 1;
        }
        if bytes.get(index) == Some(&b']') {
            return Ok((Value::Array(items), index + 1));
        }
        let (value, used) = read_node(text, index, number)?;
        items.push(value);
        index = used;
        while bytes.get(index) == Some(&b' ') {
            index += 1;
        }
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b']') => return Ok((Value::Array(items), index + 1)),
            _ => return Err(at(number, "close the list with a bracket")),
        }
    }
}

fn read_flow_mapping(text: &str, from: usize, number: usize) -> Result<(Value, usize), Error> {
    let bytes = text.as_bytes();
    let mut index = from + 1;
    let mut fields = Map::new();
    loop {
        while bytes.get(index) == Some(&b' ') {
            index += 1;
        }
        if bytes.get(index) == Some(&b'}') {
            return Ok((Value::Object(fields), index + 1));
        }
        let (key, used) = read_flow_key(text, index, number)?;
        index = used;
        while bytes.get(index) == Some(&b' ') {
            index += 1;
        }
        if bytes.get(index) != Some(&b':') {
            return Err(at(number, "separate the key and the value with a colon"));
        }
        index += 1;
        let (value, used) = read_node(text, index, number)?;
        index = used;
        if fields.insert(key.clone(), value).is_some() {
            return Err(at(
                number,
                &format!("remove one of the two keys named {key}"),
            ));
        }
        while bytes.get(index) == Some(&b' ') {
            index += 1;
        }
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b'}') => return Ok((Value::Object(fields), index + 1)),
            _ => return Err(at(number, "close the mapping with a brace")),
        }
    }
}

fn read_flow_key(text: &str, from: usize, number: usize) -> Result<(String, usize), Error> {
    let bytes = text.as_bytes();
    if matches!(bytes.get(from), Some(b'"' | b'\'')) {
        return read_quoted(text, from);
    }
    let mut index = from;
    while index < bytes.len() && !matches!(bytes[index], b':' | b',' | b'}' | b']') {
        index += 1;
    }
    let key = text[from..index].trim_end();
    if key.is_empty() {
        return Err(at(number, "write a key"));
    }
    Ok((key.to_owned(), index))
}

fn read_quoted(text: &str, from: usize) -> Result<(String, usize), Error> {
    let bytes = text.as_bytes();
    let opener = bytes[from];
    let mut out = String::new();
    let mut index = from + 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if opener == b'\'' {
            if byte == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    out.push('\'');
                    index += 2;
                    continue;
                }
                return Ok((out, index + 1));
            }
            let character = text[index..].chars().next().unwrap_or('\u{fffd}');
            out.push(character);
            index += character.len_utf8();
            continue;
        }
        if byte == b'"' {
            return Ok((out, index + 1));
        }
        if byte == b'\\' {
            let (character, used) = read_escape(text, index)?;
            out.push(character);
            index += used;
            continue;
        }
        let character = text[index..].chars().next().unwrap_or('\u{fffd}');
        out.push(character);
        index += character.len_utf8();
    }
    Err(invalid(
        "close the quotation, because it runs to the end of the line",
    ))
}

fn read_escape(text: &str, at_backslash: usize) -> Result<(char, usize), Error> {
    let bytes = text.as_bytes();
    let Some(marker) = bytes.get(at_backslash + 1) else {
        return Err(invalid("complete the escape, because it ends the line"));
    };
    let simple = match marker {
        b'"' => Some('"'),
        b'\\' => Some('\\'),
        b'/' => Some('/'),
        b'b' => Some('\u{8}'),
        b'f' => Some('\u{c}'),
        b'n' => Some('\n'),
        b'r' => Some('\r'),
        b't' => Some('\t'),
        b'0' => Some('\0'),
        _ => None,
    };
    if let Some(character) = simple {
        return Ok((character, 2));
    }
    if *marker != b'u' {
        return Err(invalid(format!(
            "use one of the escapes this build reads, because \\{} is not one of them",
            char::from(*marker)
        )));
    }
    let digits = text
        .get(at_backslash + 2..at_backslash + 6)
        .ok_or_else(|| invalid("write four hexadecimal digits after the escape"))?;
    let code = u32::from_str_radix(digits, 16)
        .map_err(|_| invalid("write four hexadecimal digits after the escape"))?;
    let character =
        char::from_u32(code).ok_or_else(|| invalid("write an escape that names a character"))?;
    Ok((character, 6))
}

fn read_plain(text: &str, from: usize, number: usize) -> Result<(Value, usize), Error> {
    let bytes = text.as_bytes();
    let mut index = from;
    while index < bytes.len() && !matches!(bytes[index], b',' | b']' | b'}') {
        index += 1;
    }
    let raw = text[from..index].trim_end();
    refuse_reserved(raw, number)?;
    Ok((plain_value(raw), index))
}

fn refuse_reserved(raw: &str, number: usize) -> Result<(), Error> {
    let refusal = match raw.as_bytes().first() {
        Some(b'&') => Some("write the value out, because an anchor is not read here"),
        Some(b'*') => Some("write the value out, because an alias is not read here"),
        Some(b'!') => Some("remove the tag, because none is read here"),
        Some(b'|' | b'>') => {
            Some("write the value on one line, because a block scalar is not read here")
        }
        Some(b'@' | b'`') => Some("quote the value, because it begins with a reserved character"),
        _ => None,
    };
    match refusal {
        Some(action) => Err(at(number, action)),
        None => Ok(()),
    }
}

fn plain_value(raw: &str) -> Value {
    match raw {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" | "~" => return Value::Null,
        _ => {}
    }
    if is_integer(raw) {
        if let Ok(signed) = raw.parse::<i64>() {
            return Value::Number(Number::from(signed));
        }
        if let Ok(unsigned) = raw.parse::<u64>() {
            return Value::Number(Number::from(unsigned));
        }
    }
    Value::String(raw.to_owned())
}

fn is_integer(raw: &str) -> bool {
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    digits == "0" || !digits.starts_with('0')
}

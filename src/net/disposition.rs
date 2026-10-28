//! the file name a server gives in `Content-Disposition`, read as RFC 6266 describes.

use percent_encoding::percent_decode_str;
use reqwest::header::{CONTENT_DISPOSITION, HeaderMap};

/// the name a response asks to be saved under, decoded but not yet made safe.
pub(super) fn file_name(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(CONTENT_DISPOSITION)?;
    // servers often send raw UTF-8 here, which a strict header parser would reject
    parse(&String::from_utf8_lossy(value.as_bytes()))
}

fn parse(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for (key, raw) in parameters(value) {
        if key.eq_ignore_ascii_case("filename*") {
            extended = extended.or_else(|| decode_extended(&raw));
        } else if key.eq_ignore_ascii_case("filename") {
            plain = plain.or(Some(raw));
        }
    }
    extended.or(plain).filter(|name| !name.trim().is_empty())
}

/// the `key=value` pairs after the disposition type, with quoted values unescaped.
fn parameters(value: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = value.find(';').map_or("", |at| &value[at + 1..]);
    while !rest.trim().is_empty() {
        let end = rest.find([';', '=']).unwrap_or(rest.len());
        let key = rest[..end].trim().to_owned();
        if !rest[end..].starts_with('=') {
            rest = rest.get(end + 1..).unwrap_or_default();
            continue;
        }
        let after = rest[end + 1..].trim_start();
        let (raw, remaining) = if let Some(quoted) = after.strip_prefix('"') {
            unquote(quoted)
        } else {
            let end = after.find(';').unwrap_or(after.len());
            (after[..end].trim().to_owned(), &after[end..])
        };
        found.push((key, raw));
        rest = remaining.find(';').map_or("", |at| &remaining[at + 1..]);
    }
    found
}

/// a quoted string's text with its escapes undone, and whatever follows the closing quote.
fn unquote(quoted: &str) -> (String, &str) {
    let mut text = String::new();
    let mut chars = quoted.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => return (text, &quoted[index + 1..]),
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    text.push(escaped);
                }
            }
            c => text.push(c),
        }
    }
    (text, "")
}

/// an RFC 8187 value such as `UTF-8''na%C3%AFve.csv`.
fn decode_extended(raw: &str) -> Option<String> {
    let mut parts = raw.splitn(3, '\'');
    let charset = parts.next()?;
    let _language = parts.next()?;
    let bytes: Vec<u8> = percent_decode_str(parts.next()?).collect();
    if charset.eq_ignore_ascii_case("utf-8") {
        String::from_utf8(bytes).ok()
    } else if charset.eq_ignore_ascii_case("iso-8859-1") {
        Some(bytes.into_iter().map(char::from).collect())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn plain_and_quoted_names() {
        assert_eq!(
            parse("attachment; filename=\"iris.csv\"").as_deref(),
            Some("iris.csv")
        );
        assert_eq!(
            parse("attachment;filename=images.zip").as_deref(),
            Some("images.zip")
        );
        assert_eq!(
            parse("inline; size=12; filename=x.bin").as_deref(),
            Some("x.bin")
        );
        assert_eq!(
            parse(r#"attachment; filename="a \"quoted\" name.txt""#).as_deref(),
            Some("a \"quoted\" name.txt")
        );
    }

    #[test]
    fn the_extended_name_wins_wherever_it_appears() {
        let value = "attachment; filename=\"naive.csv\"; filename*=UTF-8''na%C3%AFve%20data.csv";
        assert_eq!(parse(value).as_deref(), Some("naïve data.csv"));
        assert_eq!(
            parse("attachment; filename*=iso-8859-1'en'caf%E9.txt").as_deref(),
            Some("café.txt")
        );
    }

    #[test]
    fn no_name_is_none() {
        assert_eq!(parse("attachment"), None);
        assert_eq!(parse("attachment; filename=\"\""), None);
        assert_eq!(parse("attachment; flag; other=1"), None);
    }
}

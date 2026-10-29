//! the response headers downloads read: lengths, ranges, validators and the file name a server
//! gives in `Content-Disposition`, as RFC 6266 describes it.

use percent_encoding::percent_decode_str;
use reqwest::StatusCode;
use reqwest::header::{self, HeaderMap};

use crate::cache::Validators;

/// the name a response asks to be saved under, decoded but not yet made safe.
pub(super) fn file_name(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::CONTENT_DISPOSITION)?;
    // servers often send raw UTF-8 here, which a strict header parser would reject
    disposition(&String::from_utf8_lossy(value.as_bytes()))
}

pub(super) fn is_xml(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "application/xml" | "text/xml"
            )
        })
}

pub(super) fn is_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("text/html")
        })
}

/// the validator `If-Range` can trust: a strong `ETag`, else a modification date when no `ETag`
/// was recorded. RFC 9110 section 13.1.5 allows no weak tag, and no date beside a tag.
pub(super) fn if_range(validators: &Validators) -> Option<&str> {
    match &validators.etag {
        Some(tag) => (!tag.starts_with("W/")).then_some(tag.as_str()),
        None => validators.last_modified.as_deref(),
    }
}

/// whether a response states an `ETag` or `Last-Modified` other than the one `recorded`, so its
/// bytes may belong to another version of the file.
pub(super) fn changed(recorded: &Validators, stated: &Validators) -> bool {
    let differs = |stated: Option<&str>, recorded: Option<&str>| {
        stated.is_some_and(|stated| recorded != Some(stated))
    };
    differs(stated.etag.as_deref(), recorded.etag.as_deref())
        || differs(
            stated.last_modified.as_deref(),
            recorded.last_modified.as_deref(),
        )
}

pub(super) fn validators(headers: &HeaderMap) -> Validators {
    let text = |name| {
        headers
            .get(name)
            .and_then(|value: &header::HeaderValue| value.to_str().ok())
            .map(str::to_owned)
    };
    Validators {
        etag: text(header::ETAG),
        last_modified: text(header::LAST_MODIFIED),
    }
}

/// the first byte of a `Content-Range: bytes 100-199/200` answer.
pub(super) fn range_start(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let (range, _) = value.strip_prefix("bytes ")?.split_once('/')?;
    range.split_once('-')?.0.trim().parse().ok()
}

/// the size of the whole file, from a range answer or from the length of a full one.
pub(super) fn total_length(headers: &HeaderMap, status: StatusCode) -> Option<u64> {
    if status == StatusCode::PARTIAL_CONTENT {
        let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
        value.rsplit_once('/')?.1.trim().parse().ok()
    } else {
        headers
            .get(header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

fn disposition(value: &str) -> Option<String> {
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
    use super::*;

    fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(name.clone(), value.parse().unwrap());
        }
        map
    }

    #[test]
    fn plain_and_quoted_names() {
        assert_eq!(
            disposition("attachment; filename=\"iris.csv\"").as_deref(),
            Some("iris.csv")
        );
        assert_eq!(
            disposition("attachment;filename=images.zip").as_deref(),
            Some("images.zip")
        );
        assert_eq!(
            disposition("inline; size=12; filename=x.bin").as_deref(),
            Some("x.bin")
        );
        assert_eq!(
            disposition(r#"attachment; filename="a \"quoted\" name.txt""#).as_deref(),
            Some("a \"quoted\" name.txt")
        );
    }

    #[test]
    fn the_extended_name_wins_wherever_it_appears() {
        let value = "attachment; filename=\"naive.csv\"; filename*=UTF-8''na%C3%AFve%20data.csv";
        assert_eq!(disposition(value).as_deref(), Some("naïve data.csv"));
        assert_eq!(
            disposition("attachment; filename*=iso-8859-1'en'caf%E9.txt").as_deref(),
            Some("café.txt")
        );
    }

    #[test]
    fn no_name_is_none() {
        assert_eq!(disposition("attachment"), None);
        assert_eq!(disposition("attachment; filename=\"\""), None);
        assert_eq!(disposition("attachment; flag; other=1"), None);
    }

    #[test]
    fn ranges_and_lengths() {
        let partial = headers(&[(header::CONTENT_RANGE, "bytes 100-199/200")]);
        assert_eq!(range_start(&partial), Some(100));
        assert_eq!(
            total_length(&partial, StatusCode::PARTIAL_CONTENT),
            Some(200)
        );
        let full = headers(&[(header::CONTENT_LENGTH, "4177")]);
        assert_eq!(total_length(&full, StatusCode::OK), Some(4177));
    }

    #[test]
    fn a_date_guards_a_resume_only_when_no_etag_was_recorded() {
        let date = "Tue, 01 Oct 2024 10:00:00 GMT";
        for (etag, last_modified, sent) in [
            (Some("W/\"a\""), None, None),
            // RFC 9110 section 13.1.5: never a weak tag, and never a date beside an entity tag
            (Some("W/\"a\""), Some(date), None),
            (Some("\"a\""), Some(date), Some("\"a\"")),
            (None, Some(date), Some(date)),
            (None, None, None),
        ] {
            let validators = Validators {
                etag: etag.map(str::to_owned),
                last_modified: last_modified.map(str::to_owned),
            };
            assert_eq!(if_range(&validators), sent, "{validators:?}");
        }
    }

    #[test]
    fn pages_are_told_apart_from_files() {
        assert!(is_html(&headers(&[(
            header::CONTENT_TYPE,
            "text/html; charset=utf-8"
        )])));
        assert!(!is_html(&headers(&[(
            header::CONTENT_TYPE,
            "application/octet-stream"
        )])));
        assert!(!is_html(&headers(&[])));
    }
}

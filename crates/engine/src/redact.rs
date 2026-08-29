//! Redaction applied at the point of construction.

use std::fmt;

use serde::{Deserialize, Serialize, Serializer};

/// The fixed text every redacted value is replaced with.
pub const REDACTED: &str = "[redacted]";

/// A request header whose value is never written anywhere.
pub const SENSITIVE_HEADERS: [&str; 2] = ["authorization", "cookie"];

/// Reports whether a header name is one whose value is never written anywhere.
#[must_use]
pub fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS
        .iter()
        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
}

/// A value that is never written to any stream, file, or record.
///
/// Takes any value. Its debug, display, and serialized forms are the fixed
/// redacted text, so the value can only leave through an explicit exposure.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps a value so it cannot be written by accident.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Returns the wrapped value for the one call that must use it.
    #[must_use]
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> Serialize for Secret<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(REDACTED)
    }
}

/// A location string that carries no credential.
///
/// Takes any reference or location text. Returns text whose userinfo component
/// and whose every query parameter value have been replaced with the fixed
/// redacted text. It cannot be constructed any other way, so a secret cannot
/// reach an event, an error, a receipt, or a plan through it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct SafeUrl(String);

impl SafeUrl {
    /// Redacts a location string and returns the result.
    #[must_use]
    pub fn new(raw: &str) -> Self {
        Self(redact_location(raw))
    }

    /// Returns the redacted text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SafeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for SafeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

fn redact_location(raw: &str) -> String {
    let (head, fragment) = match raw.find('#') {
        Some(index) => (&raw[..index], &raw[index..]),
        None => (raw, ""),
    };
    let (before_query, query) = match head.find('?') {
        Some(index) => (&head[..index], Some(&head[index + 1..])),
        None => (head, None),
    };

    let mut output = redact_userinfo(before_query);
    if let Some(query) = query {
        output.push('?');
        output.push_str(&redact_query(query));
    }
    output.push_str(fragment);
    output
}

fn redact_userinfo(before_query: &str) -> String {
    let Some(scheme_end) = before_query.find("://") else {
        return before_query.to_owned();
    };
    let authority_start = scheme_end + 3;
    let authority_end = before_query[authority_start..]
        .find('/')
        .map_or(before_query.len(), |offset| authority_start + offset);
    let authority = &before_query[authority_start..authority_end];
    let Some(at) = authority.rfind('@') else {
        return before_query.to_owned();
    };
    let mut output = String::with_capacity(before_query.len() + REDACTED.len());
    output.push_str(&before_query[..authority_start]);
    output.push_str(REDACTED);
    output.push_str(&authority[at..]);
    output.push_str(&before_query[authority_end..]);
    output
}

fn redact_query(query: &str) -> String {
    let mut output = String::with_capacity(query.len());
    for (index, parameter) in query.split('&').enumerate() {
        if index > 0 {
            output.push('&');
        }
        match parameter.split_once('=') {
            Some((name, _)) => {
                output.push_str(name);
                output.push('=');
                output.push_str(REDACTED);
            }
            None => output.push_str(parameter),
        }
    }
    output
}

impl From<String> for SafeUrl {
    fn from(raw: String) -> Self {
        Self::new(&raw)
    }
}

impl From<SafeUrl> for String {
    fn from(url: SafeUrl) -> Self {
        url.0
    }
}

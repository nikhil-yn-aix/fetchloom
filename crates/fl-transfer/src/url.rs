//! Locations that never show their secrets.

use std::fmt;
use std::str::FromStr;

use url::Url;

const HIDDEN: &str = "***";

/// A location, held from the moment it enters the program.
///
/// Displayed or debugged, its user info, every query value and its fragment are hidden, so a
/// signed link or a password in a URL never reaches a log. The full text leaves this crate only
/// as a request.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SafeUrl(Url);

/// The scheme, host and port that credentials are scoped to.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
    default_port: bool,
}

/// Text that is not an absolute URL. The text itself is left out, since it may hold a secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an absolute URL: {0}")]
pub struct UrlError(url::ParseError);

impl SafeUrl {
    /// Reads an absolute URL.
    ///
    /// # Errors
    ///
    /// Returns [`UrlError`] when `text` is not an absolute URL.
    pub fn parse(text: &str) -> Result<Self, UrlError> {
        Url::parse(text).map(Self).map_err(UrlError)
    }

    /// The scheme, such as `https`.
    #[must_use]
    pub fn scheme(&self) -> &str {
        self.0.scheme()
    }

    /// The host, such as `zenodo.org` or `[::1]`, when the URL has one.
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        self.0.host_str()
    }

    /// The origin, when the URL has a host and a port its scheme implies or states.
    #[must_use]
    pub fn origin(&self) -> Option<Origin> {
        Some(Origin {
            scheme: self.0.scheme().to_owned(),
            host: self.0.host_str()?.to_owned(),
            port: self.0.port_or_known_default()?,
            default_port: self.0.port().is_none(),
        })
    }

    /// The user name and password written into the URL, when there are any.
    pub(crate) fn user_info(&self) -> Option<(&str, Option<&str>)> {
        let user = self.0.username();
        let password = self.0.password();
        (!user.is_empty() || password.is_some()).then_some((user, password))
    }
}

impl FromStr for SafeUrl {
    type Err = UrlError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl fmt::Display for SafeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = &self.0;
        write!(f, "{}:", url.scheme())?;
        if let Some(host) = url.host_str() {
            f.write_str("//")?;
            if self.user_info().is_some() {
                write!(f, "{HIDDEN}@")?;
            }
            f.write_str(host)?;
            if let Some(port) = url.port() {
                write!(f, ":{port}")?;
            }
        }
        f.write_str(url.path())?;
        if let Some(query) = url.query() {
            f.write_str("?")?;
            for (index, pair) in query.split('&').enumerate() {
                if index > 0 {
                    f.write_str("&")?;
                }
                match pair.split_once('=') {
                    Some((key, _)) => write!(f, "{key}={HIDDEN}")?,
                    None => f.write_str(HIDDEN)?,
                }
            }
        }
        if url.fragment().is_some() {
            write!(f, "#{HIDDEN}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for SafeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SafeUrl({self})")
    }
}

impl Origin {
    /// The scheme, such as `https`.
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The host as written in URLs, such as `zenodo.org` or `[::1]`.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The host, with `:port` after it when the port is not the scheme's default, as credentials
    /// are named.
    #[must_use]
    pub fn authority(&self) -> String {
        if self.default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme, self.authority())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> SafeUrl {
        text.parse().unwrap()
    }

    #[test]
    fn secrets_are_hidden_when_shown() {
        for (text, shown) in [
            (
                "https://zenodo.org/records/1/files/a.csv",
                "https://zenodo.org/records/1/files/a.csv",
            ),
            (
                "https://me:pw@host.org:8443/a",
                "https://***@host.org:8443/a",
            ),
            ("https://token@host.org/a", "https://***@host.org/a"),
            (
                "https://host.org/a?X-Amz-Signature=abc&id=7&flag",
                "https://host.org/a?X-Amz-Signature=***&id=***&***",
            ),
            ("https://host.org/a?=abc", "https://host.org/a?=***"),
            (
                "https://host.org/a#access_token=abc",
                "https://host.org/a#***",
            ),
            ("http://[::1]:9000/x", "http://[::1]:9000/x"),
            (
                "s3://bucket/key?versionId=3",
                "s3://bucket/key?versionId=***",
            ),
            ("mailto:me@host.org", "mailto:me@host.org"),
        ] {
            let parsed = url(text);
            assert_eq!(parsed.to_string(), shown, "{text}");
            assert_eq!(format!("{parsed:?}"), format!("SafeUrl({shown})"));
        }
    }

    #[test]
    fn origins_are_scheme_host_and_port() {
        let origin = url("https://Zenodo.org/a").origin().unwrap();
        assert_eq!(origin.to_string(), "https://zenodo.org");
        assert_eq!(
            origin,
            url("https://zenodo.org:443/b?c=d").origin().unwrap()
        );
        assert_ne!(origin, url("http://zenodo.org/a").origin().unwrap());
        let other = url("https://127.0.0.1:8443/a").origin().unwrap();
        assert_eq!(other.authority(), "127.0.0.1:8443");
        assert_eq!((other.scheme(), other.host()), ("https", "127.0.0.1"));
        assert_eq!(url("mailto:me@host.org").origin(), None);
    }

    #[test]
    fn a_bad_url_is_refused_without_repeating_it() {
        let error = SafeUrl::parse("secret-token-123").unwrap_err();
        assert!(!error.to_string().contains("secret"), "{error}");
    }
}

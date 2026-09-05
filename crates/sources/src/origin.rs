//! What a credential is bound to, and what a redirect leaving it looks like.

use std::fmt;

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::redact::SafeUrl;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    /// # Errors
    /// `reference.unresolved` when the location has no scheme, or a scheme
    /// this source does not serve.
    pub fn of(location: &str) -> Result<Self, Error> {
        let (scheme, rest) = location
            .split_once("://")
            .ok_or_else(|| unreachable(location))?;
        let scheme = scheme.to_lowercase();
        let default = match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => return Err(unreachable(location)),
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        let authority = authority
            .rsplit_once('@')
            .map_or(authority, |(_, after)| after);
        let (host, port) = match authority.rsplit_once(':') {
            Some((before, after)) if after.chars().all(|digit| digit.is_ascii_digit()) => {
                (before, after.parse().unwrap_or(default))
            }
            _ => (authority, default),
        };
        Ok(Self {
            scheme,
            host: host.trim_matches(['[', ']']).to_lowercase(),
            port,
        })
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// # Errors
    /// `reference.unresolved` when the location redirected from has no origin,
    /// and `network.status` when the target is relative to a path rather than
    /// to the origin, which this build does not resolve.
    pub fn join(from: &str, target: &str) -> Result<String, Error> {
        if target.contains("://") {
            return Ok(target.to_owned());
        }
        let origin = Self::of(from)?;
        if let Some(path) = target.strip_prefix('/') {
            return Ok(format!("{origin}/{path}"));
        }
        Err(Error::new(
            ErrorKind::NetworkStatus,
            format!(
                "ask the source for a location Fetchloom can follow, because it redirected to {}, which is neither an absolute location nor a rooted path",
                SafeUrl::new(target)
            ),
        )
        .with_source(from))
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}://{}:{}", self.scheme, self.host, self.port)
    }
}

fn unreachable(location: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "give a location beginning with http or https, because {} names no scheme this source reaches",
            SafeUrl::new(location)
        ),
    )
}

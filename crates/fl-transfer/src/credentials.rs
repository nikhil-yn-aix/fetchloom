//! Credentials: the header a request carries for its origin, found only when that origin is
//! about to be contacted, and never shown.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use fl_model::error::ErrorKind;

use crate::url::Origin;

const HIDDEN: &str = "***";

/// A secret value. It shows as `***` when displayed or debugged.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// The value itself, for the one place that sends it.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(HIDDEN)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(HIDDEN)
    }
}

/// A header that proves who is asking, such as `Authorization: Bearer ...`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Credential {
    /// The header's name.
    pub header: String,
    /// The header's value.
    pub value: Secret,
}

impl Credential {
    /// A token as people write it: sent whole as the `Authorization` value when it contains a
    /// space, such as `Token abc`, and as `Bearer <token>` otherwise.
    #[must_use]
    pub fn token(token: &str) -> Self {
        let token = token.trim();
        let value = if token.contains(' ') {
            token.to_owned()
        } else {
            format!("Bearer {token}")
        };
        Self {
            header: "Authorization".to_owned(),
            value: Secret::new(value),
        }
    }
}

/// A place credentials are found.
pub trait Credentials: Send + Sync {
    /// The credential for `origin`, if this place holds one.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError`] when the place exists but cannot be trusted or read.
    fn find(&self, origin: &Origin) -> Result<Option<Credential>, CredentialError>;
}

/// A credentials file that cannot be used.
#[derive(Debug, thiserror::Error)]
#[error("`{}` {reason}", path.display())]
pub struct CredentialError {
    /// The file.
    pub path: PathBuf,
    /// What is wrong with it.
    pub reason: String,
}

impl CredentialError {
    /// The kind a command reports for this error.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        ErrorKind::AuthRejected
    }
}

/// Places tried in order, the first that holds a credential winning. Only https origins are
/// ever looked up, so no credential is sent over plain http.
pub struct Chain(Vec<Box<dyn Credentials>>);

impl Chain {
    /// The places, in the order they are tried.
    #[must_use]
    pub fn new(places: Vec<Box<dyn Credentials>>) -> Self {
        Self(places)
    }
}

impl Credentials for Chain {
    fn find(&self, origin: &Origin) -> Result<Option<Credential>, CredentialError> {
        if origin.scheme() != "https" {
            return Ok(None);
        }
        for place in &self.0 {
            if let Some(found) = place.find(origin)? {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
}

type Read = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// `FETCHLOOM_TOKEN_<HOST>`: the host, with `:port` when it is not the default, uppercased and
/// every character that is not a letter or digit written as `_`.
pub struct Environment(Read);

impl Environment {
    /// Reads variables with `read`, so tests never see the real environment.
    #[must_use]
    pub fn new(read: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        Self(Box::new(read))
    }

    /// The variable that holds the token for `origin`.
    #[must_use]
    pub fn variable(origin: &Origin) -> String {
        let host: String = origin
            .authority()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("FETCHLOOM_TOKEN_{host}")
    }
}

impl Credentials for Environment {
    fn find(&self, origin: &Origin) -> Result<Option<Credential>, CredentialError> {
        Ok((self.0)(&Self::variable(origin))
            .filter(|token| !token.trim().is_empty())
            .map(|token| Credential::token(&token)))
    }
}

/// A variable a provider documents, such as `ZENODO_TOKEN`, sent to one host in the header
/// shape its adapter declares.
pub struct Variable {
    authority: String,
    name: String,
    header: String,
    format: String,
    read: Read,
}

impl Variable {
    /// The token in `variable` goes to the host `authority` as the header `header`, its value
    /// `format` with `{token}` replaced, such as `Bearer {token}`.
    #[must_use]
    pub fn new(
        authority: &str,
        variable: &str,
        header: &str,
        format: &str,
        read: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            authority: authority.to_ascii_lowercase(),
            name: variable.to_owned(),
            header: header.to_owned(),
            format: format.to_owned(),
            read: Box::new(read),
        }
    }
}

impl Credentials for Variable {
    fn find(&self, origin: &Origin) -> Result<Option<Credential>, CredentialError> {
        if origin.authority() != self.authority {
            return Ok(None);
        }
        Ok((self.read)(&self.name)
            .filter(|token| !token.trim().is_empty())
            .map(|token| Credential {
                header: self.header.clone(),
                value: Secret::new(self.format.replace("{token}", token.trim())),
            }))
    }
}

/// `credentials.toml`: `host = "token"` lines, read the first time a lookup reaches it.
///
/// On unix the file is refused when its group or others may read it.
pub struct CredentialsFile {
    path: PathBuf,
    tokens: OnceLock<Result<HashMap<String, Secret>, String>>,
}

impl CredentialsFile {
    /// The file at `path`, which need not exist.
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self {
            path,
            tokens: OnceLock::new(),
        }
    }
}

impl Credentials for CredentialsFile {
    fn find(&self, origin: &Origin) -> Result<Option<Credential>, CredentialError> {
        let tokens = self
            .tokens
            .get_or_init(|| load(&self.path))
            .as_ref()
            .map_err(|reason| CredentialError {
                path: self.path.clone(),
                reason: reason.clone(),
            })?;
        Ok(tokens
            .get(&origin.authority())
            .map(|token| Credential::token(token.expose())))
    }
}

fn load(path: &Path) -> Result<HashMap<String, Secret>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(format!("cannot be read: {error}")),
    };
    private(path)?;
    let table: toml::Table = toml::from_str(&text).map_err(|_| "is not valid TOML".to_owned())?;
    table
        .into_iter()
        .map(|(host, value)| match value {
            toml::Value::String(token) => Ok((host.to_ascii_lowercase(), Secret::new(token))),
            _ => Err(format!("holds `{host}` as something other than a string")),
        })
        .collect()
}

#[cfg(unix)]
fn private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)
        .map_err(|error| format!("cannot be read: {error}"))?
        .permissions()
        .mode();
    if mode.trailing_zeros() >= 6 {
        Ok(())
    } else {
        Err(format!(
            "can be read by others (mode {:o}); run `chmod 600` on it",
            mode & 0o777
        ))
    }
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the unix version refuses files others can read"
)]
const fn private(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url::SafeUrl;

    fn origin(text: &str) -> Origin {
        SafeUrl::parse(text).unwrap().origin().unwrap()
    }

    fn header(found: Option<Credential>) -> Option<(String, String)> {
        found.map(|credential| (credential.header, credential.value.expose().to_owned()))
    }

    fn environment(pairs: &[(&str, &str)]) -> Environment {
        let pairs: HashMap<String, String> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        Environment::new(move |name| pairs.get(name).cloned())
    }

    #[test]
    fn the_environment_names_the_host_and_port() {
        assert_eq!(
            Environment::variable(&origin("https://zenodo.org/x")),
            "FETCHLOOM_TOKEN_ZENODO_ORG"
        );
        assert_eq!(
            Environment::variable(&origin("https://127.0.0.1:8443/x")),
            "FETCHLOOM_TOKEN_127_0_0_1_8443"
        );
        let env = environment(&[
            ("FETCHLOOM_TOKEN_ZENODO_ORG", "abc"),
            ("FETCHLOOM_TOKEN_DATAVERSE_ORG", "Token xyz"),
        ]);
        let bearer = env.find(&origin("https://zenodo.org/x")).unwrap();
        assert_eq!(
            header(bearer),
            Some(("Authorization".to_owned(), "Bearer abc".to_owned()))
        );
        let whole = env.find(&origin("https://dataverse.org/x")).unwrap();
        assert_eq!(header(whole).unwrap().1, "Token xyz");
        assert!(
            env.find(&origin("https://zenodo.org:8443/x"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_chain_never_looks_up_plain_http_and_takes_the_first_match() {
        let chain = Chain::new(vec![
            Box::new(Variable::new(
                "dataverse.org",
                "DATAVERSE_KEY",
                "X-Dataverse-key",
                "{token}",
                |name| (name == "DATAVERSE_KEY").then(|| "k1".to_owned()),
            )),
            Box::new(environment(&[("FETCHLOOM_TOKEN_DATAVERSE_ORG", "k2")])),
        ]);
        let found = header(chain.find(&origin("https://dataverse.org/a")).unwrap());
        assert_eq!(found, Some(("X-Dataverse-key".to_owned(), "k1".to_owned())));
        assert!(
            chain
                .find(&origin("http://dataverse.org/a"))
                .unwrap()
                .is_none()
        );
        assert!(
            chain
                .find(&origin("https://other.org/a"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn secrets_never_show() {
        let credential = Credential::token("abc");
        assert!(!format!("{credential:?} {}", credential.value).contains("abc"));
        assert_eq!(credential.value.to_string(), "***");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_an_error_and_a_missing_one_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let missing = CredentialsFile::new(temp.path().join("none.toml"));
        assert!(missing.find(&origin("https://a.org/")).unwrap().is_none());
        let error = CredentialsFile::new(temp.path().to_owned())
            .find(&origin("https://a.org/"))
            .unwrap_err();
        assert!(error.to_string().contains("cannot be read"), "{error}");
    }

    fn private_file(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn the_file_is_read_once_when_first_needed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("credentials.toml");
        let file = CredentialsFile::new(path.clone());
        assert!(file.find(&origin("https://a.org/")).unwrap().is_none());
        fs::write(&path, "\"a.org\" = \"abc\"\n").unwrap();
        assert!(file.find(&origin("https://a.org/")).unwrap().is_none());
        let fresh = CredentialsFile::new(path.clone());
        let found = header(fresh.find(&origin("https://A.org/x")).unwrap());
        assert_eq!(found.unwrap().1, "Bearer abc");
        fs::write(&path, "a = 1\n").unwrap();
        let error = CredentialsFile::new(path)
            .find(&origin("https://a.org/"))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .ends_with("holds `a` as something other than a string")
        );
        assert_eq!(error.kind(), ErrorKind::AuthRejected);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("credentials.toml");
        fs::write(&path, "\"a.org\" = \"abc\"\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let error = CredentialsFile::new(path.clone())
            .find(&origin("https://a.org/"))
            .unwrap_err();
        assert!(error.to_string().contains("mode 644"), "{error}");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            CredentialsFile::new(path)
                .find(&origin("https://a.org/"))
                .unwrap()
                .is_some()
        );
    }
}

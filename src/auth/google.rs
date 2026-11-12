//! google's application default credentials, found where google's own libraries look. other kinds
//! are refused by name, never passed over for another identity; no metadata server is asked.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ring::signature::RsaKeyPair;
use rustls::pki_types::pem::PemObject as _;
use serde::Deserialize;

use crate::error::{Error, Kind};
use crate::source::Var;

/// the scope of a token that reads cloud storage.
pub(crate) const STORAGE_READ: &str = "https://www.googleapis.com/auth/devstorage.read_only";
/// the scope of a token that reads google drive: every file its account can see.
pub(crate) const DRIVE_READ: &str = "https://www.googleapis.com/auth/drive.readonly";
/// where every token comes from, whatever a credentials file names.
pub(crate) const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// the token endpoints a credentials file may name: all google's, all answering as the first.
const KNOWN_ENDPOINTS: [&str; 3] = [
    TOKEN_ENDPOINT,
    "https://accounts.google.com/o/oauth2/token",
    "https://www.googleapis.com/oauth2/v4/token",
];
/// how long a service account's token is asked to last: the most google grants.
const LIFETIME: u64 = 3600;
/// how to give fetchloom application default credentials.
pub(crate) const SIGN_IN: &str = "run `gcloud auth application-default login`, or set GOOGLE_APPLICATION_CREDENTIALS to a service account's key file";

/// credentials found on this machine, and the file that held them.
pub(crate) struct Credentials {
    identity: Identity,
    path: PathBuf,
    /// whether `GOOGLE_APPLICATION_CREDENTIALS` named the file.
    named: bool,
}

enum Identity {
    /// a service account's key, which signs an assertion of who asks.
    Service {
        email: String,
        key: Box<RsaKeyPair>,
        key_id: Option<String>,
    },
    /// a person's login, which refreshes a token through google's client for it.
    Person {
        client_id: String,
        client_secret: String,
        refresh_token: String,
    },
}

/// an access token and how long it lives.
pub(crate) struct Token {
    /// the token itself, a secret.
    pub(crate) value: String,
    pub(crate) lifetime: Duration,
}

/// the credentials on this machine: the file `GOOGLE_APPLICATION_CREDENTIALS` names, else gcloud's
/// application default credentials. none when neither is there.
///
/// # Errors
///
/// `usage` when `GOOGLE_APPLICATION_CREDENTIALS` names a file that cannot be read, `local.io` for
/// gcloud's file that cannot be read, and `auth.required` for a file that holds no credentials
/// fetchloom reads, such as federated or impersonated ones, or names another token endpoint.
pub(crate) fn find(var: Var<'_>) -> Result<Option<Credentials>, Error> {
    found(var, cfg!(windows))
}

/// [`find`], on windows when `windows` says so.
fn found(var: Var<'_>, windows: bool) -> Result<Option<Credentials>, Error> {
    if let Some(named) = set(var, "GOOGLE_APPLICATION_CREDENTIALS") {
        let path = PathBuf::from(named);
        let bytes = std::fs::read(&path).map_err(|err| {
            Error::new(
                Kind::Usage,
                format!(
                    "GOOGLE_APPLICATION_CREDENTIALS names {}, which fetchloom cannot read: {}",
                    path.display(),
                    crate::error::said(&err)
                ),
            )
            .help("set it to a service account's key file, or unset it to use the login `gcloud auth application-default login` saves")
        })?;
        return read(&bytes, &path, true).map(Some);
    }
    let Some(path) = gcloud_file(var, windows) else {
        return Ok(None);
    };
    match std::fs::read(&path) {
        Ok(bytes) => read(&bytes, &path, false).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::io(
            format!("could not read {}", path.display()),
            &err,
        )),
    }
}

/// where gcloud keeps application default credentials.
fn gcloud_file(var: Var<'_>, windows: bool) -> Option<PathBuf> {
    let folder = match set(var, "CLOUDSDK_CONFIG") {
        Some(folder) => PathBuf::from(folder),
        None if windows => PathBuf::from(set(var, "APPDATA")?).join("gcloud"),
        None => PathBuf::from(set(var, "HOME")?)
            .join(".config")
            .join("gcloud"),
    };
    Some(folder.join("application_default_credentials.json"))
}

impl Credentials {
    /// the file the credentials came from and, for a service account, its email, as `auth`
    /// lists them. a person's login names no email.
    pub(crate) fn described(&self) -> (&Path, Option<&str>) {
        let email = match &self.identity {
            Identity::Service { email, .. } => Some(email.as_str()),
            Identity::Person { .. } => None,
        };
        (&self.path, email)
    }

    /// who the credentials sign in as, as messages name them.
    pub(crate) fn shown(&self) -> String {
        match &self.identity {
            Identity::Service { email, .. } => {
                format!("the service account {email} in {}", self.path.display())
            }
            Identity::Person { .. } => format!("the google login in {}", self.path.display()),
        }
    }

    /// the form that asks the token endpoint for a token of `scope` at `now`. a login's token
    /// carries every scope the person granted, whatever `scope` asks.
    ///
    /// # Errors
    ///
    /// `auth.required` when the key cannot sign.
    pub(crate) fn grant(&self, scope: &str, now: SystemTime) -> Result<String, Error> {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        match &self.identity {
            Identity::Service { email, key, key_id } => {
                let assertion = assertion(email, key, key_id.as_deref(), scope, now)?;
                form.append_pair("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer")
                    .append_pair("assertion", &assertion);
            }
            Identity::Person {
                client_id,
                client_secret,
                refresh_token,
            } => {
                form.append_pair("grant_type", "refresh_token")
                    .append_pair("client_id", client_id)
                    .append_pair("client_secret", client_secret)
                    .append_pair("refresh_token", refresh_token);
            }
        }
        Ok(form.finish())
    }

    /// the token in the token endpoint's answer, of `status` and `body`, which the network did
    /// not take for a failure worth another try.
    ///
    /// # Errors
    ///
    /// `auth.required` with what google said when it refused the grant, such as a revoked login,
    /// and `source.invalid` for an answer without a bearer token.
    pub(crate) fn answer(&self, status: u16, body: &[u8]) -> Result<Token, Error> {
        let answer: Option<Answer> = serde_json::from_slice(body).ok();
        if status != 200 {
            let said = answer
                .and_then(|answer| {
                    let code = answer.error?;
                    Some(match answer.error_description {
                        Some(description) => format!("{code}: {description}"),
                        None => code,
                    })
                })
                .map_or_else(
                    || format!("google's token endpoint answered {status}"),
                    |said| format!("google said {}", plain(&said)),
                );
            return Err(Error::new(
                Kind::AuthRequired,
                format!("google refused {}", self.shown()),
            )
            .detail(said)
            .help(match &self.identity {
                Identity::Service { email, .. } => format!(
                    "the key may be deleted or disabled: make a new key for {email}, or set GOOGLE_APPLICATION_CREDENTIALS to another one"
                ),
                Identity::Person { .. } if self.named => {
                    "sign in again with `gcloud auth application-default login`, then unset GOOGLE_APPLICATION_CREDENTIALS, which names this file".to_owned()
                }
                Identity::Person { .. } => {
                    "sign in again with `gcloud auth application-default login`".to_owned()
                }
            }));
        }
        let unusable = |what: &str| {
            Error::new(
                Kind::SourceInvalid,
                format!("google's token endpoint sent {what}"),
            )
            .help(crate::error::report_answer())
        };
        let answer = answer.ok_or_else(|| unusable("an answer fetchloom could not read"))?;
        if answer
            .token_type
            .as_deref()
            .is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer"))
        {
            return Err(unusable("a token of another kind than bearer"));
        }
        // a token goes in a header, so one a header cannot carry is no token
        let value = answer
            .access_token
            .filter(|token| !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic()))
            .ok_or_else(|| unusable("no token a request can carry"))?;
        let seconds = match answer.expires_in {
            Some(serde_json::Value::Number(number)) => number.as_u64(),
            Some(serde_json::Value::String(text)) => text.trim().parse().ok(),
            _ => None,
        };
        Ok(Token {
            value,
            lifetime: Duration::from_secs(seconds.unwrap_or(LIFETIME)),
        })
    }
}

/// what the token endpoint answers: a token, or why it gave none.
#[derive(Deserialize)]
struct Answer {
    access_token: Option<String>,
    expires_in: Option<serde_json::Value>,
    token_type: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// `said` with control characters escaped and at most 300 characters kept, so a server's words
/// cannot rewrite a terminal or flood it.
pub(crate) fn plain(said: &str) -> String {
    let mut shown = String::new();
    for (count, character) in said.chars().enumerate() {
        if count == 300 {
            shown.push('…');
            break;
        }
        if character.is_control() {
            let _ = write!(shown, "{}", character.escape_debug());
        } else {
            shown.push(character);
        }
    }
    shown
}

#[derive(Deserialize)]
struct File {
    #[serde(rename = "type")]
    kind: Option<String>,
    client_email: Option<String>,
    private_key: Option<String>,
    private_key_id: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
    token_uri: Option<String>,
}

/// the credentials in the file at `path`, which held `bytes`.
fn read(bytes: &[u8], path: &Path, named: bool) -> Result<Credentials, Error> {
    let shown = path.display();
    // serde's words can quote a value of the file, which may be a secret, so only the place
    // where reading stopped is told
    let file: File = serde_json::from_slice(bytes).map_err(|err| {
        Error::new(
            Kind::AuthRequired,
            format!(
                "{shown} is not a google credentials file fetchloom can read: reading stopped at line {}, column {}",
                err.line(),
                err.column()
            ),
        )
        .help(SIGN_IN)
    })?;
    let missing = |field: &str| {
        Error::new(
            Kind::AuthRequired,
            format!("{shown} has no {field}, which its credentials need"),
        )
        .help(SIGN_IN)
    };
    let text = |value: Option<String>, field: &str| {
        value
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| missing(field))
    };
    if let Some(uri) = file
        .token_uri
        .as_deref()
        .filter(|uri| !KNOWN_ENDPOINTS.contains(uri))
    {
        let origin = url::Url::parse(uri)
            .map(|url| format!(": {}", url.origin().ascii_serialization()))
            .unwrap_or_default();
        return Err(Error::new(
            Kind::AuthRequired,
            format!("{shown} sends its token requests to a server other than google's{origin}"),
        )
        .help("fetchloom asks only https://oauth2.googleapis.com/token for tokens: use credentials made for google cloud"));
    }
    let identity = match file.kind.as_deref() {
        Some("service_account") => {
            let email = text(file.client_email, "client_email")?;
            let pem = text(file.private_key, "private_key")?;
            let unreadable = || {
                Error::new(
                    Kind::AuthRequired,
                    format!("{shown} holds a private key fetchloom cannot read, where an RSA key in PEM was expected"),
                )
                .help(format!("download a new JSON key for {email}"))
            };
            let der = rustls::pki_types::PrivatePkcs8KeyDer::from_pem_slice(pem.as_bytes())
                .map_err(|_| unreadable())?;
            let key = RsaKeyPair::from_pkcs8(der.secret_pkcs8_der()).map_err(|_| unreadable())?;
            Identity::Service {
                email,
                key: Box::new(key),
                key_id: file.private_key_id.filter(|id| !id.is_empty()),
            }
        }
        Some("authorized_user") => Identity::Person {
            client_id: text(file.client_id, "client_id")?,
            client_secret: text(file.client_secret, "client_secret")?,
            refresh_token: text(file.refresh_token, "refresh_token")?,
        },
        Some(other) => {
            let named = match other {
                "external_account" => "workload identity federation".to_owned(),
                "external_account_authorized_user" => "workforce identity federation".to_owned(),
                "impersonated_service_account" => "service account impersonation".to_owned(),
                other => format!("the type {}", plain(other)),
            };
            return Err(Error::new(
                Kind::AuthRequired,
                format!("{shown} holds credentials for {named}, which fetchloom does not read"),
            )
            .help(SIGN_IN));
        }
        None => return Err(missing("type")),
    };
    Ok(Credentials {
        identity,
        path: path.to_path_buf(),
        named,
    })
}

/// a JSON web token that says `email` asks for a token of `scope` at `now`, signed with `key`.
fn assertion(
    email: &str,
    key: &RsaKeyPair,
    key_id: Option<&str>,
    scope: &str,
    now: SystemTime,
) -> Result<String, Error> {
    let issued = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
    if let Some(key_id) = key_id {
        header["kid"] = key_id.into();
    }
    let claims = serde_json::json!({
        "iss": email,
        "scope": scope,
        "aud": TOKEN_ENDPOINT,
        "iat": issued,
        "exp": issued + LIFETIME,
    });
    let signed = format!(
        "{}.{}",
        url_base64(header.to_string().as_bytes()),
        url_base64(claims.to_string().as_bytes())
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .map_err(|_| {
        Error::new(
            Kind::AuthRequired,
            format!("the key of the service account {email} could not sign a request for a token"),
        )
        .help(format!("download a new JSON key for {email}"))
    })?;
    Ok(format!("{signed}.{}", url_base64(&signature)))
}

/// base64 with the URL alphabet and no padding, as JSON web tokens write their parts.
fn url_base64(bytes: &[u8]) -> String {
    crate::source::base64(bytes)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

/// a variable's text, when it is set and not empty.
fn set(var: Var<'_>, name: &str) -> Option<String> {
    var(name)
        .and_then(|value| value.into_string().ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests;

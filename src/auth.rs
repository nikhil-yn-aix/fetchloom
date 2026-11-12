//! accounts at the sources: which key each one takes, where that key is kept, how to check it,
//! and how a person gets one. a key is kept where the source's own tools keep it, so one key
//! works in fetchloom and in them, and a person never makes a second.

pub(crate) mod aws;
pub(crate) mod netrc;
pub(crate) mod store;

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use url::Url;

use crate::error::{Error, Kind};
use crate::net::Credential;
use crate::source::Var;

/// one step of a sign-in guide: a page to open, or something to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Open(String),
    Do(String),
}

/// an account at one source, or a login for one host.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Account {
    HuggingFace,
    Kaggle,
    Zenodo,
    Figshare,
    Osf,
    /// one installation: a dataverse key works only on the server that made it. the host, with
    /// its port when that is not 443
    Dataverse(String),
    /// a username and password for one host, kept in `.netrc`, as for nasa earthdata or physionet
    Login(String),
}

const EARTHDATA: &str = "urs.earthdata.nasa.gov";
const PHYSIONET: &str = "physionet.org";
const HARVARD: &str = "dataverse.harvard.edu";

/// what a person types after `fetchloom auth login`.
pub(crate) const NAMES: &str =
    "hf, kaggle, zenodo, figshare, osf, dataverse, earthdata, physionet or a host name";

/// what a refused request needs: the account to sign in to, and a page whose terms the person
/// must accept first, when a key alone is not enough.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Need {
    pub(crate) account: Account,
    pub(crate) terms: Option<String>,
    /// the page that shows the name asked, from a source that answers alike for a name that
    /// does not exist, so the name may be wrong rather than the account missing.
    pub(crate) maybe_missing: Option<String>,
}

/// the kind of secret an account takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Secret {
    Token,
    Password,
}

/// where a key came from, as `auth` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Variable(&'static str),
    File(PathBuf),
    Keyring,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Variable(name) => write!(f, "{name}"),
            Origin::File(path) => write!(f, "{}", path.display()),
            Origin::Keyring => f.write_str(keyring_name()),
        }
    }
}

/// a key found for an account, and where.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    pub(crate) origin: Origin,
    /// the token, or for a login the username and password
    pub(crate) secret: String,
    pub(crate) login: Option<String>,
}

/// a request that proves a key works, and how to read who it belongs to.
pub(crate) struct Check {
    pub(crate) url: Url,
    pub(crate) who: fn(&serde_json::Value) -> Option<String>,
}

/// the name of the system's credential store, as a person knows it.
pub(crate) fn keyring_name() -> &'static str {
    if cfg!(windows) {
        "windows credential manager"
    } else if cfg!(target_os = "macos") {
        "the macos keychain"
    } else {
        "the system keyring"
    }
}

impl Account {
    /// the account `name` means, with `host` naming a dataverse installation.
    pub(crate) fn parse(name: &str, host: Option<&str>) -> Result<Account, Error> {
        let lowered = name.trim().to_ascii_lowercase();
        let account = match lowered.as_str() {
            "hf" | "huggingface" | "hugging-face" | "huggingface.co" | "hf.co" => {
                Account::HuggingFace
            }
            "kaggle" | "kaggle.com" => Account::Kaggle,
            "zenodo" | "zenodo.org" => Account::Zenodo,
            "figshare" | "figshare.com" => Account::Figshare,
            "osf" | "osf.io" => Account::Osf,
            "dataverse" => match host {
                None => Account::Dataverse(HARVARD.to_owned()),
                Some(host) => Account::Dataverse(dataverse_host(host).ok_or_else(|| {
                    // what was typed is not repeated, since it may hold a password
                    Error::new(
                        Kind::Usage,
                        "--host takes a dataverse installation's host name, with its port when that is not 443",
                    )
                    .help("use it as `fetchloom auth login dataverse --host dataverse.example.edu`, or with a port as `--host dataverse.example.edu:8443`")
                })?),
            },
            "earthdata" | "nasa" | EARTHDATA => Account::Login(EARTHDATA.to_owned()),
            "physionet" | PHYSIONET => Account::Login(PHYSIONET.to_owned()),
            other if other.contains('.') && one_word(other) && !other.contains(['/', '\\', ':', '@', '?', '#', '%'])
                && url::Host::parse(other).is_ok() => {
                Account::Login(other.to_owned())
            }
            _ => {
                return Err(Error::new(
                    Kind::Usage,
                    format!(
                        "fetchloom does not know an account called \"{}\"",
                        crate::net::mask_query(name).escape_debug()
                    ),
                )
                .help(format!(
                    "use one of {NAMES}, as in `fetchloom auth login hf`"
                )));
            }
        };
        if host.is_some() && !matches!(account, Account::Dataverse(_)) {
            return Err(
                Error::new(Kind::Usage, "--host names a dataverse installation").help(
                    "use it as `fetchloom auth login dataverse --host dataverse.example.edu`",
                ),
            );
        }
        Ok(account)
    }

    /// the source whose API answered from `host`, for a refusal that names no account.
    pub(crate) fn from_host(host: &str) -> Option<Account> {
        let host = host.to_ascii_lowercase();
        let bare = host.trim_start_matches("www.");
        Some(match bare {
            "huggingface.co" | "hf.co" => Account::HuggingFace,
            "kaggle.com" | "api.kaggle.com" => Account::Kaggle,
            "zenodo.org" => Account::Zenodo,
            "api.figshare.com" | "figshare.com" | "ndownloader.figshare.com" => Account::Figshare,
            "api.osf.io" | "osf.io" | "files.osf.io" => Account::Osf,
            _ => return None,
        })
    }

    /// the name the index and the credential store use.
    pub(crate) fn id(&self) -> String {
        match self {
            Account::HuggingFace => "hf".to_owned(),
            Account::Kaggle => "kaggle".to_owned(),
            Account::Zenodo => "zenodo".to_owned(),
            Account::Figshare => "figshare".to_owned(),
            Account::Osf => "osf".to_owned(),
            Account::Dataverse(host) => format!("dataverse:{host}"),
            Account::Login(host) => format!("login:{host}"),
        }
    }

    /// the command that signs in to this account.
    pub(crate) fn command(&self) -> String {
        match self {
            Account::Dataverse(host) if host == HARVARD => {
                "fetchloom auth login dataverse".to_owned()
            }
            Account::Dataverse(host) => format!("fetchloom auth login dataverse --host {host}"),
            Account::Login(host) if host == EARTHDATA => {
                "fetchloom auth login earthdata".to_owned()
            }
            Account::Login(host) if host == PHYSIONET => {
                "fetchloom auth login physionet".to_owned()
            }
            Account::Login(host) => format!("fetchloom auth login {host}"),
            other => format!("fetchloom auth login {}", other.id()),
        }
    }

    pub(crate) fn secret(&self) -> Secret {
        match self {
            Account::Login(_) => Secret::Password,
            _ => Secret::Token,
        }
    }

    /// how a person gets a key, from an account they may not have yet to the terms some
    /// datasets ask them to accept, as steps to follow in order.
    pub(crate) fn guide(&self) -> Vec<Step> {
        let join = |site: &str| {
            Step::Do(format!(
                "make a free account at {site} if you have none, and sign in"
            ))
        };
        match self {
            Account::HuggingFace => vec![
                join("huggingface.co"),
                Step::Open("https://huggingface.co/settings/tokens".into()),
                Step::Do("create a token of the read type, or a fine-grained one that can read the repositories you need".into()),
                Step::Do("a gated dataset also needs its terms accepted on its own page, with the same account".into()),
            ],
            Account::Kaggle => vec![
                join("kaggle.com"),
                Step::Open("https://www.kaggle.com/settings/api".into()),
                Step::Do("under api tokens, create a token and copy it; it starts with KGAT_".into()),
                Step::Do("a kaggle.json you downloaded before works too: paste its path".into()),
                Step::Do("a competition also needs its rules accepted on its page, under data, with the same account".into()),
            ],
            Account::Zenodo => vec![
                join("zenodo.org"),
                Step::Open("https://zenodo.org/account/settings/applications/tokens/new/".into()),
                Step::Do("name it fetchloom and create it; downloads need no scopes".into()),
            ],
            Account::Figshare => vec![
                join("figshare.com"),
                Step::Open("https://figshare.com/account/applications".into()),
                Step::Do("under personal tokens, create one and copy it; figshare shows it only once".into()),
            ],
            Account::Osf => vec![
                join("osf.io"),
                Step::Open("https://osf.io/settings/tokens/".into()),
                Step::Do("create a token with the osf.full_read scope".into()),
            ],
            Account::Dataverse(host) => vec![
                Step::Open(format!("https://{host}/dataverseuser.xhtml?selectTab=apiTokenTab")),
                Step::Do(format!("sign in to {host}, then create or copy the api token there; it works on that server only")),
            ],
            Account::Login(host) if host == EARTHDATA => vec![
                Step::Open("https://urs.earthdata.nasa.gov/users/new".into()),
                Step::Do("use your earthdata login username and password; create an account there if you have none".into()),
                Step::Do("some archives also need their application approved under applications in your earthdata profile".into()),
            ],
            Account::Login(host) if host == PHYSIONET => vec![
                Step::Open("https://physionet.org/settings/credentialing/".into()),
                Step::Do("use your physionet username and password".into()),
                Step::Do("credentialed datasets also need the training and a signed data use agreement on their own page".into()),
            ],
            Account::Login(host) => vec![Step::Do(format!("use the username and password {host} gave you"))],
        }
    }

    /// the tools that read the same key, so a person knows signing in here signs in there too.
    pub(crate) fn shared_with(&self) -> Option<&'static str> {
        match self {
            Account::HuggingFace => Some("the hf command and the hugging face python libraries"),
            Account::Kaggle => Some("the kaggle command and kagglehub"),
            Account::Login(_) => Some("curl, wget and python's requests"),
            _ => None,
        }
    }

    /// the header this account's key travels in, and the host it may go to.
    pub(crate) fn credential(&self, found: &Found) -> Option<Credential> {
        let token = found.secret.trim();
        let (host, header, value, every_hop) = match self {
            Account::HuggingFace => (
                "huggingface.co".to_owned(),
                "authorization",
                bearer(token),
                false,
            ),
            Account::Kaggle => {
                let (host, value) = crate::source::kaggle_pasted(token)?;
                // a legacy key goes with its username, as downloads send it
                let value = found
                    .login
                    .as_deref()
                    .map_or(value, |login| basic(login, token));
                (host.to_owned(), "authorization", value, false)
            }
            Account::Zenodo => (
                "zenodo.org".to_owned(),
                "authorization",
                bearer(token),
                false,
            ),
            Account::Figshare => (
                "api.figshare.com".to_owned(),
                "authorization",
                format!("token {token}"),
                false,
            ),
            Account::Osf => (
                "api.osf.io".to_owned(),
                "authorization",
                bearer(token),
                false,
            ),
            Account::Dataverse(host) => (host.clone(), "x-dataverse-key", token.to_owned(), false),
            // a login answers wherever its host is reached, as curl's netrc does: earthdata's
            // archives send a person to urs.earthdata.nasa.gov and back
            Account::Login(host) => {
                let login = found.login.as_deref()?;
                (
                    host.clone(),
                    "authorization",
                    basic(login, &found.secret),
                    true,
                )
            }
        };
        Some(Credential {
            host,
            header,
            value,
            every_hop,
        })
    }

    /// whether a request can carry this key. one with a line break or another character no
    /// header holds is left out of every request.
    pub(crate) fn sendable(&self, found: &Found) -> bool {
        self.credential(found).is_none_or(|credential| {
            reqwest::header::HeaderValue::from_str(&credential.value).is_ok()
        })
    }

    /// every host this account's key goes to. osf's download links sit on osf.io and send a
    /// person on to files.osf.io, both osf's own, which need the key for a private file.
    pub(crate) fn credentials(&self, found: &Found) -> Vec<Credential> {
        let Some(credential) = self.credential(found) else {
            return Vec::new();
        };
        let mut all = Vec::new();
        if *self == Account::Osf {
            for host in ["osf.io", "files.osf.io"] {
                all.push(Credential {
                    host: host.to_owned(),
                    every_hop: true,
                    ..credential.clone()
                });
            }
        }
        all.insert(0, credential);
        all
    }

    /// a request that proves a key works, where the source has one.
    pub(crate) fn check(&self) -> Option<Check> {
        let (url, who): (String, fn(&serde_json::Value) -> Option<String>) = match self {
            Account::HuggingFace => ("https://huggingface.co/api/whoami-v2".into(), |v| {
                text(&v["name"])
            }),
            Account::Zenodo => (
                "https://zenodo.org/api/deposit/depositions?size=1".into(),
                |_| None,
            ),
            Account::Figshare => ("https://api.figshare.com/v2/account".into(), |v| {
                text(&v["email"]).or_else(|| text(&v["first_name"]))
            }),
            Account::Osf => ("https://api.osf.io/v2/users/me/".into(), |v| {
                text(&v["data"]["attributes"]["full_name"])
            }),
            Account::Dataverse(host) => (format!("https://{host}/api/users/:me"), |v| {
                text(&v["data"]["displayName"]).or_else(|| text(&v["data"]["identifier"]))
            }),
            Account::Login(host) if host == EARTHDATA => (
                "https://urs.earthdata.nasa.gov/api/users/tokens".into(),
                |_| None,
            ),
            Account::Kaggle => ("https://api.kaggle.com/v1/hello".into(), |v| {
                text(&v["userName"])
            }),
            Account::Login(_) => return None,
        };
        Some(Check {
            url: Url::parse(&url).ok()?,
            who,
        })
    }

    /// the key on this machine, in the order the source's own tools look.
    pub(crate) fn find(&self, var: Var<'_>) -> Option<Found> {
        let set = |name: &'static str| {
            var(name)
                .and_then(|value| value.into_string().ok())
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(|secret| Found {
                    origin: Origin::Variable(name),
                    secret,
                    login: None,
                })
        };
        let file = |path: PathBuf| {
            let secret = std::fs::read_to_string(&path).ok()?.trim().to_owned();
            (!secret.is_empty()).then_some(Found {
                origin: Origin::File(path),
                secret,
                login: None,
            })
        };
        let stored = |id: &str| {
            let place = *store::saved(var).get(id)?;
            store::read(var, id).map(|secret| Found {
                origin: match place {
                    store::Place::Keyring => Origin::Keyring,
                    store::Place::File => Origin::File(
                        store::folder(var)
                            .unwrap_or_default()
                            .join("credentials.json"),
                    ),
                },
                secret,
                login: None,
            })
        };
        match self {
            Account::HuggingFace => set("HF_TOKEN").or_else(|| hf_file(var).and_then(file)),
            Account::Kaggle => crate::source::kaggle_key(var),
            Account::Zenodo => set("ZENODO_TOKEN").or_else(|| stored(&self.id())),
            Account::Figshare => set("FIGSHARE_TOKEN").or_else(|| stored(&self.id())),
            Account::Osf => set("OSF_TOKEN").or_else(|| stored(&self.id())),
            Account::Dataverse(host) => {
                let named = var("DATAVERSE_URL")
                    .and_then(|value| value.into_string().ok())
                    .and_then(|value| Url::parse(&value).ok())
                    .and_then(|url| installation(&url));
                (named.as_deref() == Some(host.as_str()))
                    .then(|| set("DATAVERSE_API_TOKEN"))
                    .flatten()
                    .or_else(|| stored(&self.id()))
            }
            Account::Login(host) => {
                let path = netrc::path(var)?;
                let entry = netrc::read(var)
                    .into_iter()
                    .find(|entry| &entry.host == host)?;
                Some(Found {
                    origin: Origin::File(path),
                    secret: entry.password,
                    login: Some(entry.login),
                })
            }
        }
    }

    /// where signing in through fetchloom saves this account's key: the file the source's own
    /// tools read, or for sources without one, the credential store.
    pub(crate) fn home(&self, var: Var<'_>, secret: &str) -> Home {
        match self {
            Account::HuggingFace => hf_file(var).map_or(Home::Store, Home::File),
            // the kaggle CLI reads kaggle.json from KAGGLE_CONFIG_DIR, but kagglesdk reads
            // access_token only from ~/.kaggle
            Account::Kaggle if secret.trim_start().starts_with('{') => {
                crate::source::kaggle_folder(var)
                    .map_or(Home::Store, |folder| Home::File(folder.join("kaggle.json")))
            }
            Account::Kaggle => crate::source::home(var).map_or(Home::Store, |home| {
                Home::File(home.join(".kaggle").join("access_token"))
            }),
            Account::Login(_) => netrc::path(var).map_or(Home::Store, Home::Netrc),
            _ => Home::Store,
        }
    }
}

/// where a key is saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Home {
    /// a file the source's own tools read
    File(PathBuf),
    /// the `.netrc` curl and wget read
    Netrc(PathBuf),
    /// the credential store, or the person's own file when the system has none
    Store,
}

/// every key fetchloom found, as the network sends them: each to its own host only.
pub(crate) fn credentials(var: Var<'_>) -> Vec<Credential> {
    let mut accounts = vec![
        Account::HuggingFace,
        Account::Zenodo,
        Account::Figshare,
        Account::Osf,
    ];
    accounts.extend(dataverses(var).into_iter().map(Account::Dataverse));
    let mut found: Vec<Credential> = accounts
        .iter()
        .filter_map(|account| Some(account.credentials(&account.find(var)?)))
        .flatten()
        .collect();
    // the file is read once here, rather than again for each login it holds
    let path = netrc::path(var);
    found.extend(netrc::read(var).into_iter().filter_map(|entry| {
        let account = Account::Login(entry.host);
        account.credential(&Found {
            origin: Origin::File(path.clone()?),
            secret: entry.password,
            login: Some(entry.login),
        })
    }));
    found.extend(crate::source::kaggle_credentials(var));
    found
}

/// every dataverse installation a key is known for.
pub(crate) fn dataverses(var: Var<'_>) -> Vec<String> {
    let mut hosts: Vec<String> = store::saved(var)
        .keys()
        .filter_map(|id| id.strip_prefix("dataverse:").and_then(dataverse_host))
        .collect();
    if let Some(host) = var("DATAVERSE_URL")
        .and_then(|value| value.into_string().ok())
        .and_then(|value| Url::parse(&value).ok())
        .and_then(|url| installation(&url))
        && !hosts.contains(&host)
    {
        hosts.push(host);
    }
    hosts
}

/// every account worth listing in `auth`: the sources, and each dataverse and login host
/// a key is known for.
pub(crate) fn listed(var: Var<'_>) -> Vec<Account> {
    let mut accounts = vec![
        Account::HuggingFace,
        Account::Kaggle,
        Account::Zenodo,
        Account::Figshare,
        Account::Osf,
    ];
    accounts.extend(dataverses(var).into_iter().map(Account::Dataverse));
    let mut logins: Vec<String> = netrc::read(var)
        .into_iter()
        .map(|entry| entry.host)
        .collect();
    for host in [EARTHDATA, PHYSIONET] {
        if !logins.iter().any(|known| known == host) {
            logins.push(host.to_owned());
        }
    }
    accounts.extend(logins.into_iter().map(Account::Login));
    accounts
}

/// the token file the hugging face tools read: `HF_TOKEN_PATH`, else `token` in `HF_HOME`, else
/// in the hugging face folder of the user's cache.
pub(crate) fn hf_file(var: Var<'_>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    set("HF_TOKEN_PATH")
        .map(PathBuf::from)
        .or_else(|| set("HF_HOME").map(|home| PathBuf::from(home).join("token")))
        .or_else(|| {
            set("XDG_CACHE_HOME")
                .map(|cache| PathBuf::from(cache).join("huggingface").join("token"))
        })
        .or_else(|| {
            crate::source::home(var)
                .map(|home| home.join(".cache").join("huggingface").join("token"))
        })
}

/// whether a host is one word, since .netrc and the key index split on spaces and line breaks.
fn one_word(host: &str) -> bool {
    !host.is_empty() && !host.contains(|c: char| c.is_whitespace() || c.is_control() || c == '"')
}

/// a dataverse installation as its account names it, from what a person typed: the host, with
/// its port when that is not 443. none for anything else, such as a login, a path or a query.
pub(crate) fn dataverse_host(typed: &str) -> Option<String> {
    let typed = typed.trim();
    // the URL parser drops tabs and line breaks, which would join two words into one host
    if !one_word(typed) {
        return None;
    }
    let bare = typed
        .strip_prefix("https://")
        .or_else(|| typed.strip_prefix("http://"))
        .unwrap_or(typed);
    let url = Url::parse(&format!("https://{bare}")).ok()?;
    let plain = url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    if !plain {
        return None;
    }
    installation(&url)
}

/// the installation a dataverse URL is on: its host, with its port when that is not 443.
fn installation(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    Some(match url.port().filter(|port| *port != 443) {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

pub(crate) fn basic(login: &str, password: &str) -> String {
    format!(
        "Basic {}",
        crate::source::base64(format!("{login}:{password}").as_bytes())
    )
}

fn text(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// the environment of this process, the one `Var` most callers want.
pub(crate) fn env(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

#[cfg(test)]
mod tests;

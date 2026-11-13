//! accounts at the sources: which key each takes, where it is kept, how to check it and how a
//! person gets one. a key lives where the source's own tools keep it, so one key serves both.

pub(crate) mod aws;
pub(crate) mod azure;
pub(crate) mod google;
pub(crate) mod lease;
pub(crate) mod netrc;
pub(crate) mod process;
pub(crate) mod store;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use url::Url;

use crate::error::{Error, Kind};
use crate::net::Credential;
use crate::source::Var;

/// one step of a sign-in guide.
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
    Dryad,
    GitHub,
    GitLab,
    /// semantic scholar's API: its key gives a rate of its own, and the files of its datasets.
    S2,
    /// one installation's host, with its port when that is not 443: a dataverse key works only
    /// on the server that made it.
    Dataverse(String),
    /// a username and password for one host, kept in `.netrc`, as for nasa earthdata or physionet.
    Login(String),
}

const EARTHDATA: &str = "urs.earthdata.nasa.gov";
const PHYSIONET: &str = "physionet.org";
const HARVARD: &str = "dataverse.harvard.edu";
/// semantic scholar's API, the only host its key goes to.
pub(crate) const S2_HOST: &str = "api.semanticscholar.org";
/// where semantic scholar gives a person a key.
pub(crate) const S2_KEYS: &str = "https://www.semanticscholar.org/product/api#api-key-form";

/// what a person types after `fetchloom auth login`.
pub(crate) const NAMES: &str = "hf, kaggle, zenodo, figshare, osf, dryad, github, gitlab, s2, dataverse, earthdata, physionet or a host name";

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

impl Origin {
    /// where the key comes from as a line names it, a file below the home folder as `~/…`.
    pub(crate) fn shown(&self) -> String {
        match self {
            Origin::File(path) => crate::units::path(path),
            origin => origin.to_string(),
        }
    }

    /// the key as messages name it, by where it comes from and never by its value: `the key in
    /// HF_TOKEN`, or `the key saved in ~/.cache/huggingface/token`.
    pub(crate) fn named(&self) -> String {
        match self {
            Origin::Variable(name) => format!("the key in {name}"),
            origin => format!("the key saved in {}", origin.shown()),
        }
    }
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
    /// the token, or for a login the username and password.
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
            "dryad" | "datadryad.org" => Account::Dryad,
            "github" | "github.com" => Account::GitHub,
            "gitlab" | "gitlab.com" => Account::GitLab,
            "s2" | "semanticscholar" | "semantic-scholar" | S2_HOST => Account::S2,
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
                && dataverse_host(other).is_some() => {
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
            "datadryad.org" => Account::Dryad,
            "api.github.com" => Account::GitHub,
            "gitlab.com" => Account::GitLab,
            S2_HOST => Account::S2,
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
            Account::Dryad => "dryad".to_owned(),
            Account::GitHub => "github".to_owned(),
            Account::GitLab => "gitlab".to_owned(),
            Account::S2 => "s2".to_owned(),
            Account::Dataverse(host) => format!("dataverse:{host}"),
            Account::Login(host) => format!("login:{host}"),
        }
    }

    /// the command that signs in to this account, its host quoted for a shell, since a link or a
    /// server's answer named it.
    pub(crate) fn command(&self) -> String {
        let quoted = |word: &str| crate::suggested::shell_word(word).into_owned();
        let named = match self {
            Account::Dataverse(host) if host == HARVARD => "dataverse".to_owned(),
            Account::Dataverse(host) => format!("dataverse --host {}", quoted(host)),
            Account::Login(host) if host == EARTHDATA => "earthdata".to_owned(),
            Account::Login(host) if host == PHYSIONET => "physionet".to_owned(),
            Account::Login(host) => quoted(host),
            other => quoted(&other.id()),
        };
        format!("fetchloom auth login {named}")
    }

    pub(crate) fn secret(&self) -> Secret {
        match self {
            Account::Login(_) => Secret::Password,
            _ => Secret::Token,
        }
    }

    /// how a person gets a key, as steps in order, from making an account to accepting terms.
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
            Account::Dryad => vec![
                Step::Do("sign in at datadryad.org with your ORCID; dryad makes the account then".into()),
                Step::Open("https://datadryad.org/account".into()),
                Step::Do("make an API account there if you have none, then get a token; it lasts 10 hours".into()),
            ],
            Account::GitHub => vec![
                join("github.com"),
                Step::Open("https://github.com/settings/personal-access-tokens/new".into()),
                Step::Do("create a fine-grained token: public repositories need no permissions, a private one read access to its contents".into()),
            ],
            Account::GitLab => vec![
                join("gitlab.com"),
                Step::Open("https://gitlab.com/-/user_settings/personal_access_tokens".into()),
                Step::Do("add a token with the read_api scope".into()),
            ],
            Account::S2 => vec![
                Step::Open(S2_KEYS.into()),
                Step::Do("ask for a key with the form there; semantic scholar sends it by email".into()),
                Step::Do("papers and search work without one, at a rate everyone shares; the files of its datasets need it".into()),
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
                let (host, value) = crate::source::kaggle::pasted(token)?;
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
            Account::Dryad => (
                "datadryad.org".to_owned(),
                "authorization",
                bearer(token),
                false,
            ),
            // only the API, never github.com or the storage a download is sent on to
            Account::GitHub => (
                "api.github.com".to_owned(),
                "authorization",
                bearer(token),
                false,
            ),
            Account::GitLab => (
                "gitlab.com".to_owned(),
                "authorization",
                bearer(token),
                false,
            ),
            // the API alone: the links it gives to its files on S3 carry their own signature
            Account::S2 => (S2_HOST.to_owned(), "x-api-key", token.to_owned(), false),
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
            // any key may read the limits, an installation's key from a CI run too
            Account::GitHub => ("https://api.github.com/rate_limit".into(), |_| None),
            Account::GitLab => ("https://gitlab.com/api/v4/user".into(), |v| {
                text(&v["username"])
            }),
            Account::Dryad => ("https://datadryad.org/api/v2/test".into(), |v| {
                text(&v["message"]).map(|said| {
                    said.trim_start_matches("Welcome application owner ")
                        .to_owned()
                })
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
            // any request with a key it does not know is refused with 403, and the answer names
            // nobody
            Account::S2 => (
                format!("https://{S2_HOST}/datasets/v1/release/latest"),
                |_| None,
            ),
            Account::Login(_) => return None,
        };
        Some(Check {
            url: Url::parse(&url).ok()?,
            who,
        })
    }

    /// the key on this machine, in the order the source's own tools look.
    pub(crate) fn find(&self, var: Var<'_>) -> Option<Found> {
        self.find_in(var, &store::saved(var))
    }

    /// [`Account::find`], with `saved` the index of keys saved through fetchloom, read already.
    fn find_in(&self, var: Var<'_>, saved: &BTreeMap<String, store::Place>) -> Option<Found> {
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
            let place = *saved.get(id)?;
            store::read(var, id, place).map(|secret| Found {
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
            Account::Kaggle => crate::source::kaggle::locate(var),
            Account::Zenodo => set("ZENODO_TOKEN").or_else(|| stored(&self.id())),
            Account::Figshare => set("FIGSHARE_TOKEN").or_else(|| stored(&self.id())),
            Account::Osf => set("OSF_TOKEN").or_else(|| stored(&self.id())),
            Account::Dryad => set("DRYAD_TOKEN").or_else(|| stored(&self.id())),
            Account::GitHub => set("GH_TOKEN")
                .or_else(|| set("GITHUB_TOKEN"))
                .or_else(|| stored(&self.id())),
            Account::GitLab => set("GITLAB_TOKEN").or_else(|| stored(&self.id())),
            Account::S2 => set("S2_API_KEY").or_else(|| stored(&self.id())),
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
                crate::source::kaggle::config_folder(var)
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
    /// a file the source's own tools read.
    File(PathBuf),
    /// the `.netrc` curl and wget read.
    Netrc(PathBuf),
    /// the credential store, or the person's own file when the system has none.
    Store,
}

/// every key fetchloom found, as the network sends them: each to its own host only.
pub(crate) fn credentials(var: Var<'_>) -> Vec<Credential> {
    let mut accounts = vec![
        Account::HuggingFace,
        Account::Zenodo,
        Account::Figshare,
        Account::Osf,
        Account::Dryad,
        Account::GitHub,
        Account::GitLab,
        Account::S2,
    ];
    // the index of saved keys is read once here, rather than again for each account
    let saved = store::saved(var);
    accounts.extend(
        saved_dataverses(var, &saved)
            .into_iter()
            .map(Account::Dataverse),
    );
    let mut found: Vec<Credential> = accounts
        .iter()
        .filter_map(|account| Some(account.credentials(&account.find_in(var, &saved)?)))
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
    found.extend(crate::source::kaggle::credentials(var));
    found
}

/// every dataverse installation a key is known for.
pub(crate) fn dataverses(var: Var<'_>) -> Vec<String> {
    saved_dataverses(var, &store::saved(var))
}

/// [`dataverses`], with `saved` the index of keys saved through fetchloom, read already.
fn saved_dataverses(var: Var<'_>, saved: &BTreeMap<String, store::Place>) -> Vec<String> {
    let mut hosts: Vec<String> = saved
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
        Account::Dryad,
        Account::GitHub,
        Account::GitLab,
        Account::S2,
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

/// the token file the hugging face tools read, found as they find it.
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

/// the installation a dataverse URL is on: its host, with its port when that is not 443. none
/// for a host with other characters than letters, digits, dots, hyphens and underscores, such as
/// the `$(` a shell reads.
fn installation(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    let named = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_');
    if matches!(url.host(), Some(url::Host::Domain(_))) && !host.chars().all(named) {
        return None;
    }
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

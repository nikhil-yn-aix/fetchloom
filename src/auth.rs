//! accounts at the sources: which key each takes, where it is kept, how to check it and how a
//! person gets one. a key lives where the source's own tools keep it, so one key serves both.

pub(crate) mod aws;
pub(crate) mod azure;
pub(crate) mod cookies;
pub(crate) mod google;
pub(crate) mod kaggle;
pub(crate) mod lease;
pub(crate) mod netrc;
pub(crate) mod oauth;
pub(crate) mod process;
pub(crate) mod site;
pub(crate) mod store;

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use url::Url;

use crate::account::Account;
use crate::error::{Error, Kind};
use crate::machine::Var;

/// a token from where its source keeps it, sent only to its own host, at its own port unless
/// `every_port` says otherwise, over https, or over plain http only to this machine or an origin
/// `allowed_origins` lists, with no proxy on the way.
#[derive(Clone)]
pub(crate) struct Credential {
    /// the host, with `:port` when the key belongs to another port than 443.
    pub(crate) host: String,
    pub(crate) header: &'static str,
    pub(crate) value: String,
    /// sent on every hop that reaches its host, as curl sends a `.netrc` login, rather than only
    /// before a redirect leaves the first origin. only a login a person tied to a host gets this.
    pub(crate) every_hop: bool,
    /// sent to every port of its host, as curl, wget and python's requests send the login of a
    /// `.netrc` line that names no port. a key for the very port a request goes to wins over it.
    pub(crate) every_port: bool,
}

/// one step of a sign-in guide.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Open(String),
    Do(String),
}

const EARTHDATA: &str = "urs.earthdata.nasa.gov";
const PHYSIONET: &str = "physionet.org";
/// semantic scholar's API, the only host its key goes to.
pub(crate) const S2_HOST: &str = "api.semanticscholar.org";
/// where semantic scholar gives a person a key.
pub(crate) const S2_KEYS: &str = "https://www.semanticscholar.org/product/api#api-key-form";

/// what a person types after `fetchloom auth login`.
pub(crate) const NAMES: &str = "hf, kaggle, zenodo, figshare, osf, dryad, github, gitlab, s2, earthdata, physionet or a host name";

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

    /// where a key fetchloom saved in `place` is.
    pub(crate) fn saved(var: Var<'_>, place: store::Place) -> Origin {
        match place {
            store::Place::Keyring => Origin::Keyring,
            store::Place::File => Origin::File(
                crate::machine::settings_folder(var)
                    .unwrap_or_default()
                    .join("credentials.json"),
            ),
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

/// the refusal to sign in to `name` when it is a cloud whose own tool signs in, which fetchloom
/// reads rather than keeping a key of its own.
fn cloud_sign_in(name: &str) -> Option<Error> {
    let (signs_in, reads, then) = match name {
        "aws" => (
            "the AWS CLI",
            "reads its profile",
            "run `aws sso login` or `aws configure`, then list your private buckets in the s3_buckets setting",
        ),
        "google" | "gcloud" => (
            "the gcloud CLI",
            "reads its application default credentials",
            "run `gcloud auth application-default login`, then list your private buckets in the gcs_buckets setting",
        ),
        "azure" | "az" => (
            "the az CLI",
            "asks it for a token",
            "run `az login`, then list your private containers in the azure_containers setting",
        ),
        _ => return None,
    };
    Some(
        Error::new(
            Kind::Usage,
            format!("{name} signs in with {signs_in}, and fetchloom {reads}"),
        )
        .help(then),
    )
}

/// the refusal to sign in to `name` when it is a source whose keys each belong to one
/// installation, as dataverse's do, which a person signs in to by its host.
fn per_host_sign_in(name: &str) -> Option<Error> {
    let key = crate::account::PER_HOST_KEYS
        .into_iter()
        .find(|key| key.name == name)?;
    Some(
        Error::new(
            Kind::Usage,
            format!("a {name} key belongs to one installation, so you sign in to its host"),
        )
        .help(format!(
            "run `fetchloom auth login <host> --header {}` with your installation's host, as in `fetchloom auth login demo.dataverse.org --header {}`",
            key.header, key.header
        )),
    )
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
    /// the account `name` means.
    pub(crate) fn parse(name: &str) -> Result<Account, Error> {
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
            "earthdata" | "nasa" | EARTHDATA => Account::Login(EARTHDATA.to_owned()),
            "physionet" | PHYSIONET => Account::Login(PHYSIONET.to_owned()),
            // a login for another port than 443 names it, as a website's own key does
            other
                if (other.contains('.') || other.split(':').next() == Some("localhost"))
                    && one_word(other)
                    && !other.contains(['/', '\\', '@', '?', '#', '%'])
                    && typed_host(other).is_some() =>
            {
                Account::Login(typed_host(other).unwrap_or_else(|| other.to_owned()))
            }
            _ => {
                if let Some(refused) =
                    cloud_sign_in(&lowered).or_else(|| per_host_sign_in(&lowered))
                {
                    return Err(refused);
                }
                let error = Error::new(
                    Kind::Usage,
                    format!(
                        "fetchloom does not know an account called \"{}\"",
                        crate::shown::mask_typed(name).escape_debug()
                    ),
                );
                return Err(match meant_account(name) {
                    Some(meant) => error.help(format!("did you mean `{}`?", meant.command())),
                    None => error.help(format!(
                        "use one of {NAMES}, as in `fetchloom auth login hf`"
                    )),
                });
            }
        };
        Ok(account)
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
            Account::Login(host) => format!("login:{host}"),
        }
    }

    /// the command that signs in to this account, its host quoted for a shell, since a link or a
    /// server's answer named it.
    pub(crate) fn command(&self) -> String {
        let quoted = |word: &str| crate::suggested::shell_word(word).into_owned();
        let named = match self {
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
    /// a login kept to a port other than 443 has none: curl, wget and python's requests look a
    /// `.netrc` line up by host name alone, so they never read one that names a port.
    pub(crate) fn shared_with(&self) -> Option<&'static str> {
        match self {
            Account::HuggingFace => Some("the hf command and the hugging face python libraries"),
            Account::Kaggle => Some("the kaggle command and kagglehub"),
            Account::Login(host) if !names_a_port(host) => Some("curl, wget and python's requests"),
            _ => None,
        }
    }

    /// what a sign-in that saved this account's key says of the other tools that read it: that
    /// they use it too, or that they skip a login kept to one port.
    pub(crate) fn told_shared(&self) -> Option<String> {
        match (self, self.shared_with()) {
            (_, Some(tools)) => Some(format!("{tools} use this key too")),
            (Account::Login(_), None) => Some(
                "curl, wget and python's requests skip a .netrc line with a port, so only fetchloom uses this login"
                    .to_owned(),
            ),
            _ => None,
        }
    }

    /// the header this account's key travels in, and the host it may go to, as its key's shape
    /// names them.
    pub(crate) fn credential(&self, found: &Found) -> Option<Credential> {
        let token = found.secret.trim();
        let (host, header, value, every_hop) = match self {
            Account::Kaggle => {
                let (host, value) = kaggle::pasted(token)?;
                // a legacy key goes with its username, as downloads send it
                let value = found
                    .login
                    .as_deref()
                    .map_or(value, |login| basic(login, token));
                (host.to_owned(), "authorization", value, false)
            }
            // a login answers its host on every hop, and every port of it from a line without
            // one, as curl reads netrc: earthdata sends a person through urs.earthdata.nasa.gov
            Account::Login(host) => {
                let login = found.login.as_deref()?;
                (
                    host.clone(),
                    "authorization",
                    basic(login, &found.secret),
                    true,
                )
            }
            source => {
                let key = source.key()?;
                (key.host?.to_owned(), key.header, (key.value)(token), false)
            }
        };
        let every_port = matches!(self, Account::Login(host) if !names_a_port(host));
        Some(Credential {
            host,
            header,
            value,
            every_hop,
            every_port,
        })
    }

    /// whether a request can carry this key. one with a line break or another character no
    /// header holds is left out of every request.
    pub(crate) fn sendable(&self, found: &Found) -> bool {
        self.credential(found).is_none_or(|credential| {
            reqwest::header::HeaderValue::from_str(&credential.value).is_ok()
        })
    }

    /// every host this account's key goes to: its own, and the hosts its key's shape sends it to
    /// on every hop, as osf's download hosts.
    pub(crate) fn credentials(&self, found: &Found) -> Vec<Credential> {
        let Some(credential) = self.credential(found) else {
            return Vec::new();
        };
        let every_hop = self.key().map_or(&[][..], |key| key.every_hop);
        let mut all = vec![credential.clone()];
        all.extend(every_hop.iter().map(|host| Credential {
            host: (*host).to_owned(),
            every_hop: true,
            ..credential.clone()
        }));
        all
    }

    /// a request that proves a key works, where the source has one, at the path its key's shape
    /// names, and how to read who the key belongs to.
    pub(crate) fn check(&self) -> Option<Check> {
        let who = self.key().map_or(
            (|_| None) as fn(&serde_json::Value) -> Option<String>,
            |key| key.who,
        );
        let url = match self {
            Account::Login(host) if host == EARTHDATA => {
                "https://urs.earthdata.nasa.gov/api/users/tokens".to_owned()
            }
            Account::Login(_) => return None,
            source => {
                let key = source.key()?;
                format!("https://{}{}", key.host?, key.check?)
            }
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
            crate::machine::set(var, name).map(|secret| Found {
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
                origin: Origin::saved(var, place),
                secret,
                login: None,
            })
        };
        let named = || {
            self.key()
                .and_then(|key| key.env.iter().find_map(|name| set(name)))
        };
        match self {
            Account::HuggingFace => named().or_else(|| hf_file(var).and_then(file)),
            Account::Kaggle => kaggle::locate(var),
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
            _ => named().or_else(|| stored(&self.id())),
        }
    }

    /// where signing in through fetchloom saves this account's key: the file the source's own
    /// tools read, or for sources without one, the credential store.
    pub(crate) fn home(&self, var: Var<'_>, secret: &str) -> Home {
        match self {
            Account::HuggingFace => hf_file(var).map_or(Home::Store, Home::File),
            // the kaggle CLI reads kaggle.json from KAGGLE_CONFIG_DIR, but kagglesdk reads
            // access_token only from ~/.kaggle
            Account::Kaggle if secret.trim_start().starts_with('{') => kaggle::config_folder(var)
                .map_or(Home::Store, |folder| Home::File(folder.join("kaggle.json"))),
            Account::Kaggle => crate::machine::home(var).map_or(Home::Store, |home| {
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
    // kaggle's keys are read below, a legacy key with its username
    let accounts: Vec<Account> = Account::SOURCES
        .into_iter()
        .filter(|account| *account != Account::Kaggle)
        .collect();
    // the index of saved keys is read once here, rather than again for each account
    let saved = store::saved(var);
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
    found.extend(kaggle::credentials(var));
    found
}

/// every account worth listing in `auth`/// every account worth listing in `auth`: the sources, and each login host a key is known for.
pub(crate) fn listed(var: Var<'_>) -> Vec<Account> {
    let mut accounts = Account::SOURCES.to_vec();
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
fn hf_file(var: Var<'_>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    set("HF_TOKEN_PATH")
        .map(PathBuf::from)
        .or_else(|| set("HF_HOME").map(|home| PathBuf::from(home).join("token")))
        .or_else(|| {
            set("XDG_CACHE_HOME")
                .map(|cache| PathBuf::from(cache).join("huggingface").join("token"))
        })
        .or_else(|| {
            crate::machine::home(var)
                .map(|home| home.join(".cache").join("huggingface").join("token"))
        })
}

/// whether a login's host names its port, as `lab.example.org:8443` does.
fn names_a_port(host: &str) -> bool {
    site::without_port(host) != host
}

/// whether a host is one word, since .netrc and the key index split on spaces and line breaks.
fn one_word(host: &str) -> bool {
    !host.is_empty() && !host.contains(|c: char| c.is_whitespace() || c.is_control() || c == '"')
}

/// the names a person types for each account, which [`meant_account`] holds a typo up to. hosts
/// are left out, since a typo of one may be another host, and so is nasa, a typo away from `nas`.
const TYPED_NAMES: [&str; 13] = [
    "hf",
    "huggingface",
    "kaggle",
    "zenodo",
    "figshare",
    "osf",
    "dryad",
    "github",
    "gitlab",
    "s2",
    "semanticscholar",
    "earthdata",
    "physionet",
];

/// the account whose name `typed` is a typo of, as `hff` is of hf. a word shorter than three
/// letters is too short to tell a typo by, and one with a dot or a port names a host.
pub(crate) fn meant_account(typed: &str) -> Option<Account> {
    let lowered = typed.trim().to_ascii_lowercase();
    if lowered.chars().count() < 3 || lowered.contains(['.', ':']) {
        return None;
    }
    let name = crate::name::sole_closest(&lowered, TYPED_NAMES)?;
    Account::parse(name).ok()
}

/// a host as a key saved for it names it, from what a person typed: the host, with its port when
/// that is not 443. none for anything else, such as a login, a path or a query.
pub(crate) fn typed_host(typed: &str) -> Option<String> {
    let typed = typed.trim();
    // the URL parser drops tabs and line breaks, which would join two words into one host
    if !one_word(typed) {
        return None;
    }
    let bare = typed
        .strip_prefix("https://")
        .or_else(|| typed.strip_prefix("http://"))
        .unwrap_or(typed);
    installation(&plain_host(&format!("https://{bare}"))?)
}

/// `url` parsed, when it names a host, with its port, and nothing else: no login, path, query
/// or fragment.
pub(crate) fn plain_host(url: &str) -> Option<Url> {
    let url = Url::parse(url).ok()?;
    let plain = url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    plain.then_some(url)
}

/// the host a URL is at, with its port when that is not 443, as an installation's key names it.
/// none for a host with other characters than letters, digits, dots, hyphens and underscores,
/// such as the `$(` a shell reads.
pub(crate) fn installation(url: &Url) -> Option<String> {
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

pub(crate) fn basic(login: &str, password: &str) -> String {
    format!("Basic {}", base64(format!("{login}:{password}").as_bytes()))
}

/// standard base64 with padding, which basic authentication sends.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|at| chunk.get(at).copied().unwrap_or(0));
        let sextets = [
            a >> 2,
            ((a & 0b11) << 4) | (b >> 4),
            ((b & 0b1111) << 2) | (c >> 6),
            c & 0b11_1111,
        ];
        for (at, sextet) in sextets.into_iter().enumerate() {
            text.push(if at <= chunk.len() {
                char::from(ALPHABET[usize::from(sextet)])
            } else {
                '='
            });
        }
    }
    text
}

/// the bytes `text` writes in base64, with the standard alphabet and its padding.
pub(crate) fn from_base64(text: &str) -> Option<Vec<u8>> {
    let digits = text.trim_end_matches('=');
    if text.is_empty() || !text.len().is_multiple_of(4) || text.len() - digits.len() > 2 {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let (mut bits, mut held) = (0_u32, 0);
    for byte in digits.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push(u8::try_from(bits >> held).ok()?);
            bits &= (1 << held) - 1;
        }
    }
    // the bits past the last whole byte must be zero, as an encoder leaves them
    (bits == 0).then_some(bytes)
}

#[cfg(test)]
mod tests;

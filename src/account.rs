//! an account at one source, or a login for one host: the plain data a refusal names and a key
//! belongs to. [`crate::auth`] finds, checks and names the keys of each.

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
    /// a username and password for one host, kept in `.netrc`, as for nasa earthdata or physionet.
    Login(String),
}

/// the key a source takes: the host it goes to, the header it goes in and how its value is made
/// from the key. a source's access descriptor names it, so its key goes nowhere else.
pub(crate) struct KeyShape {
    /// the source as messages name it, as `dataverse`.
    pub(crate) name: &'static str,
    /// the host it goes to: the source's api, or none for a key of one installation, as a
    /// dataverse key is, which is saved as that host's key in the header the source names.
    pub(crate) host: Option<&'static str>,
    pub(crate) header: &'static str,
    pub(crate) value: fn(&str) -> String,
    /// the variables that hold it, in the order they are read.
    pub(crate) env: &'static [&'static str],
    /// for a key of one installation, the variable that names the installation the key in `env`
    /// is for, as `DATAVERSE_URL` does.
    pub(crate) env_host: Option<&'static str>,
    /// hosts the key goes to on every hop, besides its own.
    pub(crate) every_hop: &'static [&'static str],
    /// the request on its host that says whose key it is, and how to read who from its answer.
    pub(crate) check: Option<&'static str>,
    pub(crate) who: fn(&serde_json::Value) -> Option<String>,
    /// for a key of one installation, the page on its host where a person makes one.
    pub(crate) page: Option<&'static str>,
}

/// the text of a JSON string, without the spaces around it, when it holds any.
fn text(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// an answer that names nobody.
fn nobody(_: &serde_json::Value) -> Option<String> {
    None
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn raw(token: &str) -> String {
    token.to_owned()
}

pub(crate) static HUGGING_FACE_KEY: KeyShape = KeyShape {
    name: "hugging face",
    host: Some("huggingface.co"),
    header: "authorization",
    value: bearer,
    env: &["HF_TOKEN"],
    env_host: None,
    every_hop: &[],
    check: Some("/api/whoami-v2"),
    who: |v| text(&v["name"]),
    page: None,
};

/// a legacy key goes with its username, so kaggle's own reading makes the value.
pub(crate) static KAGGLE_KEY: KeyShape = KeyShape {
    name: "kaggle",
    host: Some("api.kaggle.com"),
    header: "authorization",
    value: bearer,
    env: &["KAGGLE_API_TOKEN"],
    env_host: None,
    every_hop: &[],
    check: Some("/v1/hello"),
    who: |v| text(&v["userName"]),
    page: None,
};

pub(crate) static ZENODO_KEY: KeyShape = KeyShape {
    name: "zenodo",
    host: Some("zenodo.org"),
    header: "authorization",
    value: bearer,
    env: &["ZENODO_TOKEN"],
    env_host: None,
    every_hop: &[],
    check: Some("/api/deposit/depositions?size=1"),
    who: nobody,
    page: None,
};

pub(crate) static FIGSHARE_KEY: KeyShape = KeyShape {
    name: "figshare",
    host: Some("api.figshare.com"),
    header: "authorization",
    value: |token| format!("token {token}"),
    env: &["FIGSHARE_TOKEN"],
    env_host: None,
    every_hop: &[],
    check: Some("/v2/account"),
    who: |v| text(&v["email"]).or_else(|| text(&v["first_name"])),
    page: None,
};

/// osf's download links sit on osf.io and send a person on to files.osf.io, both osf's own,
/// which need the key for a private file.
pub(crate) static OSF_KEY: KeyShape = KeyShape {
    name: "osf",
    host: Some("api.osf.io"),
    header: "authorization",
    value: bearer,
    env: &["OSF_TOKEN"],
    env_host: None,
    every_hop: &["osf.io", "files.osf.io"],
    check: Some("/v2/users/me/"),
    who: |v| text(&v["data"]["attributes"]["full_name"]),
    page: None,
};

pub(crate) static DRYAD_KEY: KeyShape = KeyShape {
    name: "dryad",
    host: Some("datadryad.org"),
    header: "authorization",
    value: bearer,
    env: &["DRYAD_TOKEN"],
    env_host: None,
    every_hop: &[],
    check: Some("/api/v2/test"),
    who: |v| {
        text(&v["message"]).map(|said| {
            said.trim_start_matches("Welcome application owner ")
                .to_owned()
        })
    },
    page: None,
};

/// only the API, never github.com or the storage a download is sent on to. any key may read
/// the limits, an installation's key from a CI run too.
pub(crate) static GITHUB_KEY: KeyShape = KeyShape {
    name: "github",
    host: Some("api.github.com"),
    header: "authorization",
    value: bearer,
    env: &["GH_TOKEN", "GITHUB_TOKEN"],
    env_host: None,
    every_hop: &[],
    check: Some("/rate_limit"),
    who: nobody,
    page: None,
};

pub(crate) static GITLAB_KEY: KeyShape = KeyShape {
    name: "gitlab",
    host: Some("gitlab.com"),
    header: "authorization",
    value: bearer,
    env: &["GITLAB_TOKEN"],
    env_host: None,
    every_hop: &[],
    check: Some("/api/v4/user"),
    who: |v| text(&v["username"]),
    page: None,
};

/// the API alone: the links it gives to its files on S3 carry their own signature. any request
/// with a key it does not know is refused with 403.
pub(crate) static S2_KEY: KeyShape = KeyShape {
    name: "semantic scholar",
    host: Some("api.semanticscholar.org"),
    header: "x-api-key",
    value: raw,
    env: &["S2_API_KEY"],
    env_host: None,
    every_hop: &[],
    check: Some("/datasets/v1/release/latest"),
    who: nobody,
    page: None,
};

/// a key of one dataverse installation, which works on the server that made it alone.
/// `DATAVERSE_API_TOKEN` counts only for the installation `DATAVERSE_URL` names.
pub(crate) static DATAVERSE_KEY: KeyShape = KeyShape {
    name: "dataverse",
    host: None,
    header: "x-dataverse-key",
    value: raw,
    env: &["DATAVERSE_API_TOKEN"],
    env_host: Some("DATAVERSE_URL"),
    every_hop: &[],
    check: Some("/api/users/:me"),
    who: |v| text(&v["data"]["displayName"]).or_else(|| text(&v["data"]["identifier"])),
    page: Some("/dataverseuser.xhtml?selectTab=apiTokenTab"),
};

/// the keys of one installation, each saved as its host's key in the header its source names.
pub(crate) static PER_HOST_KEYS: [&KeyShape; 1] = [&DATAVERSE_KEY];

/// the key of one installation whose source names `header`, as `x-dataverse-key` is dataverse's,
/// whatever its case.
pub(crate) fn per_host_key(header: &str) -> Option<&'static KeyShape> {
    PER_HOST_KEYS
        .into_iter()
        .find(|key| key.header.eq_ignore_ascii_case(header.trim()))
}

impl Account {
    /// the key this account's source takes, for a source's own account; none for a login, which
    /// goes as a username and password.
    pub(crate) fn key(&self) -> Option<&'static KeyShape> {
        Some(match self {
            Account::HuggingFace => &HUGGING_FACE_KEY,
            Account::Kaggle => &KAGGLE_KEY,
            Account::Zenodo => &ZENODO_KEY,
            Account::Figshare => &FIGSHARE_KEY,
            Account::Osf => &OSF_KEY,
            Account::Dryad => &DRYAD_KEY,
            Account::GitHub => &GITHUB_KEY,
            Account::GitLab => &GITLAB_KEY,
            Account::S2 => &S2_KEY,
            Account::Login(_) => return None,
        })
    }

    /// the sources' own accounts, each with the key its source takes.
    pub(crate) const SOURCES: [Account; 9] = [
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
}

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
    /// one installation's host, with its port when that is not 443: a dataverse key works only
    /// on the server that made it.
    Dataverse(String),
    /// a username and password for one host, kept in `.netrc`, as for nasa earthdata or physionet.
    Login(String),
}

/// the key a source takes: the host it goes to, the header it goes in and how its value is made
/// from the key. a source's access descriptor names it, so its key goes nowhere else.
pub(crate) struct KeyShape {
    /// the host it goes to: the source's api, or none where it is the installation's own, as a
    /// dataverse key's is.
    pub(crate) host: Option<&'static str>,
    pub(crate) header: &'static str,
    pub(crate) value: fn(&str) -> String,
    /// the variables that hold it, in the order they are read.
    pub(crate) env: &'static [&'static str],
    /// hosts the key goes to on every hop, besides its own.
    pub(crate) every_hop: &'static [&'static str],
    /// the request on its host that says whose key it is.
    pub(crate) check: Option<&'static str>,
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn raw(token: &str) -> String {
    token.to_owned()
}

pub(crate) static HUGGING_FACE_KEY: KeyShape = KeyShape {
    host: Some("huggingface.co"),
    header: "authorization",
    value: bearer,
    env: &["HF_TOKEN"],
    every_hop: &[],
    check: Some("/api/whoami-v2"),
};

/// a legacy key goes with its username, so kaggle's own reading makes the value.
pub(crate) static KAGGLE_KEY: KeyShape = KeyShape {
    host: Some("api.kaggle.com"),
    header: "authorization",
    value: bearer,
    env: &["KAGGLE_API_TOKEN"],
    every_hop: &[],
    check: Some("/v1/hello"),
};

pub(crate) static ZENODO_KEY: KeyShape = KeyShape {
    host: Some("zenodo.org"),
    header: "authorization",
    value: bearer,
    env: &["ZENODO_TOKEN"],
    every_hop: &[],
    check: Some("/api/deposit/depositions?size=1"),
};

pub(crate) static FIGSHARE_KEY: KeyShape = KeyShape {
    host: Some("api.figshare.com"),
    header: "authorization",
    value: |token| format!("token {token}"),
    env: &["FIGSHARE_TOKEN"],
    every_hop: &[],
    check: Some("/v2/account"),
};

/// osf's download links sit on osf.io and send a person on to files.osf.io, both osf's own,
/// which need the key for a private file.
pub(crate) static OSF_KEY: KeyShape = KeyShape {
    host: Some("api.osf.io"),
    header: "authorization",
    value: bearer,
    env: &["OSF_TOKEN"],
    every_hop: &["osf.io", "files.osf.io"],
    check: Some("/v2/users/me/"),
};

pub(crate) static DRYAD_KEY: KeyShape = KeyShape {
    host: Some("datadryad.org"),
    header: "authorization",
    value: bearer,
    env: &["DRYAD_TOKEN"],
    every_hop: &[],
    check: Some("/api/v2/test"),
};

/// only the API, never github.com or the storage a download is sent on to. any key may read
/// the limits, an installation's key from a CI run too.
pub(crate) static GITHUB_KEY: KeyShape = KeyShape {
    host: Some("api.github.com"),
    header: "authorization",
    value: bearer,
    env: &["GH_TOKEN", "GITHUB_TOKEN"],
    every_hop: &[],
    check: Some("/rate_limit"),
};

pub(crate) static GITLAB_KEY: KeyShape = KeyShape {
    host: Some("gitlab.com"),
    header: "authorization",
    value: bearer,
    env: &["GITLAB_TOKEN"],
    every_hop: &[],
    check: Some("/api/v4/user"),
};

/// the API alone: the links it gives to its files on S3 carry their own signature. any request
/// with a key it does not know is refused with 403.
pub(crate) static S2_KEY: KeyShape = KeyShape {
    host: Some("api.semanticscholar.org"),
    header: "x-api-key",
    value: raw,
    env: &["S2_API_KEY"],
    every_hop: &[],
    check: Some("/datasets/v1/release/latest"),
};

/// `DATAVERSE_API_TOKEN` counts only for the installation `DATAVERSE_URL` names.
pub(crate) static DATAVERSE_KEY: KeyShape = KeyShape {
    host: None,
    header: "x-dataverse-key",
    value: raw,
    env: &["DATAVERSE_API_TOKEN"],
    every_hop: &[],
    check: Some("/api/users/:me"),
};

impl Account {
    /// the key this account's source takes, for a source's own account or a dataverse
    /// installation's; none for a login, which goes as a username and password.
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
            Account::Dataverse(_) => &DATAVERSE_KEY,
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

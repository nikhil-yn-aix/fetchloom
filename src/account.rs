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

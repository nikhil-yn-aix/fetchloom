//! azure storage's names as the settings write them: an account, a container, and the SAS an
//! entry of `azure_containers` may carry, which never shows.

use std::collections::HashSet;
use std::fmt;

use url::Url;

/// the end of every blob host's name in azure's public cloud.
pub(crate) const BLOB_HOST: &str = ".blob.core.windows.net";

/// whether `name` names a storage account: 3 to 24 lowercase letters and digits.
pub(crate) fn is_account(name: &str) -> bool {
    (3..=24).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// whether `name` names a container: 3 to 63 lowercase letters, digits and single hyphens,
/// starting and ending with a letter or digit.
pub(crate) fn is_container(name: &str) -> bool {
    let plain = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let bytes = name.as_bytes();
    (3..=63).contains(&name.len())
        && bytes.first().is_some_and(plain)
        && bytes.last().is_some_and(plain)
        && !name.contains("--")
        && bytes.iter().all(|byte| plain(byte) || *byte == b'-')
}

/// the account a blob host names, such as `lab` for lab.blob.core.windows.net.
pub(crate) fn account_of(host: &str) -> Option<&str> {
    host.strip_suffix(BLOB_HOST)
        .filter(|account| is_account(account))
}

/// a shared access signature: the query that lets a request in, which never shows.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Sas(String);

impl Sas {
    /// reads a SAS as the azure portal and the az cli give one, with or without a leading `?`:
    /// a query with a signature, `sig`, and the version it was signed for, `sv`.
    pub(crate) fn parse(text: &str) -> Option<Sas> {
        let text = text.trim();
        let text = text.strip_prefix('?').unwrap_or(text);
        let plain = !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'#' | b'?'));
        let keys: HashSet<String> = url::form_urlencoded::parse(text.as_bytes())
            .map(|(key, _)| key.into_owned())
            .collect();
        (plain && keys.contains("sig") && keys.contains("sv")).then(|| Sas(text.to_owned()))
    }

    /// the query a request carries it in.
    pub(crate) fn query(&self) -> &str {
        &self.0
    }

    /// when it stops letting requests in, as its `se` says.
    pub(crate) fn expiry(&self) -> Option<String> {
        url::form_urlencoded::parse(self.0.as_bytes())
            .find(|(key, _)| key == "se")
            .map(|(_, value)| value.into_owned())
    }
}

impl fmt::Debug for Sas {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sas(***)")
    }
}

/// a container as the `azure_containers` setting names it, with the SAS its entry gives, if any.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AzureContainer {
    account: String,
    name: String,
    sas: Option<Sas>,
}

impl AzureContainer {
    /// reads `azure:<account>/<container>`, or the container's link,
    /// `https://<account>.blob.core.windows.net/<container>`, which may carry the container's
    /// SAS as the azure portal gives its SAS link. either may end in `/`.
    pub(crate) fn parse(text: &str) -> Option<AzureContainer> {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix("azure:") {
            let rest = rest.strip_suffix('/').unwrap_or(rest);
            let (account, name) = rest.split_once('/')?;
            let account = account.to_ascii_lowercase();
            return (is_account(&account) && is_container(name)).then(|| AzureContainer {
                account,
                name: name.to_owned(),
                sas: None,
            });
        }
        let url = Url::parse(text).ok()?;
        let account = account_of(url.host_str()?)?.to_owned();
        let path = url.path().strip_prefix('/')?;
        let name = path.strip_suffix('/').unwrap_or(path);
        let sas = match url.query() {
            Some(query) => Some(Sas::parse(query)?),
            None => None,
        };
        let plain = url.scheme() == "https"
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none();
        (plain && is_container(name)).then(|| AzureContainer {
            account,
            name: name.to_owned(),
            sas,
        })
    }

    pub(crate) fn account(&self) -> &str {
        &self.account
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// the SAS the setting's entry gives the container.
    pub(crate) fn sas(&self) -> Option<&Sas> {
        self.sas.as_ref()
    }
}

impl fmt::Display for AzureContainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "azure:{}/{}", self.account, self.name)
    }
}

impl fmt::Debug for AzureContainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sas = if self.sas.is_some() {
            " with a SAS"
        } else {
            ""
        };
        write!(f, "AzureContainer({self}{sas})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a SAS reads as the portal and the az cli give one, and a container's entry as its name or
    /// its link, which may carry its own SAS. neither shows the SAS when printed for debugging.
    #[test]
    fn settings_read_containers_and_sas_as_azure_gives_them() {
        let sas =
            Sas::parse("?sv=2022-11-02&sr=c&sp=rl&se=2026-12-31T00%3A00%3A00Z&sig=abc%3D").unwrap();
        assert_eq!(
            sas.query(),
            "sv=2022-11-02&sr=c&sp=rl&se=2026-12-31T00%3A00%3A00Z&sig=abc%3D"
        );
        assert_eq!(sas.expiry().as_deref(), Some("2026-12-31T00:00:00Z"));
        assert_eq!(format!("{sas:?}"), "Sas(***)");
        for bad in ["", "sv=1", "sig=abc", "sv=1&sig=a b", "sv=1&sig=a#b"] {
            assert_eq!(Sas::parse(bad), None, "{bad}");
        }
        let linked =
            AzureContainer::parse("https://lab.blob.core.windows.net/raw/?sv=2022-11-02&sig=abc")
                .unwrap();
        assert_eq!(linked.to_string(), "azure:lab/raw");
        assert_eq!(linked.sas().map(Sas::query), Some("sv=2022-11-02&sig=abc"));
        assert!(!format!("{linked:?}").contains("abc"));
        assert_eq!(
            AzureContainer::parse("azure:Lab/raw/").map(|entry| entry.to_string()),
            Some("azure:lab/raw".to_owned())
        );
        for bad in [
            "azure:lab",
            "azure:lab/raw/scans",
            "https://lab.blob.core.windows.net/raw?sp=rl",
            "https://lab.blob.core.windows.net:8443/raw",
            "https://lab.example.org/raw",
        ] {
            assert_eq!(AzureContainer::parse(bad), None, "{bad}");
        }
    }
}

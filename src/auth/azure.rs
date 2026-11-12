//! azure storage's credentials where the az cli reads them: a SAS in `AZURE_STORAGE_SAS_TOKEN`
//! or an account key in `AZURE_STORAGE_KEY`, each kept to the account `AZURE_STORAGE_ACCOUNT`
//! names. [`crate::net`] decides which request carries which; the key only signs and is never
//! sent.

use std::fmt;

use crate::error::{Error, Kind};
use crate::source::{AzureContainer, Sas, Var, is_azure_account};

/// what the environment gives for azure storage.
#[derive(Clone, Debug, Default)]
pub(crate) struct Storage {
    /// the SAS `AZURE_STORAGE_SAS_TOKEN` holds.
    pub(crate) sas: Option<Sas>,
    /// the key `AZURE_STORAGE_KEY` holds, of the account in `account`.
    pub(crate) key: Option<AccountKey>,
    /// the account `AZURE_STORAGE_ACCOUNT` names, in lowercase, which the SAS and the key are
    /// kept to.
    pub(crate) account: Option<String>,
}

/// a storage account's key, which signs requests and never shows.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AccountKey(Vec<u8>);

impl AccountKey {
    /// reads a key as the azure portal and `az storage account keys list` give one: base64.
    pub(crate) fn parse(text: &str) -> Option<AccountKey> {
        decoded(text.trim())
            .filter(|bytes| !bytes.is_empty())
            .map(AccountKey)
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for AccountKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountKey(***)")
    }
}

/// what the environment gives for the `listed` containers.
///
/// # Errors
///
/// `auth.required` for a variable that holds no SAS, account or key fetchloom can read, a key
/// without its account, or a SAS without its account while the listed containers it may go to,
/// those without a SAS of their own, are in two or more accounts, since a SAS is one account's.
/// none of them repeats what a variable holds.
pub(crate) fn find(var: Var<'_>, listed: &[AzureContainer]) -> Result<Storage, Error> {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    let sas = set("AZURE_STORAGE_SAS_TOKEN")
        .map(|text| {
            text.to_str().and_then(Sas::parse).ok_or_else(|| {
                Error::new(
                    Kind::AuthRequired,
                    "AZURE_STORAGE_SAS_TOKEN holds no SAS fetchloom can read",
                )
                .help("set it to the SAS the azure portal or `az storage container generate-sas` gives, a query such as sv=...&sp=rl&se=...&sig=...")
            })
        })
        .transpose()?;
    let key = set("AZURE_STORAGE_KEY");
    if sas.is_none() && key.is_none() {
        return Ok(Storage::default());
    }
    let account = set("AZURE_STORAGE_ACCOUNT")
        .map(|account| {
            account
                .to_str()
                .map(str::to_ascii_lowercase)
                .filter(|account| is_azure_account(account))
                .ok_or_else(|| {
                    Error::new(
                        Kind::AuthRequired,
                        "AZURE_STORAGE_ACCOUNT holds no storage account's name",
                    )
                    .help("set it to the account the SAS or the key is for, 3 to 24 lowercase letters and digits, or unset it")
                })
        })
        .transpose()?;
    let key = match key {
        None => None,
        Some(_) if account.is_none() => {
            return Err(Error::new(
                Kind::AuthRequired,
                "AZURE_STORAGE_KEY is set, but AZURE_STORAGE_ACCOUNT is not",
            )
            .help("set AZURE_STORAGE_ACCOUNT to the account the key is for, or unset AZURE_STORAGE_KEY"));
        }
        Some(text) => Some(text.to_str().and_then(AccountKey::parse).ok_or_else(|| {
            Error::new(
                Kind::AuthRequired,
                "AZURE_STORAGE_KEY holds no account key fetchloom can read",
            )
            .help("set it to one of the account's access keys, as the azure portal or `az storage account keys list` gives it")
        })?),
    };
    if sas.is_some() && account.is_none() {
        let mut accounts: Vec<&str> = listed
            .iter()
            .filter(|entry| entry.sas().is_none())
            .map(AzureContainer::account)
            .collect();
        accounts.sort_unstable();
        accounts.dedup();
        if accounts.len() > 1 {
            return Err(Error::new(
                Kind::AuthRequired,
                format!(
                    "AZURE_STORAGE_SAS_TOKEN names no account, and azure_containers lists containers in {} accounts: {}",
                    accounts.len(),
                    crate::units::some_of(&accounts, 3)
                ),
            )
            .help("set AZURE_STORAGE_ACCOUNT to the account the SAS is for, or give each container its own SAS link in azure_containers"));
        }
    }
    Ok(Storage { sas, key, account })
}

/// the bytes `text` writes in base64, with the standard alphabet and its padding.
fn decoded(text: &str) -> Option<Vec<u8>> {
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
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn environment(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs = pairs.to_vec();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn base64_reads_back_what_was_written() {
        for length in 0..70 {
            let bytes: Vec<u8> = (0..length)
                .map(|at| u8::try_from((at * 37 + 11) % 256).unwrap())
                .collect();
            let written = crate::source::base64(&bytes);
            let read = decoded(&written);
            if length == 0 {
                assert_eq!(read, None);
            } else {
                assert_eq!(read.as_deref(), Some(&bytes[..]), "{written}");
            }
        }
        for unreadable in ["abc", "ab=c", "a===", "ab$d", "QR==", "key with spaces="] {
            assert_eq!(decoded(unreadable), None, "{unreadable}");
        }
    }

    /// a key is kept to the account `AZURE_STORAGE_ACCOUNT` names, needs it, and a key or
    /// account fetchloom cannot read fails without repeating what it holds.
    #[test]
    fn a_key_needs_its_account_and_never_shows() {
        let found = find(
            &environment(&[
                ("AZURE_STORAGE_KEY", "c2VjcmV0LWtleQ=="),
                ("AZURE_STORAGE_ACCOUNT", "Lab"),
            ]),
            &[],
        )
        .unwrap();
        assert_eq!(found.account.as_deref(), Some("lab"));
        assert_eq!(
            found.key.as_ref().map(AccountKey::bytes),
            Some(&b"secret-key"[..])
        );
        assert_eq!(format!("{:?}", found.key), "Some(AccountKey(***))");
        for (pairs, message) in [
            (
                &[("AZURE_STORAGE_KEY", "c2VjcmV0LWtleQ==")][..],
                "AZURE_STORAGE_KEY is set, but AZURE_STORAGE_ACCOUNT is not",
            ),
            (
                &[
                    ("AZURE_STORAGE_KEY", "leaked key"),
                    ("AZURE_STORAGE_ACCOUNT", "lab"),
                ],
                "AZURE_STORAGE_KEY holds no account key fetchloom can read",
            ),
            (
                &[
                    ("AZURE_STORAGE_KEY", "c2VjcmV0LWtleQ=="),
                    ("AZURE_STORAGE_ACCOUNT", "leaked/account"),
                ],
                "AZURE_STORAGE_ACCOUNT holds no storage account's name",
            ),
        ] {
            let error = find(&environment(pairs), &[]).unwrap_err();
            assert_eq!(error.kind, Kind::AuthRequired);
            assert_eq!(error.message, message);
            assert!(!format!("{error:?}").contains("leaked"), "{error:?}");
        }
        // an account nothing uses is not read at all
        let unused = find(
            &environment(&[("AZURE_STORAGE_ACCOUNT", "Not An Account")]),
            &[],
        );
        assert!(unused.is_ok());
    }
}

//! azure storage's credentials where the az cli reads them: a SAS in `AZURE_STORAGE_SAS_TOKEN`
//! or an account key in `AZURE_STORAGE_KEY`, each kept to the account `AZURE_STORAGE_ACCOUNT`
//! names, else a token of the az cli's login, from `az account get-access-token`, run as
//! [`process`] runs a program. `AZURE_TOKEN_CREDENTIALS` chooses the sign-in as it does for
//! azure's own libraries. [`crate::net`] decides which request carries which; the key only signs
//! and is never sent.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::auth::process::{self, PATIENCE};
use crate::error::{Error, Kind};
use crate::source::{AzureContainer, Sas, Var, is_azure_account};
use crate::stop::Stop;

/// the resource whose tokens read azure storage, in any account their identity may read.
pub(crate) const STORAGE: &str = "https://storage.azure.com/";

/// how to sign in when a container needs it and the environment gives no SAS or key.
const SIGN_IN: &str = "sign in with `az login`";

/// what the environment gives for azure storage.
#[derive(Clone, Debug)]
pub(crate) struct Storage {
    /// the SAS `AZURE_STORAGE_SAS_TOKEN` holds.
    pub(crate) sas: Option<Sas>,
    /// the key `AZURE_STORAGE_KEY` holds, of the account in `account`.
    pub(crate) key: Option<AccountKey>,
    /// the account `AZURE_STORAGE_ACCOUNT` names, in lowercase, which the SAS and the key are
    /// kept to.
    pub(crate) account: Option<String>,
    /// how a token is got, or what is wrong with how `AZURE_TOKEN_CREDENTIALS` asks for one,
    /// told only once a container needs a token.
    pub(crate) sign_in: Result<SignIn, Error>,
}

/// how a token for azure storage is got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SignIn {
    /// no way: no folder on PATH holds the az cli, and nothing asks for another way.
    None,
    /// `az account get-access-token`, with the az cli at this path.
    Cli(PathBuf),
}

/// a token for azure storage, a secret, and how long it lives from when it was asked for.
pub(crate) struct Token {
    pub(crate) value: String,
    pub(crate) lifetime: Duration,
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token")
            .field("lifetime", &self.lifetime)
            .finish_non_exhaustive()
    }
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
    let sign_in = sign_in(var);
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
        return Ok(Storage {
            sas,
            key: None,
            account: None,
            sign_in,
        });
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
    Ok(Storage {
        sas,
        key,
        account,
        sign_in,
    })
}

/// how `AZURE_TOKEN_CREDENTIALS` asks for a token, read as azure's own libraries read it, any
/// case alike: unset, `AzureCliCredential` or `dev` take the az cli's login, the one developer
/// tool fetchloom reads, which a folder on PATH named by its full path must hold. unset, no az
/// cli means no sign-in.
///
/// # Errors
///
/// `auth.required` for the az cli asked for by name with none on PATH, or a credential
/// fetchloom does not read.
fn sign_in(var: Var<'_>) -> Result<SignIn, Error> {
    let written = var("AZURE_TOKEN_CREDENTIALS")
        .and_then(|value| value.into_string().ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let asked = written.as_deref().map(str::to_ascii_lowercase);
    let cli = var("PATH").and_then(|path| {
        let file = if cfg!(windows) { "az.cmd" } else { "az" };
        crate::machine::installed(&path, file)
    });
    match asked.as_deref() {
        None => Ok(cli.map_or(SignIn::None, SignIn::Cli)),
        Some("azureclicredential" | "dev") => cli.map(SignIn::Cli).ok_or_else(|| {
            Error::new(
                Kind::AuthRequired,
                "AZURE_TOKEN_CREDENTIALS asks for the az cli's login, and no folder on PATH holds the az cli",
            )
            .help("install the azure cli and sign in with `az login`, or unset AZURE_TOKEN_CREDENTIALS")
        }),
        Some(_) => Err(Error::new(
            Kind::AuthRequired,
            format!(
                "AZURE_TOKEN_CREDENTIALS asks for {}, which fetchloom does not read",
                crate::auth::google::plain(written.as_deref().unwrap_or_default())
            ),
        )
        .help("set it to AzureCliCredential to sign in with the az cli, or unset it")),
    }
}

/// a token of the az cli's login at `path` that reads azure storage, from `az account
/// get-access-token`, which must still hold at `now`.
///
/// # Errors
///
/// `interrupted` once `stop` asks; `auth.required` when the az cli could not start, failed, as
/// it does without a login, ran past its time, or printed no token fetchloom reads or one that
/// had expired by `now`. none of them repeats what it printed.
pub(crate) async fn cli_token(path: &Path, now: SystemTime, stop: &Stop) -> Result<Token, Error> {
    let named = "`az account get-access-token`";
    let args = [
        "account",
        "get-access-token",
        "--resource",
        STORAGE,
        "--output",
        "json",
    ]
    .map(str::to_owned);
    tracing::debug!(program = %path.display(), "asking the az cli for an azure storage token");
    let printed = process::printed(path, &args, PATIENCE, stop)
        .await
        .map_err(|ran| {
            let Some(message) = ran.said(named, "token", path, PATIENCE) else {
                return Stop::stopped();
            };
            Error::new(Kind::AuthRequired, message).help(format!(
                "{SIGN_IN}, or run `az account get-access-token --resource {STORAGE}` yourself to see what went wrong"
            ))
        })?;
    let (value, expires) = read_cli(&printed).map_err(|wrong| {
        Error::new(
            Kind::AuthRequired,
            format!("{named} printed no token fetchloom can read: {wrong}"),
        )
        .help("update the az cli to version 2.54 or later")
    })?;
    let Ok(lifetime) = expires.duration_since(now) else {
        return Err(Error::new(
            Kind::AuthRequired,
            format!(
                "the token the az cli gave expired at {}",
                crate::project::lock::timestamp(expires)
            ),
        )
        .help(format!("{SIGN_IN} again, and check this machine's clock")));
    };
    Ok(Token { value, lifetime })
}

/// the token `az account get-access-token --output json` printed, and when it expires, or what
/// is wrong with what it printed, which never repeats it.
fn read_cli(printed: &[u8]) -> Result<(String, SystemTime), &'static str> {
    let value: Value = serde_json::from_slice(printed).map_err(|_| "it is not JSON")?;
    let object = value.as_object().ok_or("it is not a JSON object")?;
    if object
        .get("tokenType")
        .and_then(Value::as_str)
        .is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer"))
    {
        return Err("its tokenType is not Bearer");
    }
    // a token goes in a header, so one a header cannot carry is no token
    let token = object
        .get("accessToken")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic()))
        .ok_or("it has no accessToken a request can carry")?;
    let seconds = match object.get("expires_on") {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    }
    .ok_or("it has no expires_on, which the az cli writes from version 2.54")?;
    Ok((token.to_owned(), UNIX_EPOCH + Duration::from_secs(seconds)))
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

    /// a folder on PATH that holds a program called as the az cli is, for `sign_in` to find.
    fn az_folder() -> (tempfile::TempDir, PathBuf) {
        let folder = tempfile::tempdir().unwrap();
        let az = folder
            .path()
            .join(if cfg!(windows) { "az.cmd" } else { "az" });
        std::fs::write(&az, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&az, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        (folder, az)
    }

    /// `AZURE_TOKEN_CREDENTIALS` chooses the sign-in as azure's libraries read it: the az cli
    /// when unset and on PATH, or when asked for by name or as `dev`, in any case. asked for and
    /// missing, or another credential, is an error.
    #[test]
    fn azure_token_credentials_chooses_the_sign_in() {
        let (folder, az) = az_folder();
        let path = folder.path().to_str().unwrap().to_owned();
        let chosen = |asked: Option<&str>, path: Option<&str>| {
            let asked = asked.map(str::to_owned);
            let path = path.map(str::to_owned);
            sign_in(&move |name: &str| match name {
                "AZURE_TOKEN_CREDENTIALS" => asked.clone().map(OsString::from),
                "PATH" => path.clone().map(OsString::from),
                _ => None,
            })
        };
        assert_eq!(chosen(None, None).unwrap(), SignIn::None);
        for asked in [
            None,
            Some("AzureCliCredential"),
            Some("DEV"),
            Some(" azureclicredential "),
        ] {
            assert_eq!(
                chosen(asked, Some(&path)).unwrap(),
                SignIn::Cli(az.clone()),
                "{asked:?}"
            );
        }
        let missing = chosen(Some("AzureCliCredential"), Some("relative/folder")).unwrap_err();
        assert_eq!(
            missing.message,
            "AZURE_TOKEN_CREDENTIALS asks for the az cli's login, and no folder on PATH holds the az cli"
        );
        let other = chosen(Some("EnvironmentCredential"), Some(&path)).unwrap_err();
        assert_eq!(other.kind, Kind::AuthRequired);
        assert_eq!(
            other.message,
            "AZURE_TOKEN_CREDENTIALS asks for EnvironmentCredential, which fetchloom does not read"
        );
    }

    #[test]
    fn the_az_clis_token_is_read_and_a_fault_never_repeats_it() {
        let (token, expires) = read_cli(
            br#"{"accessToken": "eyJ0eXAi.printed", "expiresOn": "2026-11-12 23:59:59.000000", "expires_on": 1794527999, "subscription": "s", "tenant": "t", "tokenType": "Bearer"}"#,
        )
        .unwrap();
        assert_eq!(token, "eyJ0eXAi.printed");
        assert_eq!(
            crate::project::lock::timestamp(expires),
            "2026-11-12T23:59:59Z"
        );
        assert!(read_cli(br#"{"accessToken": "a", "expires_on": "1794527999"}"#).is_ok());
        for (printed, wrong) in [
            ("printed-token", "it is not JSON"),
            (r#"["printed-token"]"#, "it is not a JSON object"),
            (
                r#"{"accessToken": "printed-token", "expires_on": 1, "tokenType": "pop"}"#,
                "its tokenType is not Bearer",
            ),
            (
                r#"{"accessToken": "printed token", "expires_on": 1}"#,
                "it has no accessToken a request can carry",
            ),
            (
                r#"{"accessToken": "printed-token", "expiresOn": "2026-11-12 23:59:59.000000"}"#,
                "it has no expires_on, which the az cli writes from version 2.54",
            ),
        ] {
            assert_eq!(read_cli(printed.as_bytes()).err(), Some(wrong), "{printed}");
        }
    }

    /// the az cli at a test's own path: `script` run by sh.
    #[cfg(unix)]
    fn az(folder: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = folder.join("az");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// the token comes from `az account get-access-token` for azure storage's resource, and an
    /// az cli that fails, as it does without a login, or gives an expired token, says to sign in
    /// without repeating what it printed.
    #[cfg(unix)]
    #[test]
    fn the_az_cli_gives_a_token_or_says_to_sign_in() {
        use crate::source::recorded::block_on;
        let folder = tempfile::tempdir().unwrap();
        let asked = folder.path().join("asked");
        let now = UNIX_EPOCH + Duration::from_secs(1_794_524_399);
        let path = az(
            folder.path(),
            &format!(
                "printf '%s\\n' \"$@\" > '{}'\nprintf '{{\"accessToken\": \"printed-token\", \"expires_on\": 1794527999}}'",
                asked.display()
            ),
        );
        let token = block_on(cli_token(&path, now, &Stop::default())).unwrap();
        assert_eq!(
            (token.value.as_str(), token.lifetime),
            ("printed-token", Duration::from_hours(1))
        );
        assert_eq!(
            std::fs::read_to_string(&asked).unwrap(),
            "account\nget-access-token\n--resource\nhttps://storage.azure.com/\n--output\njson\n"
        );
        let later = now + Duration::from_hours(2);
        let expired = block_on(cli_token(&path, later, &Stop::default())).unwrap_err();
        assert_eq!(
            expired.message,
            "the token the az cli gave expired at 2026-11-12T23:59:59Z"
        );
        let path = az(
            folder.path(),
            "printf 'printed-token'\nprintf \"Please run 'az login' to setup account.\" >&2\nexit 1",
        );
        let failed = block_on(cli_token(&path, now, &Stop::default())).unwrap_err();
        assert_eq!(failed.kind, Kind::AuthRequired);
        assert_eq!(
            failed.message,
            "`az account get-access-token` exited with code 1"
        );
        let help = failed.help.unwrap_or_default();
        assert!(help.starts_with("sign in with `az login`"), "{help}");
        assert!(!format!("{}{help}", failed.message).contains("printed-token"));
    }
}

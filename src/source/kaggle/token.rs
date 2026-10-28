//! kaggle's credentials, found where the kaggle CLI keeps them.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::source::Var;

/// the legacy key in `kaggle.json`.
#[derive(Default, Deserialize)]
struct Key {
    username: Option<String>,
    key: Option<String>,
}

/// the `authorization` value the kaggle CLI would send: an API token as a bearer token, else a
/// legacy username and key as basic authentication.
pub(super) fn find(var: Var<'_>) -> Option<String> {
    if let Some(token) = api_token(var) {
        return Some(format!("Bearer {token}"));
    }
    let (username, key) = legacy_key(var)?;
    Some(format!(
        "Basic {}",
        base64(format!("{username}:{key}").as_bytes())
    ))
}

/// `KAGGLE_API_TOKEN`, which holds a token or names a file holding one, else `access_token` or
/// `access_token.txt` in `~/.kaggle`.
fn api_token(var: Var<'_>) -> Option<String> {
    if let Some(value) = var("KAGGLE_API_TOKEN").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(&value);
        if path.is_file() {
            return read(&path);
        }
        return value.to_str().and_then(clean);
    }
    let folder = crate::source::home(var)?.join(".kaggle");
    read(&folder.join("access_token")).or_else(|| read(&folder.join("access_token.txt")))
}

/// `KAGGLE_USERNAME` and `KAGGLE_KEY`, each over the same field of `kaggle.json`.
fn legacy_key(var: Var<'_>) -> Option<(String, String)> {
    let set = |name: &str| {
        var(name)
            .and_then(|value| value.into_string().ok())
            .and_then(|value| clean(&value))
    };
    let file: Key = config_folder(var)
        .and_then(|folder| fs::read_to_string(folder.join("kaggle.json")).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let username = set("KAGGLE_USERNAME").or_else(|| file.username.as_deref().and_then(clean))?;
    let key = set("KAGGLE_KEY").or_else(|| file.key.as_deref().and_then(clean))?;
    Some((username, key))
}

/// where the kaggle CLI keeps `kaggle.json`: `KAGGLE_CONFIG_DIR`, else `~/.kaggle`, else on linux
/// the kaggle folder of `XDG_CONFIG_HOME` when `~/.kaggle` does not exist.
fn config_folder(var: Var<'_>) -> Option<PathBuf> {
    if let Some(folder) = var("KAGGLE_CONFIG_DIR").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(folder));
    }
    let home = crate::source::home(var)?;
    let dotted = home.join(".kaggle");
    if cfg!(target_os = "linux") && !dotted.exists() {
        let config = var("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map_or_else(|| home.join(".config"), PathBuf::from);
        return Some(config.join("kaggle"));
    }
    Some(dotted)
}

fn read(path: &Path) -> Option<String> {
    clean(&fs::read_to_string(path).ok()?)
}

/// a value without the whitespace around it, or nothing when it is blank.
fn clean(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// standard base64 with padding, which basic authentication sends.
fn base64(bytes: &[u8]) -> String {
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

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::path::Path;

    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// what `find` returns with this home folder and these variables.
    fn found(home: &Path, pairs: &[(&str, &OsStr)]) -> Option<String> {
        let var = |name: &str| {
            [
                ("HOME", home.as_os_str()),
                ("USERPROFILE", home.as_os_str()),
            ]
            .iter()
            .chain(pairs)
            .rfind(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
        };
        find(&var)
    }

    #[test]
    fn an_api_token_is_sent_as_a_bearer_token() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        assert_eq!(found(home, &[]), None);
        write(&home.join(".kaggle/access_token.txt"), "KGAT_txt\n");
        assert_eq!(found(home, &[]).as_deref(), Some("Bearer KGAT_txt"));
        write(&home.join(".kaggle/access_token"), " KGAT_file \n");
        assert_eq!(found(home, &[]).as_deref(), Some("Bearer KGAT_file"));
        let named = home.join("secrets/kaggle");
        write(&named, "KGAT_named");
        assert_eq!(
            found(home, &[("KAGGLE_API_TOKEN", named.as_os_str())]).as_deref(),
            Some("Bearer KGAT_named")
        );
        assert_eq!(
            found(home, &[("KAGGLE_API_TOKEN", OsStr::new("KGAT_env"))]).as_deref(),
            Some("Bearer KGAT_env")
        );
    }

    #[test]
    fn a_legacy_key_is_sent_as_basic_authentication() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        write(
            &home.join(".kaggle/kaggle.json"),
            r#"{"username":"user","key":"key"}"#,
        );
        // base64 of user:key
        assert_eq!(found(home, &[]).as_deref(), Some("Basic dXNlcjprZXk="));
        let elsewhere = home.join("config");
        write(
            &elsewhere.join("kaggle.json"),
            r#"{"username":"other","key":"secret"}"#,
        );
        let config = ("KAGGLE_CONFIG_DIR", elsewhere.as_os_str());
        // base64 of other:secret
        assert_eq!(
            found(home, &[config]).as_deref(),
            Some("Basic b3RoZXI6c2VjcmV0")
        );
        // base64 of other:key2
        assert_eq!(
            found(home, &[config, ("KAGGLE_KEY", OsStr::new("key2"))]).as_deref(),
            Some("Basic b3RoZXI6a2V5Mg==")
        );
        let both = [
            ("KAGGLE_USERNAME", OsStr::new("env")),
            ("KAGGLE_KEY", OsStr::new("only")),
        ];
        let nowhere = home.join("nowhere");
        // base64 of env:only
        assert_eq!(
            found(&nowhere, &both).as_deref(),
            Some("Basic ZW52Om9ubHk=")
        );
        assert_eq!(found(&nowhere, &both[..1]), None, "a username alone");
    }

    #[test]
    fn a_token_wins_over_a_legacy_key() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        write(
            &home.join(".kaggle/kaggle.json"),
            r#"{"username":"user","key":"key"}"#,
        );
        write(&home.join(".kaggle/access_token"), "KGAT_file");
        assert_eq!(found(home, &[]).as_deref(), Some("Bearer KGAT_file"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_falls_back_to_the_xdg_config_folder() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let xdg = home.join("xdg");
        write(
            &xdg.join("kaggle/kaggle.json"),
            r#"{"username":"user","key":"key"}"#,
        );
        assert_eq!(
            found(home, &[("XDG_CONFIG_HOME", xdg.as_os_str())]).as_deref(),
            Some("Basic dXNlcjprZXk=")
        );
        write(
            &home.join(".config/kaggle/kaggle.json"),
            r#"{"username":"user","key":"key"}"#,
        );
        assert_eq!(found(home, &[]).as_deref(), Some("Basic dXNlcjprZXk="));
    }

    #[test]
    fn base64_matches_rfc_4648() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (text, encoded) in cases {
            assert_eq!(base64(text.as_bytes()), encoded, "{text}");
        }
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }
}

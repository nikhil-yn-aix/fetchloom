//! the hugging face token, found where the hugging face tools keep it.

use std::fs;
use std::path::PathBuf;

use crate::source::Var;

/// the token the hugging face tools would use: `HF_TOKEN`, else the file `HF_TOKEN_PATH` names,
/// else `token` in `HF_HOME`, else in the hugging face folder of the user's cache.
pub(super) fn find(var: Var<'_>) -> Option<String> {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    if let Some(token) = set("HF_TOKEN") {
        return token.to_str().and_then(clean);
    }
    let path = set("HF_TOKEN_PATH")
        .map(PathBuf::from)
        .or_else(|| set("HF_HOME").map(|home| PathBuf::from(home).join("token")))
        .or_else(|| {
            set("XDG_CACHE_HOME")
                .map(|cache| PathBuf::from(cache).join("huggingface").join("token"))
        })
        .or_else(|| {
            crate::source::home(var)
                .map(|home| home.join(".cache").join("huggingface").join("token"))
        })?;
    clean(&fs::read_to_string(path).ok()?)
}

/// a token without the newline a file ends with, or nothing when it is blank.
fn clean(text: &str) -> Option<String> {
    let token = text.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::path::Path;

    use super::*;

    #[test]
    fn the_token_is_found_where_the_hugging_face_tools_keep_it() {
        let home = tempfile::tempdir().unwrap();
        let write = |path: &Path, token: &str| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, token).unwrap();
        };
        write(&home.path().join(".cache/huggingface/token"), "hf_cache\n");
        let xdg = home.path().join("xdg");
        write(&xdg.join("huggingface/token"), "hf_xdg");
        let hf_home = home.path().join("hf");
        write(&hf_home.join("token"), "hf_home");
        let named = home.path().join("named");
        write(&named, " hf_named ");
        let found = |pairs: &[(&str, &OsStr)]| {
            let var = |name: &str| {
                [
                    ("HOME", home.path().as_os_str()),
                    ("USERPROFILE", home.path().as_os_str()),
                ]
                .iter()
                .chain(pairs)
                .rfind(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
            };
            find(&var)
        };
        let (xdg, hf_home, named) = (xdg.as_os_str(), hf_home.as_os_str(), named.as_os_str());
        assert_eq!(found(&[]).as_deref(), Some("hf_cache"));
        assert_eq!(found(&[("XDG_CACHE_HOME", xdg)]).as_deref(), Some("hf_xdg"));
        assert_eq!(
            found(&[("XDG_CACHE_HOME", xdg), ("HF_HOME", hf_home)]).as_deref(),
            Some("hf_home")
        );
        assert_eq!(
            found(&[("HF_HOME", hf_home), ("HF_TOKEN_PATH", named)]).as_deref(),
            Some("hf_named")
        );
        assert_eq!(
            found(&[("HF_TOKEN_PATH", named), ("HF_TOKEN", OsStr::new("hf_env"))]).as_deref(),
            Some("hf_env")
        );
        let elsewhere = tempfile::tempdir().unwrap();
        let nowhere = elsewhere.path().as_os_str();
        assert_eq!(found(&[("HOME", nowhere), ("USERPROFILE", nowhere)]), None);
    }
}

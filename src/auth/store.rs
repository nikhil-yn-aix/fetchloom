//! keys for sources whose own tools keep no file: the system's credential store, or, when the
//! person agrees, a file only they can read. an index lists what was saved where, so fetchloom
//! never asks the credential store about sources a person never signed in to.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::source::Var;

const SERVICE: &str = "fetchloom";

/// where a saved key lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Place {
    /// windows credential manager, the macos keychain or the linux secret service
    Keyring,
    /// `credentials.json` in fetchloom's folder, readable by its owner only
    File,
}

/// fetchloom's folder for settings: `%APPDATA%\fetchloom`, `~/Library/Application Support/
/// fetchloom` on macos, else `$XDG_CONFIG_HOME/fetchloom` or `~/.config/fetchloom`.
pub(crate) fn folder(var: Var<'_>) -> Option<PathBuf> {
    let set = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        return set("APPDATA").map(|appdata| appdata.join("fetchloom"));
    }
    if cfg!(target_os = "macos") {
        return crate::source::home(var)
            .map(|home| home.join("Library/Application Support/fetchloom"));
    }
    set("XDG_CONFIG_HOME")
        .or_else(|| crate::source::home(var).map(|home| home.join(".config")))
        .map(|config| config.join("fetchloom"))
}

/// whether the system's credential store can be used. `FETCHLOOM_KEYRING=0` turns it off, and a
/// linux session without a message bus has none, which is checked first so a server never waits
/// on a bus that is not there.
pub(crate) fn keyring_usable(var: Var<'_>) -> bool {
    if var("FETCHLOOM_KEYRING").is_some_and(|value| value == "0") {
        return false;
    }
    if cfg!(all(unix, not(target_os = "macos")))
        && var("DBUS_SESSION_BUS_ADDRESS").is_none_or(|value| value.is_empty())
    {
        return false;
    }
    keyring::Entry::store_status().is_ok()
}

/// what was saved, and where.
pub(crate) fn saved(var: Var<'_>) -> BTreeMap<String, Place> {
    folder(var)
        .and_then(|folder| fs::read_to_string(folder.join("accounts.json")).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// the key saved for `id`, or none.
pub(crate) fn read(var: Var<'_>, id: &str) -> Option<String> {
    match saved(var).get(id)? {
        Place::Keyring => keyring::Entry::new(SERVICE, id).ok()?.get_password().ok(),
        Place::File => file(var).remove(id),
    }
}

/// saves `secret` for `id` in `place`, replacing what was saved for it before.
pub(crate) fn save(var: Var<'_>, id: &str, secret: &str, place: Place) -> io::Result<()> {
    let folder =
        folder(var).ok_or_else(|| io::Error::other("no folder for fetchloom's settings"))?;
    forget(var, id)?;
    match place {
        Place::Keyring => keyring::Entry::new(SERVICE, id)
            .and_then(|entry| entry.set_password(secret))
            .map_err(io::Error::other)?,
        Place::File => {
            let path = folder.join("credentials.json");
            let mut keys: BTreeMap<String, String> = load(&path)?;
            keys.insert(id.to_owned(), secret.to_owned());
            write_private(&path, &json(&keys)?)?;
        }
    }
    let mut index: BTreeMap<String, Place> = load(&folder.join("accounts.json"))?;
    index.insert(id.to_owned(), place);
    write_private(&folder.join("accounts.json"), &json(&index)?)
}

/// removes what was saved for `id`. returns whether anything was.
pub(crate) fn forget(var: Var<'_>, id: &str) -> io::Result<bool> {
    let Some(folder) = folder(var) else {
        return Ok(false);
    };
    let mut index: BTreeMap<String, Place> = load(&folder.join("accounts.json"))?;
    let Some(place) = index.remove(id) else {
        return Ok(false);
    };
    match place {
        Place::Keyring => {
            if let Ok(entry) = keyring::Entry::new(SERVICE, id) {
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(err) => return Err(io::Error::other(err)),
                }
            }
        }
        Place::File => {
            let path = folder.join("credentials.json");
            let mut keys: BTreeMap<String, String> = load(&path)?;
            keys.remove(id);
            write_private(&path, &json(&keys)?)?;
        }
    }
    write_private(&folder.join("accounts.json"), &json(&index)?)?;
    Ok(true)
}

fn file(var: Var<'_>) -> BTreeMap<String, String> {
    folder(var)
        .and_then(|folder| fs::read_to_string(folder.join("credentials.json")).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// what `path` holds, empty when it does not exist. a file that cannot be read or is not valid
/// JSON is an error, so writing it back never drops the keys it held.
fn load<T: serde::de::DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is not valid JSON: {err}; fix or delete it",
                    path.display()
                ),
            )
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(err),
    }
}

fn json<T: serde::Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// writes `bytes` to `path` through a new file beside it, so a crash leaves the old file or the
/// new one. the new file gets a random name and is created, never opened, so nobody who can write
/// to the folder can plant a file or a link there to read the key or redirect the write. on unix
/// only the owner may read it; on windows it takes the folder's permissions, which keep other
/// users out of the profile folders the sources use by default.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let folder = path
        .parent()
        .ok_or_else(|| io::Error::other("a credential file needs a folder"))?;
    fs::create_dir_all(folder)?;
    // tempfile creates with O_EXCL and, on unix, mode 0600 from the start
    let mut staged = tempfile::Builder::new()
        .prefix(".fetchloom-")
        .suffix(".new")
        .tempfile_in(folder)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(path).map_err(|err| err.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn a_key_replaces_its_file_with_a_new_private_one() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("token");
        fs::write(&path, "old").unwrap();
        // the name the writer once used for its staging file means nothing now
        fs::write(folder.path().join(".token.fetchloom-new"), "planted").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_private(&path, b"secret\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret\n");
        assert_eq!(
            fs::read_to_string(folder.path().join(".token.fetchloom-new")).unwrap(),
            "planted"
        );
        let left: Vec<_> = fs::read_dir(folder.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".fetchloom-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn a_file_store_saves_lists_and_forgets_one_key_at_a_time() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().as_os_str().to_owned();
        let var = |name: &str| -> Option<OsString> {
            match name {
                "HOME" | "USERPROFILE" | "APPDATA" | "XDG_CONFIG_HOME" => Some(root.clone()),
                "FETCHLOOM_KEYRING" => Some("0".into()),
                _ => None,
            }
        };
        assert!(!keyring_usable(&var));
        save(&var, "zenodo", "z-key", Place::File).unwrap();
        save(&var, "dataverse:demo.dataverse.org", "d-key", Place::File).unwrap();
        save(&var, "zenodo", "z-new", Place::File).unwrap();
        assert_eq!(read(&var, "zenodo").as_deref(), Some("z-new"));
        assert_eq!(
            saved(&var).keys().map(String::as_str).collect::<Vec<_>>(),
            ["dataverse:demo.dataverse.org", "zenodo"]
        );
        assert!(forget(&var, "zenodo").unwrap());
        assert!(!forget(&var, "zenodo").unwrap());
        assert_eq!(read(&var, "zenodo"), None);
        assert_eq!(
            read(&var, "dataverse:demo.dataverse.org").as_deref(),
            Some("d-key")
        );
        let index = fs::read_to_string(folder(&var).unwrap().join("accounts.json")).unwrap();
        assert!(!index.contains("d-key"), "the index holds no secrets");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let keys = folder(&var).unwrap().join("credentials.json");
            assert_eq!(
                fs::metadata(keys).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // a damaged index is refused rather than written over, which would lose the other keys
        let index = folder(&var).unwrap().join("accounts.json");
        fs::write(&index, "{ not json").unwrap();
        let error = save(&var, "osf", "o-key", Place::File).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(forget(&var, "dataverse:demo.dataverse.org").is_err());
        assert_eq!(fs::read_to_string(&index).unwrap(), "{ not json");
        let keys = fs::read_to_string(folder(&var).unwrap().join("credentials.json")).unwrap();
        assert!(keys.contains("d-key"));
    }
}

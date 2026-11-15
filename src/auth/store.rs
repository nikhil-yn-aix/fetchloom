//! keys for sources whose own tools keep no file: the credential store, or a private file the
//! person agreed to. an index of what went where means the store is asked only for keys it holds.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::Path;

use crate::machine::Var;

const SERVICE: &str = "fetchloom";

/// where a saved key lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Place {
    /// windows credential manager, the macos keychain or the linux secret service.
    Keyring,
    /// `credentials.json` in fetchloom's folder, readable by its owner only.
    File,
}

/// a credential store: whether it can be used, and the calls that reach its keys.
pub(crate) struct Keyring {
    /// whether the store is reached over the session's message bus, as the linux secret service is.
    needs_bus: bool,
    /// asks the store whether it works.
    answers: fn() -> bool,
    get: fn(&str) -> keyring::Result<String>,
    set: fn(&str, &str) -> keyring::Result<()>,
    /// removes an id's key: none when the store cannot make an entry for it, as with no store.
    remove: fn(&str) -> Option<keyring::Result<()>>,
}

/// the system's own credential store.
pub(crate) const KEYRING: Keyring = Keyring {
    needs_bus: cfg!(all(unix, not(target_os = "macos"))),
    answers: || keyring::Entry::store_status().is_ok(),
    get: |id| keyring::Entry::new(SERVICE, id)?.get_password(),
    set: |id, secret| keyring::Entry::new(SERVICE, id)?.set_password(secret),
    remove: |id| {
        keyring::Entry::new(SERVICE, id)
            .ok()
            .map(|entry| entry.delete_credential())
    },
};

impl Keyring {
    /// whether the store can be used. a linux session without a message bus has none, checked
    /// first so a server never waits on a bus that is not there.
    pub(crate) fn usable(&self, var: Var<'_>) -> bool {
        if !keyring_allowed(var) {
            return false;
        }
        if self.needs_bus && var("DBUS_SESSION_BUS_ADDRESS").is_none_or(|value| value.is_empty()) {
            return false;
        }
        (self.answers)()
    }

    /// removes the key saved for `id`. no key there, or no store at all, leaves nothing to remove.
    fn delete(&self, id: &str) -> io::Result<()> {
        match (self.remove)(id) {
            None | Some(Ok(()) | Err(keyring::Error::NoEntry)) => Ok(()),
            Some(Err(err)) => Err(io::Error::other(err)),
        }
    }
}

/// whether `err`, from the credential store, says the store would not let fetchloom in, as a
/// locked keyring does where it cannot ask for its password.
pub(crate) fn locked(err: &io::Error) -> bool {
    match err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<keyring::Error>())
    {
        Some(keyring::Error::NoStorageAccess(_)) => true,
        Some(keyring::Error::PlatformFailure(failure)) => locked_keychain(failure.as_ref()),
        _ => false,
    }
}

/// macos's errSecInteractionNotAllowed: the keychain is locked, and no screen can show its
/// password prompt, as over ssh.
#[cfg(target_vendor = "apple")]
const INTERACTION_NOT_ALLOWED: i32 = -25308;

/// whether a failure the store reports, which keyring's apple store gives errSecInteractionNotAllowed
/// as, is a locked keychain, told by its status code since the message is in the system's language.
#[cfg(target_vendor = "apple")]
fn locked_keychain(failure: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    failure
        .downcast_ref::<security_framework::base::Error>()
        .is_some_and(|failure| failure.code() == INTERACTION_NOT_ALLOWED)
}

#[cfg(not(target_vendor = "apple"))]
fn locked_keychain(_failure: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    false
}

/// whether `FETCHLOOM_KEYRING` leaves the system's credential store in use: unless it is false,
/// as an on/off variable is read. a run refuses any other value before it starts.
pub(crate) fn keyring_allowed(var: Var<'_>) -> bool {
    let name = "FETCHLOOM_KEYRING";
    !matches!(
        crate::machine::switched(name, var(name).as_deref()),
        Ok(Some(false))
    )
}

/// what was saved, and where.
pub(crate) fn saved(var: Var<'_>) -> BTreeMap<String, Place> {
    crate::machine::settings_folder(var)
        .and_then(|folder| fs::read_to_string(folder.join("accounts.json")).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// the ids the index lists in the credential store while `FETCHLOOM_KEYRING` keeps fetchloom out
/// of it, whose keys read as none.
pub(crate) fn kept_out(var: Var<'_>) -> Vec<String> {
    if keyring_allowed(var) {
        return Vec::new();
    }
    saved(var)
        .into_iter()
        .filter_map(|(id, place)| (place == Place::Keyring).then_some(id))
        .collect()
}

/// the key saved for `id` in `place`, as the index lists it, or none.
pub(crate) fn read(var: Var<'_>, id: &str, place: Place) -> Option<String> {
    read_with(&KEYRING, var, id, place)
}

/// [`read`], with `keyring` as the credential store.
fn read_with(keyring: &Keyring, var: Var<'_>, id: &str, place: Place) -> Option<String> {
    match place {
        Place::Keyring => keyring_allowed(var)
            .then(|| (keyring.get)(id).ok())
            .flatten(),
        Place::File => file(var).remove(id),
    }
}

/// the refusal of a change `FETCHLOOM_KEYRING` keeps out of the credential store.
fn kept_out_of_store() -> io::Error {
    io::Error::other("FETCHLOOM_KEYRING is off, which keeps fetchloom out of the credential store")
}

/// the file every change to the saved keys locks, in fetchloom's settings folder.
const LOCK: &str = "accounts.lock";

/// the saved keys, and the `.netrc` fetchloom writes, held for one change until this is dropped.
pub(crate) struct Held {
    _lock: Option<File>,
}

/// holds the saved keys and the `.netrc` for one change, waiting while another run changes them:
/// a change reads a whole file and writes it back, so two at once would lose one of the keys.
/// with no settings folder, or on a filesystem without locks, the change goes on unheld.
pub(crate) fn hold(var: Var<'_>) -> io::Result<Held> {
    match crate::machine::settings_folder(var) {
        Some(folder) => hold_in(&folder),
        None => Ok(Held { _lock: None }),
    }
}

/// [`hold`], with `folder` the settings folder.
fn hold_in(folder: &Path) -> io::Result<Held> {
    let opened = fs::create_dir_all(folder)
        .and_then(|()| crate::volume::open_unfollowed(&folder.join(LOCK), true));
    let file = match opened {
        Ok(file) => file,
        // a settings folder that cannot be written holds no key to lose, while a `.netrc`
        // elsewhere may still change
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            return Ok(Held { _lock: None });
        }
        Err(err) => return Err(err),
    };
    loop {
        match file.lock() {
            Ok(()) => return Ok(Held { _lock: Some(file) }),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                tracing::debug!(%err, "this filesystem has no file locks");
                return Ok(Held { _lock: None });
            }
        }
    }
}

/// saves `secret` for `id` in `place`, replacing what was saved for it before. the new key is
/// stored before the old one goes, so a store that refuses it leaves the old key working.
pub(crate) fn save(var: Var<'_>, id: &str, secret: &str, place: Place) -> io::Result<()> {
    save_with(&KEYRING, var, id, secret, place)
}

/// [`save`], with `keyring` as the credential store.
fn save_with(
    keyring: &Keyring,
    var: Var<'_>,
    id: &str,
    secret: &str,
    place: Place,
) -> io::Result<()> {
    let folder = crate::machine::settings_folder(var)
        .ok_or_else(|| io::Error::other("no folder for fetchloom's settings"))?;
    let allowed = keyring_allowed(var);
    if place == Place::Keyring && !allowed {
        return Err(kept_out_of_store());
    }
    let _held = hold_in(&folder)?;
    let index_path = folder.join("accounts.json");
    let mut index: BTreeMap<String, Place> = load(&index_path)?;
    let before = index.get(id).copied();
    match place {
        Place::Keyring => (keyring.set)(id, secret).map_err(io::Error::other)?,
        Place::File => edit_file(&folder, |keys| {
            keys.insert(id.to_owned(), secret.to_owned());
        })?,
    }
    match before {
        // with the store off, the key saved there before stays, out of the index
        Some(Place::Keyring) if place != Place::Keyring && allowed => keyring.delete(id)?,
        Some(Place::File) if place != Place::File => edit_file(&folder, |keys| {
            keys.remove(id);
        })?,
        _ => {}
    }
    index.insert(id.to_owned(), place);
    write_private(&index_path, &json(&index)?)
}

/// changes the keys `credentials.json` holds and writes them back.
fn edit_file(folder: &Path, change: impl FnOnce(&mut BTreeMap<String, String>)) -> io::Result<()> {
    let path = folder.join("credentials.json");
    let mut keys: BTreeMap<String, String> = load(&path)?;
    change(&mut keys);
    write_private(&path, &json(&keys)?)
}

/// removes what was saved for `id`. returns whether anything was.
pub(crate) fn forget(var: Var<'_>, id: &str) -> io::Result<bool> {
    forget_with(&KEYRING, var, id)
}

/// [`forget`], with `keyring` as the credential store.
fn forget_with(keyring: &Keyring, var: Var<'_>, id: &str) -> io::Result<bool> {
    let Some(folder) = crate::machine::settings_folder(var) else {
        return Ok(false);
    };
    let _held = hold_in(&folder)?;
    let mut index: BTreeMap<String, Place> = load(&folder.join("accounts.json"))?;
    let Some(place) = index.remove(id) else {
        return Ok(false);
    };
    match place {
        Place::Keyring if !keyring_allowed(var) => return Err(kept_out_of_store()),
        Place::Keyring => keyring.delete(id)?,
        Place::File => edit_file(&folder, |keys| {
            keys.remove(id);
        })?,
    }
    write_private(&folder.join("accounts.json"), &json(&index)?)?;
    Ok(true)
}

fn file(var: Var<'_>) -> BTreeMap<String, String> {
    crate::machine::settings_folder(var)
        .and_then(|folder| fs::read_to_string(folder.join("credentials.json")).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// fetchloom's own key file or index that is there but cannot be read, with why: the keys it
/// holds read as none until it is fixed, so `auth` and a run that looks for keys warn of it.
pub(crate) fn unreadable(var: Var<'_>) -> Option<crate::error::Error> {
    let folder = crate::machine::settings_folder(var)?;
    let index = folder.join("accounts.json");
    let keys = folder.join("credentials.json");
    let (path, why) = problem::<BTreeMap<String, Place>>(&index)
        .map(|why| (index, why))
        .or_else(|| problem::<BTreeMap<String, String>>(&keys).map(|why| (keys, why)))?;
    Some(
        crate::error::Error::new(
            crate::error::Kind::AuthRequired,
            format!(
                "{} {why}, so fetchloom uses none of the keys saved in it",
                crate::units::path(&path)
            ),
        )
        .help("fix it, or delete it and sign in again"),
    )
}

/// why the file at `path` cannot be read as `T`, as a clause after its name: `is not valid JSON
/// (…)` or `could not be read: …`. none when it can, or is not there.
fn problem<T: serde::de::DeserializeOwned>(path: &Path) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<T>(&text)
            .err()
            .map(|err| format!("is not valid JSON ({})", stopped(&err))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => Some(format!("could not be read: {}", crate::error::said(&err))),
    }
}

/// what `path` holds, empty when it does not exist. a file that cannot be read or is not valid
/// JSON is an error, so writing it back never drops the keys it held.
fn load<T: serde::de::DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is not valid JSON ({}); fix or delete it",
                    path.display(),
                    stopped(&err)
                ),
            )
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(err),
    }
}

/// where reading a key file stopped, and why in a word, as `a syntax error at line 1 column 2`.
/// `serde_json`'s own words may quote what the file holds, a key among it.
fn stopped(err: &serde_json::Error) -> String {
    let what = match err.classify() {
        serde_json::error::Category::Syntax => "a syntax error",
        serde_json::error::Category::Data => "an unexpected value",
        serde_json::error::Category::Eof => "an early end",
        serde_json::error::Category::Io => "a read error",
    };
    format!("{what} at line {} column {}", err.line(), err.column())
}

fn json<T: serde::Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// writes `bytes` to `path` through a new file beside it, so a crash leaves the old file or the
/// new one, and nobody can plant a file or a link in its place. only its owner may read it on
/// unix; on windows it takes the folder's permissions, which keep others out of profile folders.
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
    use std::cell::RefCell;
    use std::ffi::OsString;

    use super::*;

    /// a store that cannot ask for its password counts as locked, so the help says to unlock it;
    /// any other failure does not.
    #[test]
    fn a_store_that_cannot_ask_for_its_password_is_locked() {
        let failed = |err: keyring::Error| io::Error::other(err);
        assert!(locked(&failed(keyring::Error::NoStorageAccess(Box::new(
            io::Error::other("locked")
        )))));
        assert!(!locked(&failed(keyring::Error::PlatformFailure(Box::new(
            io::Error::other("User interaction is not allowed")
        )))));
        assert!(!locked(&failed(keyring::Error::NoEntry)));
        assert!(!locked(&io::Error::other("no store at all")));
    }

    /// macos answers errSecInteractionNotAllowed for a locked keychain no screen can unlock, as
    /// over ssh, which keyring's apple store reports as a platform failure; its code tells it,
    /// whatever language the message is in.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_keychain_that_no_screen_can_unlock_is_locked() {
        let failed = |code| {
            io::Error::other(keyring::Error::PlatformFailure(Box::new(
                security_framework::base::Error::from_code(code),
            )))
        };
        assert!(locked(&failed(-25308)));
        // errSecAuthFailed: a wrong password, not a lock
        assert!(!locked(&failed(-25293)));
    }

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

    /// letter case does not matter, and unset, empty, true or 1 leave the store in use.
    #[test]
    fn the_keyring_variable_turns_the_store_off_with_false_or_0() {
        for (value, allowed) in [
            (None, true),
            (Some(""), true),
            (Some("1"), true),
            (Some("True"), true),
            (Some("0"), false),
            (Some("false"), false),
            (Some(" FALSE "), false),
        ] {
            let var = |name: &str| {
                value
                    .filter(|_| name == "FETCHLOOM_KEYRING")
                    .map(OsString::from)
            };
            assert_eq!(keyring_allowed(&var), allowed, "{value:?}");
        }
    }

    /// the store is asked only once `FETCHLOOM_KEYRING` allows it and, for a store on the message
    /// bus, a bus is set. its answer then decides.
    #[test]
    fn the_keyring_is_asked_only_when_allowed_and_reachable() {
        let bus = Some("unix:path=/run/user/1000/bus");
        for (allowed, needs_bus, address, asked) in [
            (None, false, None, true),
            (Some("1"), true, bus, true),
            (Some("0"), false, None, false),
            (None, true, None, false),
            (None, true, Some(""), false),
        ] {
            let var = |name: &str| match name {
                "FETCHLOOM_KEYRING" => allowed.map(OsString::from),
                "DBUS_SESSION_BUS_ADDRESS" => address.map(OsString::from),
                _ => None,
            };
            let case =
                format!("FETCHLOOM_KEYRING {allowed:?}, needs a bus {needs_bus}, bus {address:?}");
            if asked {
                for answer in [true, false] {
                    let answers: fn() -> bool = if answer { || true } else { || false };
                    let store = Keyring {
                        needs_bus,
                        answers,
                        ..KEYRING
                    };
                    assert_eq!(store.usable(&var), answer, "{case}");
                }
            } else {
                let store = Keyring {
                    needs_bus,
                    answers: || panic!("the store was asked"),
                    ..KEYRING
                };
                assert!(!store.usable(&var), "{case}");
            }
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
        assert!(!KEYRING.usable(&var));
        let saved_key = |id: &str| saved(&var).get(id).and_then(|&place| read(&var, id, place));
        save(&var, "zenodo", "z-key", Place::File).unwrap();
        save(&var, "dataverse:demo.dataverse.org", "d-key", Place::File).unwrap();
        save(&var, "zenodo", "z-new", Place::File).unwrap();
        assert_eq!(saved_key("zenodo").as_deref(), Some("z-new"));
        assert_eq!(
            saved(&var).keys().map(String::as_str).collect::<Vec<_>>(),
            ["dataverse:demo.dataverse.org", "zenodo"]
        );
        assert!(forget(&var, "zenodo").unwrap());
        assert!(!forget(&var, "zenodo").unwrap());
        assert_eq!(saved_key("zenodo"), None);
        assert_eq!(
            saved_key("dataverse:demo.dataverse.org").as_deref(),
            Some("d-key")
        );
        let index = fs::read_to_string(
            crate::machine::settings_folder(&var)
                .unwrap()
                .join("accounts.json"),
        )
        .unwrap();
        assert!(!index.contains("d-key"), "the index holds no secrets");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let keys = crate::machine::settings_folder(&var)
                .unwrap()
                .join("credentials.json");
            assert_eq!(
                fs::metadata(keys).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // a damaged index, not JSON or not even UTF-8, is refused rather than written over, which
        // would lose the other keys
        let index = crate::machine::settings_folder(&var)
            .unwrap()
            .join("accounts.json");
        for damaged in [b"{ not json".as_slice(), b"\xff\xfe{\0}\0"] {
            fs::write(&index, damaged).unwrap();
            let error = save(&var, "osf", "o-key", Place::File).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(forget(&var, "dataverse:demo.dataverse.org").is_err());
            assert_eq!(fs::read(&index).unwrap(), damaged);
        }
        let keys = fs::read_to_string(
            crate::machine::settings_folder(&var)
                .unwrap()
                .join("credentials.json"),
        )
        .unwrap();
        assert!(keys.contains("d-key"));
    }

    /// sign-ins and sign-outs at once, as parallel `auth login` and `logout` runs make them, each
    /// keep their change: every key saved is there, and the index lists exactly the keys the file
    /// holds.
    #[test]
    fn keys_saved_and_forgotten_at_once_all_stay_as_left() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().as_os_str().to_owned();
        let var = |name: &str| -> Option<OsString> {
            match name {
                "HOME" | "USERPROFILE" | "APPDATA" | "XDG_CONFIG_HOME" => Some(root.clone()),
                _ => None,
            }
        };
        let ids: Vec<String> = (0..8).map(|n| format!("site:{n}.example")).collect();
        let at_once = |change: &(dyn Fn(usize, &str) + Sync)| {
            let start = std::sync::Barrier::new(ids.len());
            std::thread::scope(|scope| {
                for (n, id) in ids.iter().enumerate() {
                    let start = &start;
                    scope.spawn(move || {
                        start.wait();
                        change(n, id);
                    });
                }
            });
        };
        let kept = || {
            let index: Vec<(String, Place)> = saved(&var).into_iter().collect();
            let keys: Vec<(String, String)> = file(&var).into_iter().collect();
            (index, keys)
        };
        for round in 0..3 {
            at_once(&|_, id| save(&var, id, &format!("{id} {round}"), Place::File).unwrap());
            let all = ids.iter().map(|id| (id.clone(), format!("{id} {round}")));
            assert_eq!(
                kept(),
                (
                    ids.iter().map(|id| (id.clone(), Place::File)).collect(),
                    all.collect()
                ),
                "round {round}, every key saved"
            );
            at_once(&|n, id| {
                if n % 2 == 0 {
                    assert!(forget(&var, id).unwrap());
                } else {
                    save(&var, id, "again", Place::File).unwrap();
                }
            });
            let odd: Vec<&String> = ids.iter().skip(1).step_by(2).collect();
            assert_eq!(
                kept(),
                (
                    odd.iter().map(|&id| (id.clone(), Place::File)).collect(),
                    odd.iter()
                        .map(|&id| (id.clone(), "again".to_owned()))
                        .collect()
                ),
                "round {round}, half forgotten"
            );
            for id in odd {
                assert!(forget(&var, id).unwrap());
            }
        }
    }

    /// a credential store that panics once asked anything.
    const UNTOUCHED: Keyring = Keyring {
        needs_bus: false,
        answers: || panic!("the store was asked"),
        get: |_| panic!("the store was read"),
        set: |_, _| panic!("the store was written"),
        remove: |_| panic!("the store was written"),
    };

    /// with `FETCHLOOM_KEYRING` off, the store is never asked: a key the index lists there reads
    /// as none, a key saved in its place goes to the file alone, and removing it is refused.
    #[test]
    fn the_keyring_variable_keeps_every_read_and_write_out_of_the_store() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().as_os_str().to_owned();
        let var = |name: &str| -> Option<OsString> {
            match name {
                "HOME" | "USERPROFILE" | "APPDATA" | "XDG_CONFIG_HOME" => Some(root.clone()),
                "FETCHLOOM_KEYRING" => Some("false".into()),
                _ => None,
            }
        };
        let folder = crate::machine::settings_folder(&var).unwrap();
        fs::create_dir_all(&folder).unwrap();
        let listed = r#"{"osf": "keyring", "zenodo": "keyring"}"#;
        fs::write(folder.join("accounts.json"), listed).unwrap();
        assert_eq!(read_with(&UNTOUCHED, &var, "zenodo", Place::Keyring), None);
        assert_eq!(kept_out(&var), ["osf", "zenodo"]);
        assert!(forget_with(&UNTOUCHED, &var, "zenodo").is_err());
        assert_eq!(
            fs::read_to_string(folder.join("accounts.json")).unwrap(),
            listed
        );
        save_with(&UNTOUCHED, &var, "zenodo", "z-file", Place::File).unwrap();
        assert_eq!(saved(&var).get("zenodo"), Some(&Place::File));
        assert_eq!(file(&var).remove("zenodo").as_deref(), Some("z-file"));
        assert_eq!(kept_out(&var), ["osf"]);
        let on = |name: &str| (name != "FETCHLOOM_KEYRING").then(|| var(name)).flatten();
        assert_eq!(kept_out(&on), Vec::<String>::new());
    }

    /// a key file or index that does not read leaves every key in it unfound, so it is named, by
    /// where it stops reading and never by what it holds, which may be a key; one that is not
    /// there yet is no problem.
    #[test]
    fn a_key_file_that_does_not_read_is_named_without_what_it_holds() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().as_os_str().to_owned();
        let var = |name: &str| -> Option<OsString> {
            match name {
                "HOME" | "USERPROFILE" | "APPDATA" | "XDG_CONFIG_HOME" => Some(root.clone()),
                _ => None,
            }
        };
        assert!(unreadable(&var).is_none());
        let folder = crate::machine::settings_folder(&var).unwrap();
        fs::create_dir_all(&folder).unwrap();
        let index = folder.join("accounts.json");
        fs::write(&index, r#"{"zenodo": "secret"}"#).unwrap();
        let message = unreadable(&var).unwrap().message;
        assert!(
            message.contains(
                "accounts.json is not valid JSON (an unexpected value at line 1 column 19), so"
            ),
            "{message}"
        );
        assert!(!message.contains("secret"), "{message}");
        fs::write(&index, "{}").unwrap();
        let keys = folder.join("credentials.json");
        for (text, value) in [(r#""secret""#, "secret"), (r#"{"k": 123}"#, "123")] {
            fs::write(&keys, text).unwrap();
            let message = unreadable(&var).unwrap().message;
            assert!(
                message.contains("credentials.json is not valid JSON ("),
                "{message}"
            );
            assert!(!message.contains(value), "{message}");
            let refused = load::<BTreeMap<String, String>>(&keys).unwrap_err();
            assert!(!refused.to_string().contains(value), "{refused}");
        }
    }

    thread_local! {
        static HELD: RefCell<BTreeMap<String, String>> = RefCell::default();
    }

    /// a credential store that keeps its keys in memory, apart for each test's thread.
    const MEMORY: Keyring = Keyring {
        set: |id, secret| {
            HELD.with_borrow_mut(|held| held.insert(id.to_owned(), secret.to_owned()));
            Ok(())
        },
        remove: |id| {
            let removed = HELD.with_borrow_mut(|held| held.remove(id));
            Some(removed.map(drop).ok_or(keyring::Error::NoEntry))
        },
        ..KEYRING
    };

    /// saved again where it is, a key replaces itself; saved somewhere new, it leaves no copy
    /// where it was.
    #[test]
    fn a_key_saved_again_stays_in_one_place() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().as_os_str().to_owned();
        let var = |name: &str| match name {
            "HOME" | "USERPROFILE" | "APPDATA" | "XDG_CONFIG_HOME" => Some(root.clone()),
            _ => None,
        };
        let save = |secret: &str, place| save_with(&MEMORY, &var, "zenodo", secret, place).unwrap();
        let places = || {
            let held = HELD.with_borrow(|held| held.get("zenodo").cloned());
            (held, file(&var).remove("zenodo"))
        };
        save("one", Place::Keyring);
        save("two", Place::Keyring);
        assert_eq!(places(), (Some("two".to_owned()), None));
        save("three", Place::File);
        assert_eq!(places(), (None, Some("three".to_owned())));
        save("four", Place::Keyring);
        assert_eq!(places(), (Some("four".to_owned()), None));
    }
}

//! folders that onedrive, dropbox, icloud drive or google drive keep in sync. those clients
//! break hardlinks or upload each link as a whole file, so files placed there are copied.

use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// the sync client that owns `folder`, if any. `folder` must be absolute.
pub(super) fn client(
    folder: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Option<&'static str> {
    roots(var)
        .into_iter()
        .find(|(root, _)| inside(folder, root))
        .map(|(_, client)| client)
}

/// the folders each sync client owns on this machine, as far as can be told without asking it.
fn roots(var: &dyn Fn(&str) -> Option<OsString>) -> Vec<(PathBuf, &'static str)> {
    let path = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let mut roots = Vec::new();
    for name in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(root) = path(name) {
            roots.push((root, "onedrive"));
        }
    }
    let home = path("HOME").or_else(|| path("USERPROFILE"));
    // dropbox writes where its folders are into an info.json, in one of these places
    let dropbox = [
        path("APPDATA").map(|folder| folder.join("Dropbox")),
        path("LOCALAPPDATA").map(|folder| folder.join("Dropbox")),
        home.as_ref().map(|home| home.join(".dropbox")),
    ];
    for folder in dropbox.into_iter().flatten() {
        roots.extend(
            dropbox_roots(&folder.join("info.json"))
                .into_iter()
                .map(|root| (root, "dropbox")),
        );
    }
    if let Some(home) = &home {
        if cfg!(windows) {
            roots.push((home.join("iCloudDrive"), "icloud drive"));
        }
        if cfg!(target_os = "macos") {
            roots.extend(mac_roots(home));
        }
    }
    roots
}

fn mac_roots(home: &Path) -> Vec<(PathBuf, &'static str)> {
    let library = home.join("Library");
    let mut roots = vec![(library.join("Mobile Documents"), "icloud drive")];
    // macos keeps one folder here per file provider: OneDrive-…, Dropbox, GoogleDrive-…
    if let Ok(entries) = fs::read_dir(library.join("CloudStorage")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let client = if name.starts_with("onedrive") {
                "onedrive"
            } else if name.starts_with("dropbox") {
                "dropbox"
            } else if name.starts_with("googledrive") {
                "google drive"
            } else {
                "a sync client"
            };
            roots.push((entry.path(), client));
        }
    }
    // with "desktop and documents folders" turned on, icloud drive syncs both
    let documents = library.join("Mobile Documents").join("com~apple~CloudDocs");
    for name in ["Desktop", "Documents"] {
        if documents.join(name).is_dir() {
            roots.push((home.join(name), "icloud drive"));
        }
    }
    roots
}

fn dropbox_roots(info: &Path) -> Vec<PathBuf> {
    let Ok(text) = fs::read(info) else {
        return Vec::new();
    };
    let Ok(accounts) = serde_json::from_slice::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    accounts
        .as_object()
        .into_iter()
        .flat_map(serde_json::Map::values)
        .filter_map(|account| account.get("path")?.as_str())
        .map(PathBuf::from)
        .collect()
}

/// whether `folder` is `root` or inside it, comparing names the way the platform's disks do.
fn inside(folder: &Path, root: &Path) -> bool {
    let mut parts = folder.components();
    root.components()
        .all(|part| parts.next().is_some_and(|other| same(part, other)))
}

fn same(a: Component<'_>, b: Component<'_>) -> bool {
    if cfg!(any(windows, target_os = "macos")) {
        a.as_os_str().to_string_lossy().to_lowercase()
            == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.as_os_str().to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn onedrive_owns_what_is_under_its_folder() {
        let home = tempfile::tempdir().unwrap();
        let onedrive = home.path().join("OneDrive - Contoso");
        let var = env(&[("OneDriveCommercial", &onedrive)]);
        assert_eq!(
            client(&onedrive.join("thesis").join("data"), &var),
            Some("onedrive")
        );
        assert_eq!(client(&home.path().join("projects"), &var), None);
        assert_eq!(
            client(&home.path().join("OneDrive - Contoso2"), &var),
            None,
            "a folder with a longer name is not inside"
        );
    }

    #[test]
    fn dropbox_folders_come_from_its_info_file() {
        let home = tempfile::tempdir().unwrap();
        let personal = home.path().join("Dropbox");
        let business = home.path().join("Dropbox (Lab)");
        let info = home.path().join(".dropbox");
        fs::create_dir_all(&info).unwrap();
        let text = serde_json::json!({
            "personal": {"path": personal, "host": 1},
            "business": {"path": business, "host": 2},
        });
        fs::write(info.join("info.json"), text.to_string()).unwrap();
        let var = env(&[("HOME", home.path())]);
        assert_eq!(client(&personal.join("x"), &var), Some("dropbox"));
        assert_eq!(client(&business, &var), Some("dropbox"));
        assert_eq!(client(&home.path().join("elsewhere"), &var), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_compares_names_without_case() {
        let var = env(&[("OneDrive", Path::new(r"C:\Users\ada\OneDrive"))]);
        assert_eq!(
            client(Path::new(r"c:\users\ADA\onedrive\data"), &var),
            Some("onedrive")
        );
    }
}

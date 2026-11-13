//! folders that onedrive, dropbox, icloud drive or google drive keep in sync. those clients
//! break hardlinks or upload each link as a whole file, so files placed there are copied.

use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// the sync client that owns `folder`, if any. `folder` must be absolute. `mounts` reads the
/// table of mounted filesystems, in the form of linux's /proc/self/mounts.
pub(super) fn client(
    folder: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
    mounts: &dyn Fn() -> Option<String>,
) -> Option<&'static str> {
    let folder = resolved(folder);
    roots(var)
        .into_iter()
        .find(|(root, _)| inside(&folder, &resolved(root)))
        .map(|(_, client)| client)
        .or_else(|| mounted(&folder, &mounts()?))
}

/// the table of mounted filesystems, which linux lists in /proc/self/mounts.
pub(super) fn mounts() -> Option<String> {
    if cfg!(target_os = "linux") {
        fs::read_to_string("/proc/self/mounts").ok()
    } else {
        None
    }
}

/// google drive, when the deepest mount in `mounts` holding `folder` is of type `fuse.drive`, as
/// google colab mounts it, by default at /content/drive.
fn mounted(folder: &Path, mounts: &str) -> Option<&'static str> {
    let (_, kind) = mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let point = unescape(fields.nth(1)?);
            let kind = fields.next()?;
            inside(folder, &point).then(|| (point.components().count(), kind))
        })
        .max_by_key(|(depth, _)| *depth)?;
    (kind == "fuse.drive").then_some("google drive")
}

/// a mount point as the mount table writes it, with a space, tab, newline or backslash as an
/// octal escape such as `\040`.
fn unescape(field: &str) -> PathBuf {
    let mut bytes = Vec::with_capacity(field.len());
    let mut rest = field.as_bytes();
    while let Some((&first, after)) = rest.split_first() {
        if first == b'\\'
            && let Some(byte) = octal(after)
        {
            bytes.push(byte);
            rest = &after[3..];
        } else {
            bytes.push(first);
            rest = after;
        }
    }
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// the byte that the three octal digits `digits` starts with stand for.
fn octal(digits: &[u8]) -> Option<u8> {
    let value = digits.get(..3)?.iter().try_fold(0_u32, |value, digit| {
        (b'0'..=b'7')
            .contains(digit)
            .then(|| value * 8 + u32::from(digit - b'0'))
    })?;
    u8::try_from(value).ok()
}

/// `path` with the links in its existing part resolved, so two names for one folder compare
/// equal. a part that does not exist yet stays as written.
fn resolved(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(mut real) = fs::canonicalize(existing) {
            real.extend(rest.iter().rev());
            return real;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
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

/// the folders macos sync clients own under `home`: icloud drive's, and one per account in
/// `Library/CloudStorage` for file provider clients, as `OneDrive-Personal` or `Dropbox (Lab)`.
fn mac_roots(home: &Path) -> Vec<(PathBuf, &'static str)> {
    let library = home.join("Library");
    let mut roots = vec![(library.join("Mobile Documents"), "icloud drive")];
    let Ok(entries) = fs::read_dir(library.join("CloudStorage")) else {
        return roots;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy().to_ascii_lowercase();
        let client = if name.starts_with("onedrive") {
            "onedrive"
        } else if name.starts_with("dropbox") {
            "dropbox"
        } else {
            continue;
        };
        roots.push((entry.path(), client));
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
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase()
            == b.as_os_str().to_string_lossy().to_lowercase()
    } else if cfg!(target_os = "macos") {
        // apfs and hfs+ ignore case by default, and see one name in composed and decomposed
        // unicode, which hfs+ stores decomposed
        crate::name::fold(&a.as_os_str().to_string_lossy())
            == crate::name::fold(&b.as_os_str().to_string_lossy())
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a mount table that says nothing, as on a system without one.
    fn unmounted() -> Option<String> {
        None
    }

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
            client(&onedrive.join("thesis").join("data"), &var, &unmounted),
            Some("onedrive")
        );
        assert_eq!(
            client(&home.path().join("projects"), &var, &unmounted),
            None
        );
        assert_eq!(
            client(&home.path().join("OneDrive - Contoso2"), &var, &unmounted),
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
        assert_eq!(
            client(&personal.join("x"), &var, &unmounted),
            Some("dropbox")
        );
        assert_eq!(client(&business, &var, &unmounted), Some("dropbox"));
        assert_eq!(
            client(&home.path().join("elsewhere"), &var, &unmounted),
            None
        );
    }

    #[test]
    fn macos_clients_keep_their_folders_in_the_library() {
        let home = tempfile::tempdir().unwrap();
        let storage = home.path().join("Library").join("CloudStorage");
        for name in [
            "OneDrive-Personal",
            "OneDrive-Contoso",
            "Dropbox",
            "Dropbox (Lab)",
            "GoogleDrive-ada@example.org",
        ] {
            fs::create_dir_all(storage.join(name)).unwrap();
        }
        let mut roots = mac_roots(home.path());
        roots.sort();
        let library = home.path().join("Library");
        assert_eq!(
            roots,
            [
                (storage.join("Dropbox"), "dropbox"),
                (storage.join("Dropbox (Lab)"), "dropbox"),
                (storage.join("OneDrive-Contoso"), "onedrive"),
                (storage.join("OneDrive-Personal"), "onedrive"),
                (library.join("Mobile Documents"), "icloud drive"),
            ]
        );
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            mac_roots(empty.path()),
            [(
                empty.path().join("Library").join("Mobile Documents"),
                "icloud drive"
            )]
        );
    }

    #[test]
    fn mac_client_prefixes_ignore_case() {
        let home = tempfile::tempdir().unwrap();
        let cloud = home.path().join("Library/CloudStorage");
        let clients = [
            ("onedrive-personal", "onedrive"),
            ("DROPBOX-school", "dropbox"),
            ("oNeDrIvE-lab", "onedrive"),
            ("dRoPbOx-work", "dropbox"),
        ];
        for (folder, _) in clients {
            fs::create_dir_all(cloud.join(folder)).unwrap();
        }
        fs::create_dir_all(cloud.join("GoogleDrive-ada")).unwrap();
        let found = mac_roots(home.path());
        assert_eq!(found.len(), clients.len() + 1);
        for (folder, client) in clients {
            assert!(found.contains(&(cloud.join(folder), client)), "{found:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_finds_icloud_drive_and_cloud_storage_by_home() {
        let home = tempfile::tempdir().unwrap();
        let onedrive = home.path().join("Library/CloudStorage/OneDrive-Personal");
        fs::create_dir_all(&onedrive).unwrap();
        let var = env(&[("HOME", home.path())]);
        assert_eq!(
            client(&onedrive.join("thesis"), &var, &unmounted),
            Some("onedrive")
        );
        let icloud = home
            .path()
            .join("Library/Mobile Documents/com~apple~CloudDocs");
        assert_eq!(client(&icloud, &var, &unmounted), Some("icloud drive"));
        assert_eq!(
            client(&home.path().join("projects"), &var, &unmounted),
            None
        );
        assert_eq!(
            client(
                &home.path().join("library/cloudstorage/onedrive-personal"),
                &var,
                &unmounted
            ),
            Some("onedrive"),
            "names compare without case, as apfs compares them"
        );
    }

    #[test]
    fn a_folder_named_through_a_link_is_still_inside() {
        let home = tempfile::tempdir().unwrap();
        // FAT32 and exFAT can make neither a junction nor a hardlink, so no folder there is
        // named through a link
        if !crate::place::tests::makes_hardlinks(home.path()) {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }
        let onedrive = home.path().join("OneDrive");
        fs::create_dir(&onedrive).unwrap();
        let link = home.path().join("linked");
        crate::test_links::link_folder(&onedrive, &link).unwrap();
        let root_through_link = env(&[("OneDrive", &link)]);
        assert_eq!(
            client(&onedrive.join("data"), &root_through_link, &unmounted),
            Some("onedrive")
        );
        let folder_through_link = env(&[("OneDrive", &onedrive)]);
        assert_eq!(
            client(&link.join("data"), &folder_through_link, &unmounted),
            Some("onedrive")
        );
    }

    /// google colab mounts google drive at /content/drive as a fuse filesystem of type
    /// `fuse.drive`, as its `drive.mount` checks for in the mount table.
    #[cfg(unix)]
    #[test]
    fn colab_mounts_google_drive_as_a_synced_folder() {
        let table = "/dev/root / ext4 rw,relatime 0 0\n\
            drive /content/drive fuse.drive rw,nosuid,nodev,relatime,user_id=0,group_id=0,allow_other 0 0\n\
            drive /content/my\\040drive fuse.drive rw 0 0\n\
            tmpfs /content/drive/MyDrive/scratch tmpfs rw 0 0\n\
            remote /content/sshfs fuse.sshfs rw 0 0\n\
            odd /content/odd\\777 fuse.drive rw 0 0\n";
        let mounts = || Some(table.to_owned());
        let none = env(&[]);
        let on = |path: &str| client(Path::new(path), &none, &mounts);

        assert_eq!(
            on("/content/drive/MyDrive/thesis/data"),
            Some("google drive")
        );
        assert_eq!(on("/content/drive"), Some("google drive"));
        assert_eq!(on("/content/my drive/data"), Some("google drive"));
        assert_eq!(on("/content/thesis/data"), None);
        assert_eq!(
            on("/content/drive2/data"),
            None,
            "a longer name is not inside"
        );
        assert_eq!(
            on("/content/drive/MyDrive/scratch/data"),
            None,
            "a mount inside it"
        );
        assert_eq!(on("/content/sshfs/data"), None, "another fuse filesystem");
        assert_eq!(
            on("/content/odd\\777/data"),
            Some("google drive"),
            "an escape past one byte stays as written"
        );
        assert_eq!(
            client(Path::new("/content/drive/data"), &none, &unmounted),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_compares_names_without_case() {
        let var = env(&[("OneDrive", Path::new(r"C:\Users\ada\OneDrive"))]);
        assert_eq!(
            client(Path::new(r"c:\users\ADA\onedrive\data"), &var, &unmounted),
            Some("onedrive")
        );
    }
}

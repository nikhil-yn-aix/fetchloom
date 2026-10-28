//! file and folder names that windows, macos and linux can all store.

use std::path::Path;

use percent_encoding::percent_decode_str;
use url::Url;

/// the longest name kept, in bytes. most filesystems stop at 255.
const LONGEST: usize = 200;

/// a name a server or a link suggested, made safe to create anywhere: only its last path part,
/// with the characters windows refuses replaced and the names windows reserves avoided.
pub(crate) fn sanitize(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let replaced: String = last
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    // windows drops trailing dots and spaces, so two different names could become one
    let trimmed = replaced.trim().trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return None;
    }
    let name = shorten(trimmed);
    Some(if is_reserved_on_windows(&name) {
        format!("_{name}")
    } else {
        name
    })
}

/// the name of the file a URL points at: its last path segment, decoded.
pub(crate) fn from_url(url: &Url) -> Option<String> {
    let segment = url.path_segments()?.rfind(|segment| !segment.is_empty())?;
    sanitize(&percent_decode_str(segment).decode_utf8_lossy())
}

/// the folder `get` makes for a file: its name without the extension, and without a `.tar`
/// before it, so `iris.csv` gets `iris` and `data.tar.gz` gets `data`.
pub(crate) fn folder_for(file: &str) -> String {
    let stem = Path::new(file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file);
    let stem = match stem.len().checked_sub(4) {
        Some(at) if stem.is_char_boundary(at) && stem[at..].eq_ignore_ascii_case(".tar") => {
            &stem[..at]
        }
        _ => stem,
    };
    let stem = stem.trim_start_matches('.');
    if stem.is_empty() {
        "download".to_owned()
    } else {
        stem.to_owned()
    }
}

/// why a path inside a dataset or an archive could reach outside its folder on some platform,
/// if it could. such a path is refused everywhere, so a `data.lock` works on every machine.
pub(crate) fn escapes(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("it is empty");
    }
    if path.starts_with('/') {
        return Some("it starts at the root of the disk");
    }
    if path.contains('\\') {
        return Some("it has a \\, which windows reads as a folder separator");
    }
    if path.contains('\0') {
        return Some("it holds a NUL character");
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some("it names a windows drive");
    }
    for part in path.split('/') {
        if part.is_empty() {
            return Some("it has an empty folder name");
        }
        if part == "." || part == ".." {
            return Some("it climbs out of its folder");
        }
    }
    None
}

/// why this machine's disks cannot store a path, if they cannot. checked before anything is
/// written, since other platforms may store the same path fine.
pub(crate) fn unstorable(path: &str) -> Option<String> {
    for part in path.split('/') {
        if part.len() > 255 {
            return Some(format!("\"{}…\" is longer than disks allow", shorten(part)));
        }
        if cfg!(windows) {
            if let Some(c) = part
                .chars()
                .find(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
            {
                return Some(format!(
                    "\"{part}\" has {c:?}, which windows does not allow in names"
                ));
            }
            if part.ends_with(['.', ' ']) {
                return Some(format!(
                    "\"{part}\" ends with a dot or a space, which windows drops"
                ));
            }
            if is_reserved_on_windows(part) {
                return Some(format!("\"{part}\" is a name windows reserves for devices"));
            }
        }
    }
    None
}

fn shorten(name: &str) -> String {
    if name.len() <= LONGEST {
        return name.to_owned();
    }
    let extension = name
        .rfind('.')
        .map(|at| &name[at..])
        .filter(|extension| extension.len() <= 20)
        .unwrap_or_default();
    let mut end = LONGEST - extension.len();
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{extension}", &name[..end])
}

fn is_reserved_on_windows(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            matches!(
                rest,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    };
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered("COM")
        || numbered("LPT")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(text: &str) -> Option<String> {
        from_url(&Url::parse(text).unwrap())
    }

    #[test]
    fn urls_are_named_by_their_last_segment() {
        assert_eq!(
            named("https://zenodo.org/records/1/files/iris.csv?download=1").as_deref(),
            Some("iris.csv")
        );
        assert_eq!(
            named("https://example.org/my%20data.csv").as_deref(),
            Some("my data.csv")
        );
        assert_eq!(named("https://example.org/dir/").as_deref(), Some("dir"));
        assert_eq!(named("https://example.org/"), None);
    }

    #[test]
    fn names_from_servers_cannot_escape_or_trip_up_windows() {
        assert_eq!(sanitize("../../.bashrc").as_deref(), Some(".bashrc"));
        assert_eq!(
            sanitize("C:\\Windows\\evil.dll").as_deref(),
            Some("evil.dll")
        );
        assert_eq!(sanitize("a:b*c?.txt").as_deref(), Some("a_b_c_.txt"));
        assert_eq!(sanitize("notes.txt. . ").as_deref(), Some("notes.txt"));
        assert_eq!(sanitize("con.csv").as_deref(), Some("_con.csv"));
        assert_eq!(sanitize("COM1").as_deref(), Some("_COM1"));
        assert_eq!(sanitize("com0").as_deref(), Some("com0"));
        assert_eq!(sanitize(".."), None);
        assert_eq!(sanitize(" "), None);
    }

    #[test]
    fn long_names_keep_their_extension() {
        let long = format!("{}.csv", "é".repeat(150));
        let short = sanitize(&long).unwrap();
        assert!(short.len() <= LONGEST);
        assert_eq!(Path::new(&short).extension(), Some("csv".as_ref()));
    }

    #[test]
    fn folders_drop_the_extension() {
        assert_eq!(folder_for("iris.csv"), "iris");
        assert_eq!(folder_for("data.tar.gz"), "data");
        assert_eq!(folder_for("data.TAR.zst"), "data");
        assert_eq!(folder_for("release-v1.2.zip"), "release-v1.2");
        assert_eq!(folder_for("README"), "README");
        assert_eq!(folder_for(".hidden"), "hidden");
    }

    #[test]
    fn paths_that_could_escape_are_caught_on_every_platform() {
        for safe in ["a.csv", "a/b/c.csv", "ünïcode/名前.txt", "con.csv", "ab:c"] {
            assert_eq!(escapes(safe), None, "{safe}");
        }
        // on windows `a:b` is a path on drive A, so it leaves the folder
        for unsafe_path in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a//b",
            "a/./b",
            "a\\b",
            "C:x",
            "c:/x",
            "a:b",
            "a\0b",
            "a/",
        ] {
            assert!(escapes(unsafe_path).is_some(), "{unsafe_path:?}");
        }
    }

    #[test]
    fn storable_depends_on_the_platform() {
        assert_eq!(unstorable("data/train.csv"), None);
        assert!(unstorable(&"x".repeat(256)).is_some());
        assert_eq!(
            unstorable("2020-01-01T10:00:00.csv").is_some(),
            cfg!(windows)
        );
        assert_eq!(unstorable("data/CON.txt").is_some(), cfg!(windows));
        assert_eq!(unstorable("notes. ").is_some(), cfg!(windows));
    }
}

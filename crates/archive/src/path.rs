//! Path validation the reader can decide from headers alone, with no knowledge
//! of the destination volume.

use fetchloom_engine::error::{Error, ErrorKind};

fn unsafe_path(member: &str, next_action: String) -> Error {
    Error::new(ErrorKind::ArchiveUnsafePath, next_action).with_member(member)
}

/// # Errors
/// `archive.unsafe_path` for a name holding a NUL, not valid UTF-8, absolute,
/// climbing out with a relative component, or nested past the limit.
pub fn validate_member_path(raw: &[u8], nesting_limit: u32) -> Result<String, Error> {
    if raw.contains(&0) {
        let lossy = String::from_utf8_lossy(raw);
        return Err(unsafe_path(
            &lossy,
            format!("member path \"{lossy}\" holds a NUL byte"),
        ));
    }
    let Ok(text) = std::str::from_utf8(raw) else {
        let lossy = String::from_utf8_lossy(raw);
        return Err(unsafe_path(
            &lossy,
            format!("member path \"{lossy}\" is not valid UTF-8"),
        ));
    };
    let path = text.to_owned();
    if path.starts_with('/') {
        return Err(unsafe_path(
            &path,
            format!("member path \"{path}\" is an absolute path"),
        ));
    }
    if is_drive_letter_prefixed(&path) {
        return Err(unsafe_path(
            &path,
            format!("member path \"{path}\" names a Windows drive location"),
        ));
    }
    if path.contains('\\') {
        return Err(unsafe_path(
            &path,
            format!(
                "repack \"{path}\" with a writer that separates path components with a forward slash, because a backslash is a legal character in a member name and this archive gives no way to tell a separator from one"
            ),
        ));
    }
    let components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if components.contains(&"..") {
        return Err(unsafe_path(
            &path,
            format!("member path \"{path}\" holds a parent-directory component"),
        ));
    }
    let depth = u32::try_from(components.len()).unwrap_or(u32::MAX);
    if depth > nesting_limit {
        return Err(unsafe_path(
            &path,
            format!(
                "member path \"{path}\" nests {depth} components deep, past the limit of {nesting_limit}"
            ),
        ));
    }
    Ok(path)
}

pub(crate) fn claim_member_path(
    claimed: &mut std::collections::BTreeSet<String>,
    path: &str,
) -> Result<(), Error> {
    if claimed.insert(path.to_owned()) {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::ArchiveCollision,
        format!("member path \"{path}\" is claimed by two entries of this archive"),
    ))
}

pub(crate) fn validate_link_target(member: &str, raw: &[u8]) -> Result<(), Error> {
    let target = String::from_utf8_lossy(raw);
    let escape = |reason: &str| {
        Err(Error::new(
            ErrorKind::ArchiveLinkEscape,
            format!("link \"{member}\" points at \"{target}\", which {reason}"),
        ))
    };
    if target.starts_with('/') {
        return escape("is an absolute path");
    }
    if is_drive_letter_prefixed(&target) {
        return escape("names a Windows drive location");
    }
    let mut depth = member.split('/').count().saturating_sub(1);
    for component in target.split(['/', '\\']) {
        match component {
            ".." => {
                let Some(above) = depth.checked_sub(1) else {
                    return escape("climbs above the destination root");
                };
                depth = above;
            }
            "." | "" => {}
            _ => depth += 1,
        }
    }
    Ok(())
}

fn is_drive_letter_prefixed(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test assertions, where a failed read is the failure being asserted"
)]
mod tests {
    use super::validate_member_path;

    #[test]
    fn accepts_an_ordinary_path() {
        assert_eq!(
            validate_member_path(b"docs/readme.txt", 64).unwrap(),
            "docs/readme.txt"
        );
    }

    #[test]
    fn rejects_an_absolute_path() {
        let error = validate_member_path(b"/etc/passwd", 64).unwrap_err();
        assert_eq!(error.kind().label(), "archive.unsafe_path");
    }

    #[test]
    fn rejects_a_dotdot_component() {
        let error = validate_member_path(b"a/../b", 64).unwrap_err();
        assert_eq!(error.kind().label(), "archive.unsafe_path");
    }

    #[test]
    fn rejects_a_backslash() {
        let error = validate_member_path(b"a\\b", 64).unwrap_err();
        assert_eq!(error.kind().label(), "archive.unsafe_path");
    }

    #[test]
    fn rejects_a_drive_letter() {
        let error = validate_member_path(b"C:/a", 64).unwrap_err();
        assert_eq!(error.kind().label(), "archive.unsafe_path");
    }

    #[test]
    fn rejects_invalid_utf8() {
        let error = validate_member_path(b"bad\xFF\xFE.txt", 64).unwrap_err();
        assert_eq!(error.kind().label(), "archive.unsafe_path");
    }

    #[test]
    fn rejects_an_embedded_nul() {
        let error = validate_member_path(b"bad\0name.txt", 64).unwrap_err();
        assert_eq!(error.kind().label(), "archive.unsafe_path");
    }

    #[test]
    fn rejects_a_path_deeper_than_the_limit() {
        let deep = "a/".repeat(65) + "file.txt";
        let error = validate_member_path(deep.as_bytes(), 64).unwrap_err();
        assert_eq!(error.kind().label(), "archive.unsafe_path");
    }

    #[test]
    fn does_not_reject_a_windows_reserved_name() {
        assert!(validate_member_path(b"CON", 64).is_ok());
        assert!(validate_member_path(b"weird:name.txt", 64).is_ok());
    }
}

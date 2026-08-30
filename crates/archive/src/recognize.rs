//! Format recognition: what a manifest or a location name claims, checked
//! against what the archive's own leading bytes say.

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::manifest::ArchiveFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Tar,
    Gzip,
    Zstd,
    Xz,
    Bzip2,
    Zip,
}

impl Family {
    fn label(self) -> &'static str {
        match self {
            Self::Tar => "tar",
            Self::Gzip => "gzip",
            Self::Zstd => "zstd",
            Self::Xz => "xz",
            Self::Bzip2 => "bzip2",
            Self::Zip => "zip",
        }
    }

    fn of(format: ArchiveFormat) -> Self {
        match format {
            ArchiveFormat::Tar => Self::Tar,
            ArchiveFormat::TarGzip | ArchiveFormat::Gzip => Self::Gzip,
            ArchiveFormat::TarZstd | ArchiveFormat::Zstd => Self::Zstd,
            ArchiveFormat::TarXz | ArchiveFormat::Xz => Self::Xz,
            ArchiveFormat::TarBzip2 | ArchiveFormat::Bzip2 => Self::Bzip2,
            ArchiveFormat::Zip => Self::Zip,
        }
    }
}

const EXTENSIONS: &[(&str, ArchiveFormat)] = &[
    (".tar.gz", ArchiveFormat::TarGzip),
    (".tar.zst", ArchiveFormat::TarZstd),
    (".tar.xz", ArchiveFormat::TarXz),
    (".tar.bz2", ArchiveFormat::TarBzip2),
    (".tgz", ArchiveFormat::TarGzip),
    (".tbz2", ArchiveFormat::TarBzip2),
    (".zip", ArchiveFormat::Zip),
    (".gz", ArchiveFormat::Gzip),
    (".zst", ArchiveFormat::Zstd),
    (".xz", ArchiveFormat::Xz),
    (".bz2", ArchiveFormat::Bzip2),
    (".tar", ArchiveFormat::Tar),
];

/// Reads the archive format a location's final extensions name.
///
/// Takes the location as written. Returns the format the longest matching
/// extension names, or nothing when no known extension matches.
#[must_use]
pub fn format_from_extension(location: &str) -> Option<ArchiveFormat> {
    EXTENSIONS
        .iter()
        .find(|(suffix, _)| location.ends_with(suffix))
        .map(|(_, format)| *format)
}

fn sniff(header: &[u8]) -> Option<Family> {
    if header.starts_with(&[0x1f, 0x8b]) {
        return Some(Family::Gzip);
    }
    if header.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some(Family::Zstd);
    }
    if header.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some(Family::Xz);
    }
    if header.starts_with(b"BZh") {
        return Some(Family::Bzip2);
    }
    if header.starts_with(&[0x50, 0x4B, 0x03, 0x04])
        || header.starts_with(&[0x50, 0x4B, 0x05, 0x06])
    {
        return Some(Family::Zip);
    }
    if header.len() >= 262 && &header[257..262] == b"ustar" {
        return Some(Family::Tar);
    }
    None
}

/// Decides what format an archive is, or that it is not an archive at all.
///
/// Takes the format a manifest declared, when one did; the location the
/// reference names; and the archive's own leading bytes. Returns the format
/// when the manifest or the location's extensions claim one and the bytes
/// agree. Returns nothing when neither the manifest nor a known extension
/// claims a format, meaning the reference is not an archive and materializes
/// as one file. Fails with `archive.unsupported` naming what the name said
/// and what the bytes said when a claimed format and the bytes disagree.
///
/// # Errors
///
/// Fails when a format is claimed and the leading bytes do not agree with it.
pub fn recognize(
    declared: Option<ArchiveFormat>,
    location: &str,
    header: &[u8],
) -> Result<Option<ArchiveFormat>, Error> {
    let Some(claimed) = declared.or_else(|| format_from_extension(location)) else {
        return Ok(None);
    };
    let expected_family = Family::of(claimed);
    let observed = sniff(header);
    if observed == Some(expected_family) {
        return Ok(Some(claimed));
    }
    let observed_label = observed.map_or("unrecognized bytes", Family::label);
    Err(Error::new(
        ErrorKind::ArchiveUnsupported,
        format!(
            "rename it or state its format in a manifest, because the name said \"{}\" and the archive's bytes said {observed_label}",
            claimed.label()
        ),
    ))
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test assertions, where a failed read is the failure being asserted"
)]
mod tests {
    use super::{format_from_extension, recognize};
    use fetchloom_engine::manifest::ArchiveFormat;

    #[test]
    fn reads_the_longest_matching_extension() {
        assert_eq!(
            format_from_extension("silesia.tar.gz"),
            Some(ArchiveFormat::TarGzip)
        );
        assert_eq!(
            format_from_extension("silesia.tgz"),
            Some(ArchiveFormat::TarGzip)
        );
        assert_eq!(
            format_from_extension("silesia.tar"),
            Some(ArchiveFormat::Tar)
        );
        assert_eq!(format_from_extension("silesia.bin"), None);
    }

    #[test]
    fn an_unknown_extension_with_no_manifest_is_not_an_archive() {
        let bytes = [0x50, 0x4B, 0x03, 0x04];
        assert_eq!(recognize(None, "silesia.bin", &bytes).unwrap(), None);
    }

    #[test]
    fn agreement_between_extension_and_bytes_succeeds() {
        let bytes = [0x1f, 0x8b, 0x08, 0x00];
        assert_eq!(
            recognize(None, "silesia.gz", &bytes).unwrap(),
            Some(ArchiveFormat::Gzip)
        );
    }

    #[test]
    fn disagreement_between_name_and_bytes_is_rejected() {
        let bytes = [0x50, 0x4B, 0x03, 0x04];
        let error = recognize(None, "silesia.gz", &bytes).unwrap_err();
        assert_eq!(error.kind().label(), "archive.unsupported");
    }

    #[test]
    fn a_manifest_declared_format_is_checked_against_the_bytes_too() {
        let bytes = [0x50, 0x4B, 0x03, 0x04];
        let error = recognize(Some(ArchiveFormat::Gzip), "anything", &bytes).unwrap_err();
        assert_eq!(error.kind().label(), "archive.unsupported");
    }
}

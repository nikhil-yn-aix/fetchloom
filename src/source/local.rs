//! text that names something on this machine rather than data published elsewhere: a path such
//! as `./x.csv`, `~/data` or `C:\data`, a `file:` link, or a bare word that names a file or
//! folder in the current folder. fetchloom never searches for these or sends them anywhere.

use std::path::Path;

use crate::error::{Error, Kind};
use crate::net;

/// whether `text` is written as a path on this machine: from the root, `.`, `..` or `~`, on a
/// drive such as `C:`, or as a `file:` link. this reads the text alone, never the disk, so a
/// reference in data.toml means the same in every folder.
pub(crate) fn is_path(text: &str) -> bool {
    let text = text.trim_start();
    let relative = [".", ".."].iter().any(|dots| {
        text.strip_prefix(dots)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
    });
    // no reference prefix has one letter, so a letter and a colon can only be a drive
    let drive = matches!(text.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
    text.starts_with(['/', '\\', '~'])
        || relative
        || drive
        || text
            .get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
}

/// what `word` names in `folder`, `file` or `folder`, when it names something there. a word that
/// could be a search is looked up here first, so the name of a local file never leaves the
/// machine as search words.
pub(crate) fn here(word: &str, folder: &Path) -> Option<&'static str> {
    let path = folder.join(word);
    std::fs::symlink_metadata(&path).ok()?;
    Some(if path.is_dir() { "folder" } else { "file" })
}

/// the refusal for `text`, a path on this machine given where a reference belongs.
pub(crate) fn refused(text: &str) -> Error {
    Error::new(
        Kind::ReferenceInvalid,
        format!(
            "\"{}\" is a path on this machine, and fetchloom downloads data from links, DOIs and ids",
            net::mask_query(text.trim())
        ),
    )
    .help("give the link, DOI or id where the data is published, such as https://zenodo.org/records/3608990")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_on_this_machine_are_never_search_words() {
        for (text, path) in [
            ("/home/ada/flowers.csv", true),
            ("/", true),
            ("./flowers.csv", true),
            ("../flowers", true),
            (".", true),
            ("..", true),
            ("~", true),
            ("~/Downloads/flowers.csv", true),
            ("~ada/flowers.csv", true),
            (".\\flowers.csv", true),
            ("..\\flowers", true),
            ("\\\\lab\\share\\flowers.csv", true),
            ("C:\\data\\flowers.csv", true),
            ("c:/data", true),
            ("D:", true),
            ("d:flowers.csv", true),
            ("file:///etc/hostname", true),
            ("FILE:/etc/hostname", true),
            ("file://lab/share/flowers.csv", true),
            ("  ./flowers.csv", true),
            ("iris", false),
            ("fashion mnist", false),
            ("uciml/iris", false),
            (".hidden", false),
            ("...", false),
            ("zenodo:3608990", false),
            ("hf:datasets/scikit-learn/iris", false),
            ("https://example.org/flowers.csv", false),
            ("s3://bucket/flowers/", false),
            ("10.5281/zenodo.3608990", false),
            ("doi:10.5281/zenodo.3608990", false),
            ("", false),
        ] {
            assert_eq!(is_path(text), path, "{text:?}");
            assert!(
                !(path && crate::source::search::is_search(text)),
                "{text:?} would be searched for"
            );
        }
    }

    #[test]
    fn a_path_is_refused_as_a_reference_with_what_to_give_instead() {
        for text in [
            "./flowers.csv",
            "~/Downloads/flowers.csv",
            "C:\\data\\flowers.csv",
            "file:///etc/hostname",
        ] {
            let error = crate::source::Reference::parse(text).unwrap_err();
            assert_eq!(error.kind, crate::error::Kind::ReferenceInvalid, "{text}");
            assert_eq!(
                error.message,
                format!(
                    "\"{text}\" is a path on this machine, and fetchloom downloads data from links, DOIs and ids"
                )
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "give the link, DOI or id where the data is published, such as https://zenodo.org/records/3608990"
                )
            );
        }
    }
}

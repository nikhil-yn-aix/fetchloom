//! text that names something on this machine: a path such as `./x.csv` or `C:\data`, a `file:`
//! link, or a bare word naming a file here. fetchloom never searches for these or sends them
//! anywhere; `fetchloom add <path>` imports them under the ref [`IMPORTED`].

use std::path::Path;

use crate::error::{Error, Kind};
use crate::net;

/// the ref of a dataset imported from files on a machine, in data.toml and data.lock alike. it
/// names no path, which would mean nothing on another machine and say where the files were.
pub(crate) const IMPORTED: &str = "local";

/// whether `reference`, as data.toml or data.lock writes it, is that of an import.
pub(crate) fn is_imported(reference: &str) -> bool {
    reference == IMPORTED
}

/// the refusal to resolve an import: no source lists its files.
pub(crate) fn unlisted() -> Error {
    Error::new(
        Kind::ReferenceUnresolved,
        "\"local\" names files imported from a folder, which no source can list again",
    )
    .help(format!(
        "run `{}` where the files are",
        crate::suggested::command("add <folder> --as <name>")
    ))
}

/// the refusal of a changed data.toml entry of `name`, an import: only importing the files
/// again can pin other ones.
pub(crate) fn changed(name: &str) -> Error {
    Error::new(
        Kind::ReferenceUnresolved,
        format!("{name} was imported from files, so a change to its entry in data.toml cannot pin other files"),
    )
    .help(format!(
        "put datasets.{name} back as it was, or run `{}`, then `{}` where the files are",
        crate::suggested::command(&format!("remove {name}")),
        crate::suggested::command(&format!("add <folder> --as {name}"))
    ))
}

/// the refusal of `name`, an import that data.lock holds no pin of: nothing tells which files
/// it was.
pub(crate) fn unpinned(name: &str) -> Error {
    Error::new(
        Kind::ReferenceUnresolved,
        format!("{name} was imported from files, and data.lock holds no pin of them"),
    )
    .help(format!(
        "restore data.lock from git, or run `{}` where the files are",
        crate::suggested::command(&format!("add <folder> --as {name}"))
    ))
}

/// the refusal to upgrade `name`, an import.
pub(crate) fn no_newer(name: &str) -> Error {
    Error::new(
        Kind::ReferenceUnresolved,
        format!("{name} was imported from files, and no source has a newer version of them"),
    )
    .help(format!(
        "to pin other files, run `{}`, then `{}` where they are",
        crate::suggested::command(&format!("remove {name}")),
        crate::suggested::command(&format!("add <folder> --as {name}"))
    ))
}

/// the error for files of `name`, an import, that this machine's cache lacks, as a clone's does,
/// and the mirrors too when `mirrored`: no link fetches them. `folder` is where they go.
pub(crate) fn imported_elsewhere(name: &str, folder: &str, mirrored: bool) -> Error {
    let lacking = if mirrored {
        "neither this machine's cache nor a mirror holds them"
    } else {
        "this machine's cache does not hold them"
    };
    Error::new(
        Kind::ReferenceUnresolved,
        format!("{name} was imported from files, and {lacking}"),
    )
    .help(format!(
        "copy them to {folder} by hand, then run `{}`, or run `{}` where they are",
        crate::suggested::command("sync"),
        crate::suggested::command(&format!("add <folder> --as {name}"))
    ))
}

/// [`imported_elsewhere`] for several imports, `names`, whose files this machine lacks, in one
/// error, so a teammate supplies them all in one round rather than learn of one a run.
pub(crate) fn several_imported_elsewhere(names: &[&str], mirrored: bool) -> Error {
    let lacking = if mirrored {
        "neither this machine's cache nor a mirror holds them"
    } else {
        "this machine's cache does not hold them"
    };
    Error::new(
        Kind::ReferenceUnresolved,
        format!(
            "{} were imported from files, and {lacking}",
            crate::units::listed(names, "and")
        ),
    )
    .help(format!(
        "copy each one's files to its folder by hand, then run `{}`, or run `{}` where they are",
        crate::suggested::command("sync"),
        crate::suggested::command("add <folder> --as <name>")
    ))
}

/// whether `text` is written as a path on this machine, from the root, `.`, `..`, `~`, a drive
/// or `file:`. it reads the text alone, never the disk, so data.toml means the same everywhere.
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

/// whether `word`, though [`is_path`] does not read it as a path, is still written as one: it ends
/// with `/`, holds two `/` or a `\`, or its last name ends with a file type, as `visits.csv` does.
/// a link, an id and a DOI are none of these, and such a word is never sent off as search words.
pub(crate) fn reads_as_path(word: &str) -> bool {
    let word = word.trim();
    let last = word.rsplit(['/', '\\']).next().unwrap_or_default();
    !word.contains(':')
        && super::doi::Doi::bare(word).is_none()
        && (word.ends_with('/')
            || word.matches('/').count() > 1
            || word.contains('\\')
            || crate::name::has_file_type(last))
}

/// what `word` names in `folder`, `file` or `folder`, when it names something there, so a local
/// file's name is never sent off as search words.
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

//! file and folder names that windows and linux can both store.

use std::borrow::Cow;
use std::path::Path;

use icu_normalizer::properties::CanonicalCombiningClassMapBorrowed;
use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use percent_encoding::percent_decode_str;
use url::Url;

/// the longest name kept, in bytes. most filesystems stop at 255.
const LONGEST: usize = 200;

/// the most characters a dataset name has.
pub(crate) const DATASET_NAME_MOST: usize = 64;

/// a name a server or a link suggested, made safe to create anywhere: only its last path part,
/// with the characters windows refuses replaced and the names windows reserves avoided.
pub(crate) fn sanitize(raw: &str) -> Option<String> {
    if raw.contains(is_bidi_control) {
        return None;
    }
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
    // windows drops trailing dots and spaces, so two different names could become one. cutting a
    // long name can leave new ones at its end, so they go again once it is short
    let trimmed = replaced.trim().trim_end_matches(['.', ' ']);
    let name = shorten(trimmed);
    let name = name.trim_end_matches(['.', ' ']);
    if name.is_empty() {
        return None;
    }
    Some(if is_reserved_on_windows(name) {
        format!("_{name}")
    } else {
        name.to_owned()
    })
}

/// the name of the file a URL points at: its last path segment, decoded.
pub(crate) fn from_url(url: &Url) -> Option<String> {
    let segment = url.path_segments()?.rfind(|segment| !segment.is_empty())?;
    sanitize(&percent_decode_str(segment).decode_utf8_lossy())
}

/// a dataset name made from free text, such as a title or a file name: lowercase letters,
/// digits, `-` and `_`, at most 40 characters, ending at a word break when one is near. a letter
/// with an accent keeps its letter, as `ï` becomes `i`, and a name windows keeps for a device
/// gets `-data`, as `con` becomes `con-data`.
pub(crate) fn dataset_name(text: &str) -> Option<String> {
    let mut name = String::with_capacity(text.len());
    for c in plain_letters(text).chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            name.push(c);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    // a name may not start with `_`, as a hidden `_data.csv` would make it
    let mut name = name
        .trim_start_matches(['-', '_'])
        .trim_end_matches('-')
        .to_owned();
    if name.len() > 40 {
        // names are ascii here, so byte offsets are character offsets
        let cut = name[..=40].rfind('-').filter(|&at| at >= 20).unwrap_or(40);
        name.truncate(cut);
        let kept = name.trim_end_matches('-').len();
        name.truncate(kept);
    }
    (!name.is_empty()).then(|| unreserved(name))
}

/// `name`, with `-data` after it where windows keeps it for a device.
fn unreserved(name: String) -> String {
    if is_reserved_on_windows(&name) {
        format!("{name}-data")
    } else {
        name
    }
}

/// words a cut title should not end on.
const DANGLING: [&str; 13] = [
    "a", "an", "and", "at", "by", "for", "from", "in", "of", "on", "the", "to", "with",
];

/// a dataset name made from a title: at most its first four words and 32 characters, without
/// a dangling "and" or "of" at the end. a first word too long for that is cut as
/// [`dataset_name`] cuts any text, at 40 characters, and a device name gets `-data` as there.
pub(crate) fn title_name(title: &str) -> Option<String> {
    let lower = plain_letters(title);
    let words = lower
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        .map(|word| word.trim_start_matches('_'))
        .filter(|word| !word.is_empty());
    let mut kept: Vec<&str> = Vec::new();
    let mut length = 0;
    for word in words {
        let grown = length + usize::from(!kept.is_empty()) + word.len();
        if kept.len() == 4 || grown > 32 {
            break;
        }
        kept.push(word);
        length = grown;
    }
    while kept.len() > 1 && kept.last().is_some_and(|word| DANGLING.contains(word)) {
        kept.pop();
    }
    if kept.is_empty() {
        return dataset_name(title);
    }
    Some(unreserved(kept.join("-")))
}

/// a dataset name made from a reference: its last part when that holds a letter, as iris for
/// hf:datasets/scikit-learn/iris, else the whole reference without its version.
pub(crate) fn reference_name(reference: &str) -> Option<String> {
    let bare = reference.split('@').next().unwrap_or(reference);
    let last = bare
        .rsplit(['/', ':'])
        .find(|part| !part.is_empty())
        .unwrap_or(bare);
    if last.chars().any(|c| c.is_ascii_alphabetic())
        && let Some(name) = dataset_name(last)
    {
        return Some(name);
    }
    dataset_name(bare)
}

pub(crate) fn is_dataset_name(text: &str) -> bool {
    dataset_name_problem(text).is_none()
}

/// why `text` cannot be a dataset name, in plain words, or none when it can be one. a name is
/// also a folder on every machine that syncs the project, so it keeps to characters every
/// filesystem stores alike and is none of the names windows keeps for devices.
pub(crate) fn dataset_name_problem(text: &str) -> Option<String> {
    let count = text.chars().count();
    if count == 0 {
        return Some("it is empty".to_owned());
    }
    if count > DATASET_NAME_MOST {
        return Some(format!(
            "it has {count} characters, and the most is {DATASET_NAME_MOST}"
        ));
    }
    let mut capitals = false;
    let mut spaces = 0;
    let mut others: Vec<char> = Vec::new();
    for c in text.chars() {
        match c {
            'a'..='z' | '0'..='9' | '-' | '_' => {}
            'A'..='Z' => capitals = true,
            ' ' => spaces += 1,
            other if !others.contains(&other) => others.push(other),
            _ => {}
        }
    }
    let mut held: Vec<String> = Vec::new();
    if capitals {
        held.push("capital letters".to_owned());
    }
    match spaces {
        0 => {}
        1 => held.push("a space".to_owned()),
        _ => held.push("spaces".to_owned()),
    }
    let shown = others.len().min(5);
    for &other in &others[..shown] {
        held.push(if is_control(other) {
            other.escape_unicode().to_string()
        } else {
            other.to_string()
        });
    }
    if others.len() > shown {
        held.push(format!("{} more", others.len() - shown));
    }
    if let [first @ .., last] = held.as_slice() {
        return Some(if first.is_empty() {
            format!("it holds {last}")
        } else {
            format!("it holds {} and {last}", first.join(", "))
        });
    }
    if text.starts_with(['-', '_']) {
        return Some("it does not start with a letter or a digit".to_owned());
    }
    is_reserved_on_windows(text)
        .then(|| "windows keeps it for a device, so it cannot be a folder there".to_owned())
}

/// `text` in lowercase with each letter's accents dropped, as `Ï` becomes `i`, and the letters
/// unicode does not split into a letter and an accent spelled out, as `ß` becomes `ss`. other
/// characters stay as they are.
fn plain_letters(text: &str) -> String {
    let lower = text.to_lowercase();
    if lower.is_ascii() {
        return lower;
    }
    let marks = CanonicalCombiningClassMapBorrowed::new();
    let mut plain = String::with_capacity(lower.len());
    for c in DecomposingNormalizerBorrowed::new_nfd()
        .normalize(&lower)
        .chars()
        .filter(|&c| marks.get_u8(c) == 0)
    {
        match c {
            'ß' => plain.push_str("ss"),
            'æ' => plain.push_str("ae"),
            'œ' => plain.push_str("oe"),
            'þ' => plain.push_str("th"),
            'ø' => plain.push('o'),
            'đ' | 'ð' => plain.push('d'),
            'ł' => plain.push('l'),
            'ı' => plain.push('i'),
            other => plain.push(other),
        }
    }
    plain
}

/// the candidate closest to `word`, when it is close enough to be a likely typo.
pub(crate) fn closest<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let limit = (word.chars().count() / 3).max(1);
    candidates
        .into_iter()
        .map(|candidate| (distance(word, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

/// the candidate closest to `word` when it is close enough to be a likely typo and no other is
/// as close. a candidate equal to `word` means `word` is no typo, so none is.
pub(crate) fn sole_closest<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let limit = (word.chars().count() / 3).max(1);
    let mut best: Option<(usize, &str)> = None;
    let mut tied = false;
    for candidate in candidates {
        let edits = distance(word, candidate);
        if edits == 0 {
            return None;
        }
        match best {
            _ if edits > limit => {}
            Some((nearest, _)) if edits > nearest => {}
            Some((nearest, _)) if edits == nearest => tied = true,
            _ => {
                best = Some((edits, candidate));
                tied = false;
            }
        }
    }
    best.filter(|_| !tied).map(|(_, candidate)| candidate)
}

/// the `most` candidates nearest to `word`, nearest first, however far that is.
pub(crate) fn nearest<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    most: usize,
) -> Vec<&'a str> {
    let mut ranked: Vec<(usize, &str)> = candidates
        .into_iter()
        .map(|candidate| (distance(word, candidate), candidate))
        .collect();
    ranked.sort_unstable();
    ranked
        .into_iter()
        .take(most)
        .map(|(_, candidate)| candidate)
        .collect()
}

/// the number of edits that turn `a` into `b`: adding, dropping or changing a character, or
/// swapping two neighbours, the most common typo of all.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // a swap of two neighbours reaches two rows back, so that row is kept as well
    let mut before = vec![0; b.len() + 1];
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut row = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        row[0] = i;
        for j in 1..=b.len() {
            let change = previous[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            row[j] = change.min(previous[j] + 1).min(row[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                row[j] = row[j].min(before[j - 2] + 1);
            }
        }
        std::mem::swap(&mut before, &mut previous);
        std::mem::swap(&mut previous, &mut row);
    }
    previous[b.len()]
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

/// whether a character is a control character, including unicode bidi controls.
pub(crate) fn is_control(character: char) -> bool {
    character.is_control() || is_bidi_control(character)
}

/// whether a character changes the reading direction of the surrounding text.
pub(crate) fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// why a path inside a dataset or an archive is refused, if it is: it could reach outside its
/// folder on some platform, it spells a folder a second way, as `a/./b` spells `a/b`, or it holds
/// a control character that terminals would act on. such a path is refused everywhere, so a
/// `data.lock` works on every machine.
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
    if path.contains(is_control) {
        return Some("it holds a control character");
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some("it names a windows drive");
    }
    for part in path.split('/') {
        if part.is_empty() {
            return Some("it has an empty folder name");
        }
        if part == ".." {
            return Some("it climbs out of its folder");
        }
        if part == "." {
            return Some("it has . as a folder name");
        }
    }
    None
}

/// why a data path names repository metadata, including spellings windows treats as aliases and
/// spellings hfs+ reads as the same name.
pub(crate) fn metadata(path: &str) -> Option<&'static str> {
    for part in path.split('/') {
        let visible = visible_to_hfs(part);
        let part = visible
            .split(':')
            .next()
            .unwrap_or_default()
            .trim_end_matches(['.', ' ']);
        if [
            ".git",
            ".hg",
            ".svn",
            ".fetchloom",
            "git~1",
            "hg~1",
            "svn~1",
            "fetchl~1",
        ]
        .iter()
        .any(|reserved| part.eq_ignore_ascii_case(reserved))
        {
            return Some("it names repository metadata");
        }
    }
    None
}

/// `part` without the code points hfs+ ignores in names, so `.g\u{200c}it` names `.git` there.
/// the set is the one git skips before its own `.git` check.
fn visible_to_hfs(part: &str) -> std::borrow::Cow<'_, str> {
    if part.is_ascii() {
        return std::borrow::Cow::Borrowed(part);
    }
    part.chars()
        .filter(|character| {
            !matches!(
                character,
                '\u{200c}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{206a}'..='\u{206f}'
                    | '\u{feff}'
            )
        })
        .collect()
}

/// why this machine's disks cannot store a path, if they cannot. checked before anything is
/// written, since other platforms may store the same path fine.
pub(crate) fn unstorable(path: &str) -> Option<String> {
    for part in path.split('/') {
        // ntfs allows 255 utf-16 units in a name, ext4 255 bytes
        let length = if cfg!(windows) {
            part.encode_utf16().count()
        } else {
            part.len()
        };
        if length > 255 {
            return Some(too_long(path, part));
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

/// why `path` cannot be stored when its name `part` is longer than disks allow. messages show
/// the path beside this reason, so the path's own name is not shown again, and a folder's is
/// shown short.
fn too_long(path: &str, part: &str) -> String {
    if path.rsplit('/').next() == Some(part) {
        "its name is longer than disks allow".to_owned()
    } else {
        format!(
            "its folder \"{}\" has a name longer than disks allow",
            abridged(part)
        )
    }
}

/// `text` as a message shows a name or a path: whole up to 100 characters, else its first 60
/// and its last 30 around `…`, so a name near the most a disk allows does not fill the screen.
pub(crate) fn abridged(text: &str) -> Cow<'_, str> {
    let count = text.chars().count();
    if count <= 100 {
        return Cow::Borrowed(text);
    }
    let head: String = text.chars().take(60).collect();
    let tail: String = text.chars().skip(count - 30).collect();
    Cow::Owned(format!("{head}…{tail}"))
}

/// why some disk fetchloom supports cannot store `path`, whatever this machine is: a name longer
/// than 255 bytes, which ext4 refuses, or one windows refuses, with a character it does not
/// allow, a dot or a space at its end, or a name it keeps for devices. a recipe's result is
/// checked by it, so files a step wrote on linux place on a teammate's windows too.
pub(crate) fn unportable(path: &str) -> Option<String> {
    for part in path.split('/') {
        if part.len() > 255 {
            return Some(too_long(path, part));
        }
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
    None
}

/// a key for portable collision checks and possible filesystem aliases. unicode names are
/// composed and lowercased first to join both sharp-s forms; the round trip through uppercase
/// joins letters like the two greek sigmas,
/// which lowercase alone keeps apart. equal keys still need a filesystem check before two
/// paths can be treated as the same file.
pub(crate) fn fold(path: &str) -> String {
    if path.is_ascii() {
        return path.to_ascii_lowercase();
    }
    ComposingNormalizerBorrowed::new_nfc()
        .normalize(path)
        .to_lowercase()
        .to_uppercase()
        .to_lowercase()
}

/// whether `a` and `b` differ at most in how their unicode is composed, as `café` written with
/// one character or with an `e` and a combining accent. apfs and hfs+ see both as one name, and
/// hfs+ lists every name decomposed whatever spelling made it.
pub(crate) fn same_composed(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.is_ascii() && b.is_ascii() {
        return false;
    }
    let nfc = ComposingNormalizerBorrowed::new_nfc();
    nfc.normalize(a) == nfc.normalize(b)
}

/// `name` as this machine's disks tell names apart: composed on macos, whose filesystems see
/// composed and decomposed unicode as one name, and unchanged elsewhere.
pub(crate) fn as_disk_sees(name: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(target_os = "macos") && !name.is_ascii() {
        ComposingNormalizerBorrowed::new_nfc().normalize(name)
    } else {
        std::borrow::Cow::Borrowed(name)
    }
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

pub(crate) fn is_reserved_on_windows(name: &str) -> bool {
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

/// whether `name`, a file's or folder's own name, is one an operating system writes into folders
/// by itself: finder's `.DS_Store`, the `._` files a mac leaves beside each file on a disk not
/// its own, the folders a mac, linux or windows keeps on a removable disk, and windows
/// explorer's thumbnails and folder settings. none of them is data.
pub(crate) fn made_by_system(name: &str) -> bool {
    (name.len() > 2 && name.starts_with("._"))
        || name.starts_with(".Trash-")
        || [
            ".DS_Store",
            "__MACOSX",
            ".Spotlight-V100",
            ".Trashes",
            ".fseventsd",
            ".TemporaryItems",
            "Thumbs.db",
            "ehthumbs.db",
            "desktop.ini",
            "$RECYCLE.BIN",
            "System Volume Information",
        ]
        .iter()
        .any(|made| name.eq_ignore_ascii_case(made))
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
    fn bidi_controls_are_refused_in_names() {
        for control in [
            '\u{202e}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}',
            '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{061c}',
        ] {
            let path = format!("report{control}txt.exe");
            assert!(escapes(&path).is_some(), "accepted bidi path: {path:?}");
            assert_eq!(
                sanitize(&path),
                None,
                "accepted bidi suggested name: {path:?}"
            );
        }
        assert_eq!(escapes("data/נתונים.csv"), None);
        assert_eq!(sanitize("נתונים.csv").as_deref(), Some("נתונים.csv"));
    }

    #[test]
    fn long_names_keep_their_extension() {
        let long = format!("{}.csv", "é".repeat(150));
        let short = sanitize(&long).unwrap();
        assert!(short.len() <= LONGEST);
        assert_eq!(Path::new(&short).extension(), Some("csv".as_ref()));
    }

    #[test]
    fn a_shortened_name_never_ends_with_a_dot_or_a_space() {
        for cut in [" ", ".", ". ", " ."] {
            let long = format!("{}{cut}{}", "a".repeat(LONGEST - cut.len()), "b".repeat(30));
            let short = sanitize(&long).unwrap();
            assert!(short.len() <= LONGEST, "{short}");
            assert!(!short.ends_with(['.', ' ']), "{short:?}");
        }
        assert_eq!(
            sanitize(&format!("{} {}", ".".repeat(199), "b".repeat(30))),
            None
        );
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
            "\u{1b}[2Jcleared.csv",
            "two\nlines.csv",
            "a/",
        ] {
            assert!(escapes(unsafe_path).is_some(), "{unsafe_path:?}");
        }
        assert_eq!(escapes("a/../../x"), Some("it climbs out of its folder"));
        assert_eq!(escapes("./x"), Some("it has . as a folder name"));
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

    /// what windows or ext4 cannot store is refused on every machine, so a result made on one
    /// system places on all of them.
    #[test]
    fn unportable_names_are_refused_everywhere() {
        assert_eq!(unportable("data/train.csv"), None);
        assert_eq!(unportable(&format!("{}/x", "名".repeat(85))), None);
        for path in [
            "2020-01-01T10:00:00.csv",
            "data/CON.txt",
            "notes. ",
            "notes.",
            "a\u{7}b",
            "what?.csv",
        ] {
            assert!(unportable(path).is_some(), "{path:?}");
        }
        // 255 bytes is the most ext4 takes, though windows counts 86 units of utf-16 here
        assert!(unportable(&"名".repeat(86)).is_some());
        assert_eq!(
            unportable("a/b:c.csv").as_deref(),
            Some("\"b:c.csv\" has ':', which windows does not allow in names")
        );
    }

    #[test]
    fn windows_counts_name_length_in_utf_16_units() {
        // 100 characters, 300 bytes of utf-8 and 100 units of utf-16
        let wide = "名".repeat(100);
        assert_eq!(
            unstorable(&format!("data/{wide}.csv")).is_some(),
            !cfg!(windows)
        );
        // characters beyond the basic plane take two units each
        assert!(unstorable(&"😀".repeat(128)).is_some());
        assert_eq!(unstorable(&"😀".repeat(127)).is_some(), !cfg!(windows));
    }

    #[test]
    fn metadata_names_hide_no_code_points_hfs_plus_ignores() {
        for ignored in [
            '\u{200c}', '\u{200d}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}',
            '\u{202d}', '\u{202e}', '\u{206a}', '\u{206b}', '\u{206c}', '\u{206d}', '\u{206e}',
            '\u{206f}', '\u{feff}',
        ] {
            for path in [
                format!("data/x/.g{ignored}it/config"),
                format!("data/{ignored}.git"),
                format!("data/.git{ignored}/hooks/pre-commit"),
                format!("data/.H{ignored}g/hgrc"),
                format!("data/.git.{ignored}/config"),
            ] {
                assert!(metadata(&path).is_some(), "accepted {path:?}");
            }
        }
        assert_eq!(metadata("data/.g\u{200b}it/config"), None);
        assert_eq!(metadata("data/.github/workflows.yml"), None);
    }

    #[test]
    fn dataset_names_come_from_free_text() {
        assert_eq!(dataset_name("DatasetIris").as_deref(), Some("datasetiris"));
        assert_eq!(
            dataset_name("ImageNet 2012 (val)").as_deref(),
            Some("imagenet-2012-val")
        );
        assert_eq!(
            dataset_name("snake_case_name").as_deref(),
            Some("snake_case_name")
        );
        assert_eq!(dataset_name("  ---  "), None);
        assert_eq!(dataset_name("_data").as_deref(), Some("data"));
        assert_eq!(dataset_name("__"), None);
        assert!(
            dataset_name(&"long ".repeat(20))
                .is_some_and(|name| name.len() <= 40 && !name.ends_with('-'))
        );
        assert_eq!(
            dataset_name("Effects of xylooligosaccharide supplementation on methane").as_deref(),
            Some("effects-of-xylooligosaccharide"),
            "long names end at a word break"
        );
        assert_eq!(
            dataset_name(&"x".repeat(50)).map(|name| name.len()),
            Some(40),
            "one long word is cut where it must be"
        );
        assert!(is_dataset_name("eeg-2024_v2"));
        for bad in ["", "EEG", "a b", "-lead", "x.y"] {
            assert!(!is_dataset_name(bad), "{bad}");
        }
    }

    #[test]
    fn accented_letters_keep_their_letter_in_names() {
        assert_eq!(dataset_name("ïris-数据").as_deref(), Some("iris"));
        assert_eq!(
            dataset_name("Straße Øresund").as_deref(),
            Some("strasse-oresund")
        );
        assert_eq!(
            title_name("Données météo de Paris").as_deref(),
            Some("donnees-meteo-de-paris")
        );
        assert_eq!(dataset_name("数据"), None);
    }

    #[test]
    fn a_refused_dataset_name_says_why() {
        assert_eq!(dataset_name_problem("eeg-2024_v2"), None);
        for (name, problem) in [
            ("", "it is empty"),
            ("-lead", "it does not start with a letter or a digit"),
            ("My Data Set", "it holds capital letters and spaces"),
            ("a.b/c\u{202e}", "it holds ., / and \\u{202e}"),
            ("äöüõåé", "it holds ä, ö, ü, õ, å and 1 more"),
        ] {
            assert_eq!(
                dataset_name_problem(name).as_deref(),
                Some(problem),
                "{name}"
            );
        }
        assert_eq!(
            dataset_name_problem(&"a".repeat(65)).as_deref(),
            Some("it has 65 characters, and the most is 64")
        );
        assert_eq!(dataset_name_problem(&"a".repeat(64)), None);
    }

    #[test]
    fn windows_device_names_are_refused_and_never_generated() {
        for device in ["con", "aux", "nul", "prn", "com1", "lpt1", "lpt9"] {
            assert_eq!(
                dataset_name_problem(device).as_deref(),
                Some("windows keeps it for a device, so it cannot be a folder there"),
                "{device}"
            );
            let generated = format!("{device}-data");
            assert_eq!(dataset_name(device), Some(generated.clone()));
            assert_eq!(title_name(&device.to_uppercase()), Some(generated.clone()));
            assert_eq!(
                reference_name(&format!("hf:datasets/org/{device}")),
                Some(generated)
            );
        }
        for usable in ["console", "com10", "con-data", "auxiliary", "nul_set"] {
            assert_eq!(dataset_name_problem(usable), None, "{usable}");
        }
    }

    #[test]
    fn titles_give_short_names() {
        for (title, name) in [
            (
                "Pitch, yaw and roll (torsion) of left and right hands",
                "pitch-yaw-and-roll",
            ),
            (
                "Effects of xylooligosaccharide supplementation on methane",
                "effects-of-xylooligosaccharide",
            ),
            ("Data for the", "data"),
            ("Iris Species", "iris-species"),
            ("DatasetIris", "datasetiris"),
            ("The", "the"),
            ("_private notes", "private-notes"),
        ] {
            assert_eq!(title_name(title).as_deref(), Some(name), "{title}");
        }
        assert_eq!(title_name(&"x".repeat(50)).map(|name| name.len()), Some(40));
        assert_eq!(title_name(" -- "), None);
    }

    #[test]
    fn references_give_their_last_named_part() {
        for (reference, name) in [
            ("hf:datasets/scikit-learn/iris", "iris"),
            ("hf:datasets/org/name@main", "name"),
            ("kaggle:uciml/iris", "iris"),
            ("zenodo:3608990", "zenodo-3608990"),
            ("figshare:5616403@1", "figshare-5616403"),
        ] {
            assert_eq!(
                reference_name(reference).as_deref(),
                Some(name),
                "{reference}"
            );
        }
    }

    #[test]
    fn typos_find_their_word() {
        assert_eq!(
            closest("selct", ["ref", "select", "exclude"]),
            Some("select")
        );
        assert_eq!(
            closest("exlcude", ["ref", "select", "exclude"]),
            Some("exclude")
        );
        assert_eq!(closest("banana", ["ref", "select", "exclude"]), None);
        assert_eq!(
            closest("irsi", ["cows", "iris"]),
            Some("iris"),
            "two neighbours swapped are one edit"
        );
        assert_eq!(distance("ab", "ba"), 1);
        assert_eq!(closest("sync", ["iris", "cows"]), None);
        assert_eq!(
            closest("irxy", ["iris"]),
            None,
            "two changed letters of four are not close"
        );
        assert_eq!(
            nearest("iris.csv", ["b.txt", "iris.tsv", "a.csv"], 2),
            ["iris.tsv", "a.csv"]
        );
    }

    #[test]
    fn compositions_of_one_name_are_the_same_name() {
        assert!(same_composed("caf\u{e9}", "cafe\u{301}"));
        assert!(
            same_composed("\u{212a}", "K"),
            "the kelvin sign composes to K"
        );
        assert!(same_composed("iris", "iris"));
        assert!(!same_composed("Caf\u{e9}", "cafe\u{301}"));
        assert!(!same_composed("a", "b"));
        let seen = as_disk_sees("cafe\u{301}");
        if cfg!(target_os = "macos") {
            assert_eq!(seen, "caf\u{e9}");
        } else {
            assert_eq!(seen, "cafe\u{301}");
        }
        assert_eq!(as_disk_sees("iris"), "iris");
    }

    #[test]
    fn capital_sharp_s_has_the_same_portable_key() {
        for (a, b) in [
            ("ß", "ẞ"),
            ("Straße", "STRAẞE"),
            ("σ", "ς"),
            ("café", "cafe\u{301}"),
        ] {
            assert_eq!(fold(a), fold(b), "{a} and {b}");
        }
    }

    #[test]
    fn names_the_system_writes_by_itself_are_known_and_data_is_not() {
        for name in [
            ".DS_Store",
            ".ds_store",
            "._iris.csv",
            "__MACOSX",
            ".Spotlight-V100",
            ".Trashes",
            ".fseventsd",
            ".TemporaryItems",
            ".Trash-1000",
            "Thumbs.db",
            "thumbs.db",
            "ehthumbs.db",
            "desktop.ini",
            "$RECYCLE.BIN",
            "System Volume Information",
        ] {
            assert!(made_by_system(name), "{name}");
        }
        for name in [
            "._",
            "_iris.csv",
            "DS_Store",
            "iris.csv",
            ".git",
            "thumbs.csv",
            "Trashes",
        ] {
            assert!(!made_by_system(name), "{name}");
        }
    }
}

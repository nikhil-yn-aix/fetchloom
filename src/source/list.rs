//! lists of files, each one dataset: a `SHA256SUMS` or `MD5SUMS` file, a pooch registry or a list
//! of links names files at their own links or below the list's folder, with the MD5 or SHA-256 a
//! line states, which the first download checks. one reader takes every line form, told apart by
//! which field is a hash. a list is text anyone could have written, so it is read whole and
//! strictly before any file is asked for, and the links it gives are trusted no more than others.

use std::fmt;
use std::io::Read as _;
use std::path::Path;

use url::Url;

use super::{Access, Listing, Reference, Remote};
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::net::Api;

/// the most bytes of a list read, as much as the network reads of any answer read whole.
pub(crate) const MOST_BYTES: u64 = 64 << 20;

/// what a line holds, for messages about one that cannot be read.
pub(crate) const FORM: &str = "a line is a file's link, then its MD5 or SHA-256 if known, as md5:<32 hex digits> or sha256:<64 hex digits>. a list at a link may name paths below its folder, as `<hash>  <path>`, `SHA256 (<path>) = <hash>` or `<path> <hash> [<link>]`. blank lines and lines starting with # are skipped";

/// how data.toml marks a list: this, then its link or its path in the project.
const MARK: &str = "list:";

/// the longest path and link, in bytes, a list may give a file: the most data.lock keeps.
const LONGEST_PATH: usize = 4096;
const LONGEST_LINK: usize = 64 << 10;

/// a list as data.toml names it after `list:`: at a link, or a file in the project folder by its
/// path there, with `/` between folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum List {
    Link(Url),
    Project(String),
}

/// the hash a line states for its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Checksum {
    Md5(Md5),
    Sha256(Sha256),
}

/// what a line names: a link, a DOI or an id as written, or a path below the list's folder,
/// which a pooch registry may follow with the file's own link.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Named {
    Link(String),
    Path { path: String, link: Option<String> },
}

/// one line of a list that names something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// where the line is in its list, counting from 1.
    pub(crate) number: usize,
    pub(crate) named: Named,
    pub(crate) checksum: Option<Checksum>,
}

impl Entry {
    /// how messages name the line, as `line 2 of links.txt`.
    pub(crate) fn at(&self, list: &str) -> String {
        format!("line {} of {list}", self.number)
    }
}

/// a hash function a list may name.
#[derive(Clone, Copy)]
enum Function {
    Md5,
    Sha256,
    /// one fetchloom does not check, by the name messages give it.
    Other(&'static str),
}

impl List {
    /// reads what follows `list:`: a link, with the key of any secret it carries, or a path in
    /// the project.
    pub(super) fn parse(text: &str) -> Result<(List, Option<String>), Error> {
        let text = text.trim();
        let lower = text.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            let url = super::plain_url(text)?;
            let secret = crate::shown::secret(&url);
            return Ok((List::Link(url), secret));
        }
        let mut path = text;
        while let Some(rest) = path.strip_prefix("./") {
            path = rest;
        }
        let refused = |reason: &str| {
            Error::new(
                Kind::ReferenceInvalid,
                format!(
                    "{MARK}{} names no list fetchloom reads, because {reason}",
                    crate::name::escaped(text)
                ),
            )
            .help("write list: and the list's link, or its path inside the project with / between folders, such as list:lists/files.txt")
        };
        if path.starts_with(['~', '$']) {
            return Err(refused(
                "fetchloom does not expand ~ or $NAME, and a list is a file inside the project",
            ));
        }
        match crate::name::escapes(path).or_else(|| crate::name::metadata(path)) {
            Some(reason) => Err(refused(reason)),
            None => Ok((List::Project(path.to_owned()), None)),
        }
    }

    /// the list a plain link names by its file's name, as `SHA256SUMS` or `md5sums.txt`, which
    /// list the checksums of the files beside them.
    pub(super) fn from_link(url: &Url) -> Option<List> {
        let name = url.path_segments()?.next_back()?;
        is_named_for_sums(name).then(|| List::Link(url.clone()))
    }

    /// the path of a list in the project, which only that project's commands read.
    pub(crate) fn in_project(&self) -> Option<&str> {
        match self {
            List::Project(path) => Some(path),
            List::Link(_) => None,
        }
    }

    /// the origin a person names by writing a list's link.
    pub(super) fn written_origin(&self) -> Option<url::Origin> {
        match self {
            List::Link(url) => Some(url.origin()),
            List::Project(_) => None,
        }
    }

    /// the list as messages name it: its link, any secret in it masked, or its path.
    pub(crate) fn shown(&self) -> String {
        match self {
            List::Link(url) => crate::shown::Shown(url).to_string(),
            List::Project(path) => path.clone(),
        }
    }

    /// the text a dataset's name is made from: the list's folder for a list named after a hash,
    /// as `mitdb/1.0.0` for physionet's `SHA256SUMS.txt`, else the list's own name.
    pub(crate) fn naming(&self) -> String {
        let path = match self {
            List::Link(url) => url.path().trim_matches('/'),
            List::Project(path) => path,
        };
        match path.rsplit_once('/') {
            Some((folder, name)) if is_named_for_sums(name) => folder.to_owned(),
            Some((_, name)) => crate::name::folder_for(name),
            None => crate::name::folder_for(path),
        }
    }

    /// asks for the list at its link and lists the files it names below its folder.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let url = match self {
            List::Link(url) => url,
            List::Project(path) => {
                return Err(Error::new(
                    Kind::ReferenceUnresolved,
                    format!("{self} is a list in a project folder, which only that project's commands read"),
                )
                .help(format!(
                    "to fetch the files it lists once, run `{}` in that folder",
                    crate::suggested::command(&format!(
                        "get -i {}",
                        crate::suggested::shell_word(path)
                    ))
                )));
            }
        };
        let shown = self.shown();
        let response = api.get(url).await?;
        let host = crate::net::host(&response.url);
        match response.status {
            200..=299 => {}
            404 | 410 => {
                return Err(Error::new(
                    Kind::ReferenceUnresolved,
                    format!("{host} has nothing at {shown} ({})", response.status),
                )
                .help(crate::net::MOVED_HELP));
            }
            401 | 403 => return Err(super::refused(host, &shown)),
            status => {
                return Err(Error::new(
                    Kind::NetworkStatus,
                    format!("{host} answered {status} for {shown}"),
                ));
            }
        }
        let text = String::from_utf8(response.body.to_vec())
            .map_err(|_| super::invalid(&shown, "the list is not UTF-8 text"))?;
        if text
            .trim_start_matches('\u{feff}')
            .trim_start()
            .starts_with('<')
        {
            return Err(Error::new(
                Kind::SourceInvalid,
                format!("{shown} answers with a web page, not a list of files"),
            )
            .help("open the link in a browser: a site that asks you to sign in shows a page instead. use the link that downloads the list itself"));
        }
        let summed_at = text
            .lines()
            .position(|line| is_summed(line.trim()))
            .map(|index| format!("line {} of {shown}", index + 1));
        let read = match summed_at {
            Some(at) => Err(summed(&at, Some(&below(url, "")))),
            None => read(&shown, &text, api.most_files()),
        }
        .and_then(|entries| listing(self, entries, Some(url)));
        read.map_err(|mut error| {
            // the list is the server's, so nothing on this machine is to blame
            if error.kind == Kind::Usage {
                error.kind = Kind::SourceInvalid;
            }
            error
        })
    }

    /// the files a list in the project at `root` names, read from its file there, at most `most`.
    pub(crate) fn read_in(&self, root: &Path, most: usize) -> Result<Listing, Error> {
        let Some(path) = self.in_project() else {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!("{self} is no list in the project folder"),
            ));
        };
        let text = read_file(root, path, MOST_BYTES)?;
        let entries = read(path, &text, most)?;
        let listing = listing(self, entries, None)?;
        super::check(&Reference::List(self.clone()), listing, most)
    }
}

impl fmt::Display for List {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            List::Link(url) => write!(f, "{MARK}{url}"),
            List::Project(path) => write!(f, "{MARK}{path}"),
        }
    }
}

/// whether a list's file `name` says it holds checksums, as `SHA256SUMS`, `MD5SUMS`,
/// `sha1sum.txt`, NCBI's `md5checksums.txt` and ensembl's `CHECKSUMS` do, in any case.
fn is_named_for_sums(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let stem = name.strip_suffix(".txt").unwrap_or(&name);
    ["checksums", "checksum", "sums", "sum"]
        .iter()
        .find_map(|kind| stem.strip_suffix(kind).map(|function| (*kind, function)))
        .is_some_and(|(kind, function)| {
            (function.is_empty() && kind.starts_with("checksum"))
                || ["md5", "sha1", "sha224", "sha256", "sha384", "sha512", "b2"].contains(&function)
        })
}

/// whether `line` is what unix `sum` or `cksum` writes: a checksum of at most 10 digits, a size,
/// then the file's name.
fn is_summed(line: &str) -> bool {
    let digits = |word: &str, most: usize| {
        (1..=most).contains(&word.len()) && word.bytes().all(|byte| byte.is_ascii_digit())
    };
    let mut words = line.split_whitespace();
    let (Some(sum), Some(size), Some(_)) = (words.next(), words.next(), words.next()) else {
        return false;
    };
    digits(sum, 10) && digits(size, 20)
}

/// the error for `at`, a line unix `sum` or `cksum` wrote, whose sum shows nothing of a file's
/// bytes. `folder`, the link of a list's own folder, is the link that pins its files.
fn summed(at: &str, folder: Option<&Url>) -> Error {
    let error = Error::new(
        Kind::Usage,
        format!(
            "{at} gives a unix `sum` checksum, a kind fetchloom cannot check: it checks MD5 and SHA-256 alone"
        ),
    );
    match folder {
        Some(folder) => error.help(format!(
            "add the folder's link instead, as in `{}`, or each file's link",
            crate::suggested::command(&format!(
                "add {}",
                crate::suggested::shell_word(folder.as_str())
            ))
        )),
        None => error.help("list each file's link instead, with its MD5 or SHA-256 if you know it"),
    }
}

/// the text of the list at `path` in the project at `root`, at most `most` bytes, refused when a
/// folder on the way or the list itself is a link, since a cloned project's link could take the
/// reading anywhere.
fn read_file(root: &Path, path: &str, most: u64) -> Result<String, Error> {
    let shown = crate::name::escaped(path);
    let mut at = root.to_path_buf();
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        at.push(part);
        if parts.peek().is_none() {
            break;
        }
        match std::fs::symlink_metadata(&at) {
            Ok(found) if found.is_dir() => {}
            Ok(_) => {
                return Err(Error::new(
                    Kind::ReferenceInvalid,
                    format!("{shown} is not in a real folder of the project: {} is a link or a file", at.display()),
                )
                .help("keep the list in a folder of the project itself, and name it there in data.toml"));
            }
            Err(err) => return Err(not_there(&shown, &err)),
        }
    }
    let file = crate::volume::read_unfollowed(&at).map_err(|err| not_there(&shown, &err))?;
    let mut bytes = Vec::new();
    file.take(most + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::io(format_args!("could not read {shown}"), &err))?;
    if bytes.len() as u64 > most {
        return Err(too_big(&shown, most));
    }
    String::from_utf8(bytes).map_err(|_| {
        Error::new(Kind::Usage, format!("{shown} is not UTF-8 text"))
            .help("save the list as UTF-8, then run the command again")
    })
}

fn not_there(shown: &str, err: &std::io::Error) -> Error {
    if err.kind() == std::io::ErrorKind::NotFound {
        return Error::new(
            Kind::ReferenceUnresolved,
            format!("the list {shown} is not in the project folder"),
        )
        .help("get the commit that adds it, or restore it from git, then run the command again");
    }
    Error::new(
        Kind::LocalIo,
        format!("could not read the list {shown}: {err}"),
    )
}

/// the error for the list `shown`, which holds more than the `most` bytes a list may.
pub(crate) fn too_big(shown: &str, most: u64) -> Error {
    Error::new(
        Kind::Usage,
        format!(
            "{shown} is larger than {}, the most fetchloom reads of a list",
            crate::units::size(most)
        ),
    )
    .help("split it into lists of fewer files, each a dataset of its own")
}

/// the entries of the list `name`, whose text is `text`, at most `most` of them. a line in no
/// known form, a hash fetchloom does not check, or a path that could leave its folder is refused,
/// naming the line. a line naming a file at a hash an earlier line gave counts once.
pub(crate) fn read(name: &str, text: &str, most: usize) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::new();
    // openbsd's own SHA256 repeats lines byte for byte, as `sha256sum -c` accepts
    let mut seen = std::collections::HashSet::new();
    for (index, written) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let number = index + 1;
        let Some((named, checksum)) = read_line(name, number, written)? else {
            continue;
        };
        if !seen.insert((named.clone(), checksum)) {
            continue;
        }
        if entries.len() == most {
            return Err(super::too_many(name, most, "list fewer files"));
        }
        entries.push(Entry {
            number,
            named,
            checksum,
        });
    }
    Ok(entries)
}

/// what the line `written`, number `number` of the list `name`, names, or none for a blank line
/// or a comment.
fn read_line(
    name: &str,
    number: usize,
    written: &str,
) -> Result<Option<(Named, Option<Checksum>)>, Error> {
    let line = written.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let at = format!("line {number} of {name}");
    if is_summed(line) {
        return Err(summed(&at, None));
    }
    if line.starts_with('\\') {
        return Err(Error::new(
            Kind::Usage,
            format!("{at} writes its file's name with \\ escapes, for a \\ or a line break, which no path fetchloom places may hold"),
        )
        .help("ask the list's owner to rename the file"));
    }
    if let Some((named, checksum)) = tagged(line, &at)? {
        return Ok(Some((named, Some(checksum))));
    }
    let (first, rest) = line.split_once([' ', '\t']).unwrap_or((line, ""));
    if let Some(checksum) = digest(first).map_err(|refusal| refusal.error(first, &at))? {
        // GNU's form: one space, then a space or a `*` that says how the file was read, then the
        // name, spaces and all
        let file = rest.strip_prefix([' ', '*']).unwrap_or(rest).trim_start();
        if file.is_empty() {
            return Err(
                Error::new(Kind::Usage, format!("{at} gives a hash and no file")).help(FORM),
            );
        }
        return Ok(Some((named_file(file, &at)?, Some(checksum))));
    }
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [first, rest @ ..] = fields.as_slice() else {
        return Ok(None);
    };
    let Some((second, rest)) = rest.split_first() else {
        return named(first, &at).map(|named| Some((named, None)));
    };
    let checksum = match digest(second) {
        Ok(Some(checksum)) => checksum,
        Ok(None) if is_link(first) || is_reference(first) => {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "\"{}\" on {at} is not an MD5 or a SHA-256",
                    crate::shown::mask_query(second).escape_debug()
                ),
            )
            .detail("an MD5 is 32 hex digits and a SHA-256 64, bare or after md5: or sha256:")
            .help(FORM));
        }
        Ok(None) => return Err(not_a_link(name, number, line)),
        Err(refusal) => return Err(refusal.error(second, &at)),
    };
    let named = match (named(first, &at)?, rest) {
        (named, []) => named,
        (Named::Path { path, .. }, [own]) if is_link(own) => Named::Path {
            path,
            link: Some((*own).to_owned()),
        },
        (Named::Path { .. }, [other]) => {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "{at} gives \"{}\" where a pooch registry gives the file's link",
                    crate::shown::mask_query(other).escape_debug()
                ),
            )
            .help(FORM));
        }
        (Named::Link(_), _) => {
            return Err(Error::new(
                Kind::Usage,
                format!("{at} holds more than a link and its hash"),
            )
            .help(FORM));
        }
        (Named::Path { .. }, _) => {
            return Err(Error::new(
                Kind::Usage,
                format!("{at} holds more than a file, its hash and its link"),
            )
            .help(FORM));
        }
    };
    Ok(Some((named, Some(checksum))))
}

/// a BSD line, as `md5` writes and `sha256sum --tag` too, `SHA256 (name) = hash`; none when the
/// line has another form.
fn tagged(line: &str, at: &str) -> Result<Option<(Named, Checksum)>, Error> {
    let Some((function, rest)) = line.split_once('(') else {
        return Ok(None);
    };
    let Some(function) = function_named(function.trim_end()) else {
        return Ok(None);
    };
    let Some((file, hash)) = rest.rsplit_once(')') else {
        return Ok(None);
    };
    let Some(hash) = hash.trim_start().strip_prefix('=') else {
        return Ok(None);
    };
    let hash = hash.trim();
    let checksum = match function {
        Function::Md5 => Md5::from_hex(hash).map(Checksum::Md5),
        Function::Sha256 => Sha256::from_hex(hash).map(Checksum::Sha256),
        Function::Other(function) => return Err(Refusal::Unchecked(function).error(hash, at)),
    };
    let Some(checksum) = checksum else {
        return Err(Refusal::Digits(function).error(hash, at));
    };
    Ok(Some((named_file(file, at)?, checksum)))
}

/// why a token that names a hash is refused.
enum Refusal {
    /// it is of a function fetchloom does not check, by the name messages give it.
    Unchecked(&'static str),
    /// it is not as many hex digits as its function makes.
    Digits(Function),
}

impl Refusal {
    /// the error for `token`, refused on `at`.
    fn error(&self, token: &str, at: &str) -> Error {
        match self {
            Refusal::Unchecked(function) => {
                let article = if function.starts_with('x') { "an" } else { "a" };
                Error::new(
                    Kind::Usage,
                    format!("{at} gives {article} {function} hash, and fetchloom checks only MD5 and SHA-256"),
                )
                .help("list each file's SHA-256 or MD5 instead, as its site states them, or leave its hash off")
            }
            Refusal::Digits(function) => {
                let (name, digits) = match function {
                    Function::Md5 => ("an MD5", 32),
                    _ => ("a SHA-256", 64),
                };
                Error::new(
                    Kind::Usage,
                    format!(
                        "\"{}\" on {at} is not {name}",
                        crate::shown::mask_query(token).escape_debug()
                    ),
                )
                .detail(format!("{name} is {digits} hex digits"))
                .help(FORM)
            }
        }
    }
}

/// the hash `token` states, bare, when its length tells MD5 from SHA-256, or after a function's
/// name and `:`, as pooch writes them; none for a token that is no hash. a hash of a function
/// fetchloom does not check is refused by its name.
fn digest(token: &str) -> Result<Option<Checksum>, Refusal> {
    let (function, hex) = match token.split_once(':') {
        Some((name, hex)) => match function_named(name) {
            Some(function) => (Some(function), hex),
            None => return Ok(None),
        },
        None => (None, token),
    };
    let is_hex = !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    let function = match (function, is_hex) {
        (Some(function), _) => function,
        (None, true) => match hex.len() {
            32 => Function::Md5,
            64 => Function::Sha256,
            40 => Function::Other("SHA-1"),
            56 => Function::Other("SHA-224"),
            96 => Function::Other("SHA-384"),
            128 => Function::Other("SHA-512"),
            _ => return Ok(None),
        },
        (None, false) => return Ok(None),
    };
    let checksum = match function {
        Function::Md5 => Md5::from_hex(hex).map(Checksum::Md5),
        Function::Sha256 => Sha256::from_hex(hex).map(Checksum::Sha256),
        Function::Other(name) => return Err(Refusal::Unchecked(name)),
    };
    checksum.map(Some).ok_or(Refusal::Digits(function))
}

/// the hash function a list names `name`, in any case, as BSD lines and pooch write them.
fn function_named(name: &str) -> Option<Function> {
    let name = name.to_ascii_lowercase();
    Some(match name.as_str() {
        "md5" => Function::Md5,
        "sha256" | "sha2-256" | "sha-256" => Function::Sha256,
        "sha1" | "sha-1" => Function::Other("SHA-1"),
        "sha224" | "sha2-224" | "sha-224" => Function::Other("SHA-224"),
        "sha384" | "sha2-384" | "sha-384" => Function::Other("SHA-384"),
        "sha512" | "sha2-512" | "sha-512" => Function::Other("SHA-512"),
        "sha3-224" | "sha3-256" | "sha3-384" | "sha3-512" => Function::Other("SHA-3"),
        "blake2b" | "blake2b-256" | "blake2b-512" | "blake2s" | "b2" => Function::Other("BLAKE2"),
        "blake3" => Function::Other("BLAKE3"),
        "xxh64" | "xxh128" | "xxh3_64" | "xxh3_128" => Function::Other("xxHash"),
        _ => return None,
    })
}

/// whether `token` is a link fetchloom downloads as it is.
fn is_link(token: &str) -> bool {
    let lower = token.get(..8).unwrap_or(token).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// whether `token` names data by a prefix and `:`, as an id or a `file:` link does, or is a DOI.
fn is_reference(token: &str) -> bool {
    let scheme = token.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    scheme || super::doi::Doi::bare(token).is_some()
}

/// what a line names by `token`, the first of its words: a link, a DOI or an id, or a path below
/// the list's folder.
fn named(token: &str, at: &str) -> Result<Named, Error> {
    if is_link(token) || is_reference(token) {
        return Ok(Named::Link(token.to_owned()));
    }
    path(token, at).map(|path| Named::Path { path, link: None })
}

/// what the file name of a checksum line names: a link, or a path below the list's folder.
fn named_file(file: &str, at: &str) -> Result<Named, Error> {
    if is_link(file) {
        return Ok(Named::Link(file.to_owned()));
    }
    path(file, at).map(|path| Named::Path { path, link: None })
}

/// `written`, a path a line gives below its list's folder, without a leading `./`; refused when it
/// could leave that folder or names repository metadata.
fn path(written: &str, at: &str) -> Result<String, Error> {
    let mut path = written;
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    match crate::name::escapes(path).or_else(|| crate::name::metadata(path)) {
        None => Ok(path.to_owned()),
        Some(reason) => Err(Error::new(
            Kind::Usage,
            format!(
                "{at} names \"{}\", which fetchloom refuses because {reason}",
                crate::name::escaped(written).escape_debug()
            ),
        )
        .help("a list names each file by its link, or by its path below the list's own folder")),
    }
}

/// the error for line `number` of the list `name`, `line`, which reads as search words rather than
/// a file: the search its help names keeps each word one literal word for a shell.
pub(crate) fn not_a_link(name: &str, number: usize, line: &str) -> Error {
    let line = crate::shown::mask_query(line);
    let words: Vec<String> = line
        .split_whitespace()
        .map(|word| crate::suggested::shell_word(word).into_owned())
        .collect();
    Error::new(
        Kind::Usage,
        format!("line {number} of {name} is not a link, a DOI or an id"),
    )
    .detail(line.clone().into_owned())
    .help(format!(
        "find its id with `{}`, then list that instead",
        crate::suggested::anywhere(&format!("search {}", words.join(" ")))
    ))
}

/// the error for `entry` of the list `name`, a path, when the list has no folder to read it
/// below, as a list on this machine has not.
pub(crate) fn no_folder(name: &str, entry: &Entry, path: &str) -> Error {
    if crate::source::search::is_search(path) {
        return not_a_link(name, entry.number, path);
    }
    Error::new(
        Kind::Usage,
        format!(
            "{} names \"{}\", a path, and only a list at a link has a folder to read it below",
            entry.at(name),
            crate::name::escaped(path).escape_debug()
        ),
    )
    .help(format!(
        "put the file's link on that line, or use the list by its own link, as `{}` does",
        crate::suggested::command("add https://example.org/data/SHA256SUMS")
    ))
}

/// the link `text` a line gives a file, refused when it is no plain link, holds a control
/// character a link parser would hide, or carries a secret data.lock would keep.
fn file_link(text: &str, at: &str) -> Result<Url, Error> {
    if text.contains(crate::name::is_control) {
        return Err(Error::new(
            Kind::Usage,
            format!("the link on {at} holds a control character"),
        )
        .help("write each link on its line as it is, without control characters"));
    }
    if !is_link(text) {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "{at} names {}, and each line of a list is one file's link",
                crate::shown::mask_query(text)
            ),
        )
        .help(format!(
            "add a dataset by its id on its own, as in `{}`, or list the links of its files",
            crate::suggested::command("add zenodo:3608990")
        )));
    }
    let url = super::plain_url(text).map_err(|mut error| {
        error.message = format!("{at}: {}", error.message);
        error
    })?;
    if let Some(key) = crate::shown::secret(&url) {
        return Err(Error::new(
            Kind::ReferenceInvalid,
            format!("{at}: this link carries {key}, a secret that data.lock would keep and share"),
        )
        .help("use a link that works without it, or download the file once with `fetchloom get`"));
    }
    Ok(url)
}

/// the listing of `list`, whose lines are `entries`: each file at its own link, or at its path
/// below `folder`, the list's own link, when it has one.
fn listing(list: &List, entries: Vec<Entry>, folder: Option<&Url>) -> Result<Listing, Error> {
    let shown = list.shown();
    if entries.is_empty() {
        return Err(Error::new(Kind::Usage, format!("{shown} lists no files"))
            .help("put one file a line in it, as its link and then its MD5 or SHA-256 if known. blank lines and lines starting with # are skipped"));
    }
    let mut files = Vec::with_capacity(entries.len());
    for entry in entries {
        let at = entry.at(&shown);
        let (path, url) = match &entry.named {
            Named::Link(text) => {
                let url = file_link(text, &at)?;
                let Some(path) = crate::name::from_url(&url) else {
                    return Err(
                        Error::new(Kind::Usage, format!("the link on {at} names no file"))
                            .help("give the file's own link on that line"),
                    );
                };
                (path, url)
            }
            Named::Path {
                path,
                link: Some(link),
            } => (path.clone(), file_link(link, &at)?),
            Named::Path { path, link: None } => match folder {
                Some(folder) => (path.clone(), below(folder, path)),
                None => return Err(no_folder(&shown, &entry, path)),
            },
        };
        if path.len() > LONGEST_PATH || url.as_str().len() > LONGEST_LINK {
            let what = if path.len() > LONGEST_PATH {
                "a path"
            } else {
                "a link"
            };
            return Err(Error::new(
                Kind::Usage,
                format!("{at} gives {what} longer than data.lock keeps"),
            )
            .help("data.lock keeps a path of up to 4,096 bytes and a link of up to 64 KiB"));
        }
        let (sha256, md5) = match entry.checksum {
            Some(Checksum::Sha256(sha256)) => (Some(sha256), None),
            Some(Checksum::Md5(md5)) => (None, Some(md5)),
            None => (None, None),
        };
        files.push(Remote {
            path,
            size: None,
            sha256,
            md5,
            url,
            named_by_server: false,
            etag: None,
        });
    }
    Ok(Listing {
        resolved: Reference::List(list.clone()),
        title: None,
        license: None,
        doi: None,
        access: Access::Unknown,
        files,
        unavailable: Vec::new(),
        warnings: Vec::new(),
        within: Vec::new(),
    })
}

/// the link of `path` below the folder of the list at `list`: each folder of the path a part of
/// the link, written out, so no path can leave the list's origin or folder.
fn below(list: &Url, path: &str) -> Url {
    let mut url = list.clone();
    url.set_query(None);
    url.set_fragment(None);
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop();
        segments.extend(path.split('/'));
    }
    url
}

#[cfg(test)]
mod tests;

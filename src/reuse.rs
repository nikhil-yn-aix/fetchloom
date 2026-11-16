//! copies of pinned files that other tools keep on this machine: hugging face's and kagglehub's
//! caches, and the folders the `reuse` setting names. fetchloom only reads them, never through a
//! link, and takes a file into its cache as a clone or a copy once its bytes hash to the pin, so
//! nothing there is ever written, moved, deleted or linked to.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;
use url::Url;

use crate::digest::Sha256;
use crate::machine::Var;
use crate::stop::Stop;

/// how deep a scan goes below a folder, past any layout a tool or a person makes, so a bind mount
/// that shows a folder inside itself ends.
const DEEPEST: usize = 64;

/// a tool whose cache fetchloom knows the layout of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tool {
    HuggingFace,
    Kagglehub,
}

impl Tool {
    /// how a run names the tool's cache.
    pub(crate) fn cache(self) -> &'static str {
        match self {
            Tool::HuggingFace => "the hugging face cache",
            Tool::Kagglehub => "kagglehub's cache",
        }
    }
}

/// a folder copies may come from, with the tool that keeps it when it is a cache fetchloom knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Folder {
    pub(crate) path: PathBuf,
    pub(crate) tool: Option<Tool>,
}

/// where hugging face's and kagglehub's caches are on this machine, as their own tools find them,
/// whether or not they are there.
pub(crate) fn located(var: Var<'_>) -> Vec<(Tool, PathBuf)> {
    let named = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(|value| expanded(PathBuf::from(value), var))
    };
    let home = crate::machine::home(var);
    let cached = |below: &str| home.as_ref().map(|home| home.join(".cache").join(below));
    let hugging_face = named("HF_HUB_CACHE")
        .or_else(|| named("HUGGINGFACE_HUB_CACHE"))
        .or_else(|| named("HF_HOME").map(|home| home.join("hub")))
        .or_else(|| named("XDG_CACHE_HOME").map(|cache| cache.join("huggingface").join("hub")))
        .or_else(|| cached("huggingface").map(|folder| folder.join("hub")));
    let kagglehub = named("KAGGLEHUB_CACHE").or_else(|| cached("kagglehub"));
    [
        (Tool::HuggingFace, hugging_face),
        (Tool::Kagglehub, kagglehub),
    ]
    .into_iter()
    .filter_map(|(tool, path)| Some((tool, path?)))
    .collect()
}

/// `path` with a leading `~` as the home folder, as python's tools take one in these variables.
fn expanded(path: PathBuf, var: Var<'_>) -> PathBuf {
    match (path.strip_prefix("~"), crate::machine::home(var)) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path,
    }
}

/// the caches of [`located`] that are folders on this machine, which `reuse` names by default.
pub(crate) fn defaults(var: Var<'_>) -> Vec<Folder> {
    located(var)
        .into_iter()
        .filter(|(_, path)| std::fs::metadata(path).is_ok_and(|found| found.is_dir()))
        .map(|(tool, path)| Folder {
            path,
            tool: Some(tool),
        })
        .collect()
}

/// the folder at `path`, as a person names it, known as a tool's cache when it is where that
/// tool keeps one.
pub(crate) fn named(path: PathBuf, located: &[(Tool, PathBuf)]) -> Folder {
    let tool = located
        .iter()
        .find(|(_, at)| *at == path)
        .map(|(tool, _)| *tool);
    Folder { path, tool }
}

/// what a pinned file's link says of where another tool keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Known {
    /// a file of a hugging face repository at one commit.
    HuggingFace {
        /// the repository's folder in the cache, as `datasets--org--name`.
        folder: String,
        commit: String,
        /// the file's path in the repository, by its names.
        path: Vec<String>,
    },
    /// a file of a kaggle dataset at one version.
    Kaggle {
        owner: String,
        slug: String,
        version: u64,
        path: Vec<String>,
    },
}

/// what `link`, a pinned file's link as data.lock holds it, says of where hugging face or
/// kagglehub would keep the file. data.lock comes with a clone, so every part is checked to stay
/// inside the cache it names.
pub(crate) fn known(link: &str) -> Option<Known> {
    let url = Url::parse(link).ok()?;
    if url.scheme() != "https" || url.port().is_some() || !url.username().is_empty() {
        return None;
    }
    let parts = url
        .path_segments()?
        .map(|part| {
            percent_decode_str(part)
                .decode_utf8()
                .ok()
                .map(std::borrow::Cow::into_owned)
        })
        .collect::<Option<Vec<String>>>()?;
    match url.host_str()? {
        "huggingface.co" if url.query().is_none() => hugging_face(&parts),
        "api.kaggle.com" => kaggle(&url, &parts),
        _ => None,
    }
}

/// a hugging face file link's parts: an optional kind, a repository id of one or two names, then
/// `resolve`, a commit and the file's path.
fn hugging_face(parts: &[String]) -> Option<Known> {
    let (kind, rest) = match parts.first()?.as_str() {
        "datasets" => ("datasets", &parts[1..]),
        "spaces" => ("spaces", &parts[1..]),
        _ => ("models", parts),
    };
    (1..=2).find_map(|length| {
        let id = rest.get(..length)?;
        let [resolve, commit, path @ ..] = rest.get(length..)? else {
            return None;
        };
        let commit_like = commit.len() == 40
            && commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        (resolve == "resolve" && commit_like && id.iter().all(|name| is_repo_name(name)))
            .then_some(())?;
        let path = inside(path)?;
        Some(Known::HuggingFace {
            folder: format!("{kind}--{}", id.join("--")),
            commit: commit.clone(),
            path,
        })
    })
}

/// whether `name` may be one name of a hugging face repository id, as the hub allows them: so a
/// folder made of it stays one folder of the cache.
fn is_repo_name(name: &str) -> bool {
    (1..=96).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !name.contains("--")
        && !name.contains("..")
        && !name.starts_with(['-', '.'])
        && !name.ends_with(['-', '.'])
}

/// a kaggle dataset file link: `/v1/datasets/download/<owner>/<slug>/<path>` with the version in
/// `datasetVersionNumber`.
fn kaggle(url: &Url, parts: &[String]) -> Option<Known> {
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let ["v1", "datasets", "download", owner, slug, file] = parts.as_slice() else {
        return None;
    };
    let version = url
        .query_pairs()
        .find(|(name, _)| name == "datasetVersionNumber")?
        .1
        .parse::<u64>()
        .ok()?;
    let named = |name: &str| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    (named(owner) && named(slug)).then_some(())?;
    let path = inside(&file.split('/').map(str::to_owned).collect::<Vec<_>>())?;
    Some(Known::Kaggle {
        owner: (*owner).to_owned(),
        slug: (*slug).to_owned(),
        version,
        path,
    })
}

/// `names` as a path inside a folder, when the checks every dataset path passes hold for it on
/// this machine: no climbing out, no root, drive or control character.
fn inside(names: &[String]) -> Option<Vec<String>> {
    let path = names.join("/");
    (crate::name::escapes(&path).is_none() && crate::name::unstorable(&path).is_none())
        .then(|| names.to_vec())
}

/// `root` with `names` below it, one folder each.
fn below(root: &Path, names: &[String]) -> PathBuf {
    names
        .iter()
        .fold(root.to_path_buf(), |path, name| path.join(name))
}

/// the repository folders of the hugging face cache at `root`, as `datasets--org--name`: none
/// when `root` holds none, so it is no such cache.
pub(crate) fn repositories(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                ["models--", "datasets--", "spaces--"]
                    .iter()
                    .any(|kind| name.strip_prefix(kind).is_some_and(|id| !id.is_empty()))
            })
        })
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

/// the repository hugging face's `hf cache rm` takes for the cache folder `folder`, as
/// `dataset/org/name` for `datasets--org--name`.
pub(crate) fn removable(folder: &OsStr) -> Option<String> {
    let name = folder.to_str()?;
    let (kind, id) = name.split_once("--")?;
    let kind = kind.strip_suffix('s')?;
    matches!(kind, "model" | "dataset" | "space")
        .then(|| format!("{kind}/{}", id.replace("--", "/")))
}

/// the plain file at `path`, unfollowed, when it is one of `size` bytes: what a candidate copy
/// must be before anything reads it.
fn sized(path: &Path, size: u64) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|found| crate::volume::is_plain(&found) && found.len() == size)
}

/// where hugging face's cache at `root` keeps the bytes the blob entry `blob` of a repository
/// names: the entry itself when it is a plain file, else the shared store's file its link names in
/// the store's own layout, `../../blobs/<2 hex>/<64 hex>`. no link is ever followed.
fn blob_bytes(root: &Path, blob: &Path, size: u64) -> Option<PathBuf> {
    let found = std::fs::symlink_metadata(blob).ok()?;
    if crate::volume::is_plain(&found) {
        return (found.len() == size).then(|| blob.to_path_buf());
    }
    if !found.file_type().is_symlink() {
        return None;
    }
    let target = std::fs::read_link(blob).ok()?;
    let names = relative_names(&target)?;
    let ["..", "..", "blobs", prefix, hash] = names.as_slice() else {
        return None;
    };
    let hexed = |text: &str, length| {
        text.len() == length && text.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    (hexed(prefix, 2) && hexed(hash, 64) && hash.starts_with(prefix)).then_some(())?;
    let stored = root.join("blobs").join(prefix).join(hash);
    sized(&stored, size).then_some(stored)
}

/// the names of a relative link's target, `..` included, with either separator: none for one
/// that starts at a root or a drive.
fn relative_names(target: &Path) -> Option<Vec<&str>> {
    let text = target.to_str()?;
    if Path::new(text).is_absolute() || text.starts_with(['/', '\\']) {
        return None;
    }
    let names: Vec<&str> = text
        .split(['/', '\\'])
        .filter(|name| !name.is_empty() && *name != ".")
        .collect();
    names
        .iter()
        .all(|name| !name.contains(':'))
        .then_some(names)
}

/// the file a snapshot entry `entry` of the repository folder `repository` in hugging face's cache
/// at `root` holds: the entry when it is a plain file, as hugging face leaves one where it makes no
/// links, else the blob its link names in the cache's own layout, `../…/blobs/<hex>`.
fn snapshot_bytes(root: &Path, repository: &Path, entry: &Path, size: u64) -> Option<PathBuf> {
    let found = std::fs::symlink_metadata(entry).ok()?;
    if crate::volume::is_plain(&found) {
        return (found.len() == size).then(|| entry.to_path_buf());
    }
    if !found.file_type().is_symlink() {
        return None;
    }
    let target = std::fs::read_link(entry).ok()?;
    let names = relative_names(&target)?;
    let (climbs, rest) = names.split_at(names.iter().take_while(|name| **name == "..").count());
    let [folder, name] = rest else {
        return None;
    };
    let hashed =
        (name.len() == 40 || name.len() == 64) && name.bytes().all(|byte| byte.is_ascii_hexdigit());
    // the link must climb from the entry's folder back to the repository folder exactly
    let depth = entry
        .parent()?
        .strip_prefix(repository)
        .ok()?
        .components()
        .filter(|part| matches!(part, Component::Normal(_)))
        .count();
    (*folder == "blobs" && hashed && climbs.len() == depth).then_some(())?;
    blob_bytes(root, &repository.join("blobs").join(name), size)
}

/// where hugging face's cache at `root`, whose repository folders are `repositories`, may hold
/// the `size` bytes `digest` pins: each repository's blob named by the SHA-256, then for a file
/// `known` names at a commit, its snapshot entry. each is a plain file of that size; its bytes are
/// still to be hashed.
pub(crate) fn in_hugging_face(
    root: &Path,
    repositories: &[PathBuf],
    digest: &Sha256,
    size: u64,
    known: Option<&Known>,
) -> Vec<PathBuf> {
    let hex = digest.hex();
    let mut found: Vec<PathBuf> = repositories
        .iter()
        .filter_map(|repository| blob_bytes(root, &repository.join("blobs").join(&hex), size))
        .collect();
    if let Some(Known::HuggingFace {
        folder,
        commit,
        path,
    }) = known
    {
        let repository = root.join(folder);
        let entry = below(&repository.join("snapshots").join(commit), path);
        if let Some(bytes) = snapshot_bytes(root, &repository, &entry, size)
            && !found.contains(&bytes)
        {
            found.push(bytes);
        }
    }
    found
}

/// where kagglehub's cache at `root` holds the file `known` names at its version, when kagglehub
/// marked that version, or that file of it, complete: a version it is still writing is left alone.
pub(crate) fn in_kagglehub(root: &Path, known: &Known, size: u64) -> Option<PathBuf> {
    let Known::Kaggle {
        owner,
        slug,
        version,
        path,
    } = known
    else {
        return None;
    };
    let dataset = root.join("datasets").join(owner).join(slug);
    let version = version.to_string();
    let file = below(&dataset.join("versions").join(&version), path);
    (kagglehub_complete(&dataset, &version, path) && sized(&file, size)).then_some(file)
}

/// whether kagglehub marked the version `version` of the dataset, model or competition whose
/// folder is `folder` complete, as a whole or the file `path` of it alone.
fn kagglehub_complete(folder: &Path, version: &str, path: &[String]) -> bool {
    let whole = folder.join(format!("{version}.complete"));
    let mut one = below(&folder.join(".complete").join(version), path).into_os_string();
    one.push(".complete");
    [whole, PathBuf::from(one)]
        .iter()
        .any(|marker| std::fs::symlink_metadata(marker).is_ok_and(|found| found.is_file()))
}

/// whether the file at `relative` below kagglehub's cache may be read: one of a version kagglehub
/// marked complete, a dataset's, model's or competition's. anything else there, as an archive it
/// is still downloading or a marker, is left alone.
fn kagglehub_kept(root: &Path, relative: &[String]) -> bool {
    match relative {
        [kind, owner, slug, versions, version, path @ ..]
            if kind == "datasets" && versions == "versions" && !path.is_empty() =>
        {
            kagglehub_complete(&root.join(kind).join(owner).join(slug), version, path)
        }
        [kind, owner, model, framework, variation, version, path @ ..]
            if kind == "models" && !path.is_empty() =>
        {
            let folder = root
                .join(kind)
                .join(owner)
                .join(model)
                .join(framework)
                .join(variation);
            kagglehub_complete(&folder, version, path)
        }
        [kind, competition, path @ ..] if kind == "competitions" && !path.is_empty() => {
            kagglehub_complete(&root.join(kind), competition, path)
        }
        _ => false,
    }
}

/// every plain file below `folders` of a size among `sizes`, by size, each with the index of its
/// folder: files and folders alike are looked at without following a link, a size is read
/// before anything is opened, and no file is read. `skip` is a folder never walked, fetchloom's
/// own cache. a folder that cannot be read is passed over. fails only at a stop.
pub(crate) fn scan(
    folders: &[Folder],
    sizes: &HashSet<u64>,
    skip: &Path,
    stop: &Stop,
) -> io::Result<HashMap<u64, Vec<(usize, PathBuf)>>> {
    let mut found: HashMap<u64, Vec<(usize, PathBuf)>> = HashMap::new();
    if sizes.is_empty() {
        return Ok(found);
    }
    for (index, folder) in folders.iter().enumerate() {
        let mut waiting = vec![(folder.path.clone(), Vec::<String>::new())];
        while let Some((at, relative)) = waiting.pop() {
            stop.check_io()?;
            if at == skip {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&at) else {
                continue;
            };
            let mut here = Vec::new();
            for entry in entries.filter_map(Result::ok) {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if kind.is_dir() {
                    if relative.len() < DEEPEST && !skipped(folder.tool, &relative, &name) {
                        let mut deeper = relative.clone();
                        deeper.push(name);
                        waiting.push((entry.path(), deeper));
                    }
                    continue;
                }
                if !kind.is_file() || skipped(folder.tool, &relative, &name) {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if !crate::volume::is_plain(&metadata) || !sizes.contains(&metadata.len()) {
                    continue;
                }
                let mut path = relative.clone();
                path.push(name);
                if folder.tool == Some(Tool::Kagglehub) && !kagglehub_kept(&folder.path, &path) {
                    continue;
                }
                here.push((metadata.len(), entry.path()));
            }
            here.sort();
            for (size, path) in here {
                found.entry(size).or_default().push((index, path));
            }
        }
    }
    Ok(found)
}

/// whether a scan passes over the entry `name` in the folder at `relative` below a tool's cache:
/// hugging face's locks and the downloads it has not finished.
fn skipped(tool: Option<Tool>, relative: &[String], name: &str) -> bool {
    match tool {
        Some(Tool::HuggingFace) => {
            (relative.is_empty() && name == ".locks") || name.ends_with(".incomplete")
        }
        Some(Tool::Kagglehub) => name == ".complete",
        None => false,
    }
}

#[cfg(test)]
mod tests;

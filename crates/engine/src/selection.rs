//! Which members of an artifact a run takes, and where they land.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorKind};
use crate::tree::EntryPath;

/// A pattern matched against a canonical member path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Glob(String);

impl Glob {
    /// Builds a pattern from its text.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        Self(pattern.into())
    }

    /// Returns the pattern text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Reports whether this pattern matches a canonical member path.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        let pattern: Vec<&str> = self.0.split('/').collect();
        let components: Vec<&str> = path.split('/').collect();
        matches_components(&pattern, &components)
    }
}

fn matches_components(pattern: &[&str], path: &[&str]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return path.is_empty();
    };
    if *first == "**" {
        return (0..=path.len()).any(|skipped| matches_components(rest, &path[skipped..]));
    }
    match path.split_first() {
        Some((head, tail)) if matches_component(first, head) => matches_components(rest, tail),
        _ => false,
    }
}

fn matches_component(pattern: &str, name: &str) -> bool {
    let pattern = pattern.as_bytes();
    let name = name.as_bytes();
    let mut at_pattern = 0;
    let mut at_name = 0;
    let mut star = None;
    let mut resumed = 0;
    while at_name < name.len() {
        let literal = at_pattern < pattern.len()
            && (pattern[at_pattern] == b'?' || pattern[at_pattern] == name[at_name]);
        if literal {
            at_pattern += 1;
            at_name += 1;
        } else if at_pattern < pattern.len() && pattern[at_pattern] == b'*' {
            star = Some(at_pattern);
            resumed = at_name;
            at_pattern += 1;
        } else if let Some(last) = star {
            at_pattern = last + 1;
            resumed += 1;
            at_name = resumed;
        } else {
            return false;
        }
    }
    pattern[at_pattern..].iter().all(|byte| *byte == b'*')
}

/// How member paths are rewritten on the way to the destination.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(into = "String", try_from = "String")]
pub enum Layout {
    /// Preserve archive paths.
    #[default]
    Keep,
    /// Drop the first n path components.
    Flatten(u32),
}

impl std::fmt::Display for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep => f.write_str("keep"),
            Self::Flatten(dropped) => write!(f, "flatten:{dropped}"),
        }
    }
}

impl From<Layout> for String {
    fn from(layout: Layout) -> Self {
        layout.to_string()
    }
}

impl std::str::FromStr for Layout {
    type Err = String;

    /// Reads the text a layout is written as.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == "keep" {
            return Ok(Self::Keep);
        }
        let Some(count) = text.strip_prefix("flatten:") else {
            return Err(format!("{text} is not keep or flatten:<n>"));
        };
        let dropped: u32 = count
            .parse()
            .map_err(|_| format!("{text} is not keep or flatten:<n>"))?;
        Ok(Self::Flatten(dropped))
    }
}

impl TryFrom<String> for Layout {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

/// One member offered to a selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate<'a> {
    /// The canonical member path, with `/` separators and nothing normalized.
    pub path: &'a str,
    /// Whether the member is a directory, which decides what a layout that
    /// leaves it with no path does with it.
    pub directory: bool,
}

impl<'a> Candidate<'a> {
    /// Builds a candidate for a member that is not a directory.
    #[must_use]
    pub fn file(path: &'a str) -> Self {
        Self {
            path,
            directory: false,
        }
    }

    /// Builds a candidate for a directory member.
    #[must_use]
    pub fn directory(path: &'a str) -> Self {
        Self {
            path,
            directory: true,
        }
    }
}

/// One member a selection took, and the path it lands under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedMember {
    /// Where the member sits in the list the selection was applied to.
    pub index: usize,
    /// The path the member lands under after the layout rewrote it.
    pub path: EntryPath,
}

/// What applying a selection to a list of members produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// The selected members, in the order they were given.
    pub members: Vec<AppliedMember>,
    /// Every directory the selected paths need that no selected member names.
    pub directories: Vec<EntryPath>,
}

/// The include and exclude patterns that make selection part of identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Member paths to include. An empty list means every member.
    pub include: Vec<Glob>,
    /// Member paths to remove, applied after every include.
    pub exclude: Vec<Glob>,
    /// How the selected paths are rewritten.
    pub layout: Layout,
}

impl Selection {
    /// Reports whether one member path survives the include and exclude lists.
    #[must_use]
    pub fn takes(&self, path: &str) -> bool {
        let included =
            self.include.is_empty() || self.include.iter().any(|glob| glob.matches(path));
        included && !self.exclude.iter().any(|glob| glob.matches(path))
    }

    /// Applies this selection to a list of canonical member paths.
    ///
    /// # Errors
    ///
    /// Fails with `reference.unresolved` when nothing matches, and with
    /// `destination.unrepresentable` when the layout leaves a file with no
    /// path, when it leaves nothing at all, or when it produces a path the
    /// destination cannot hold.
    pub fn apply(&self, members: &[Candidate<'_>]) -> Result<Applied, Error> {
        let mut taken = Vec::new();
        for (index, member) in members.iter().enumerate() {
            if self.takes(member.path) {
                taken.push((index, *member));
            }
        }
        if taken.is_empty() {
            return Err(Error::new(
                ErrorKind::ReferenceUnresolved,
                format!(
                    "select a pattern that matches, because none of the {} members matched {}",
                    members.len(),
                    self.patterns()
                ),
            ));
        }

        let mut applied = Applied::default();
        for (index, member) in taken {
            if let Some(path) = self.rewrite(member)? {
                applied.members.push(AppliedMember { index, path });
            }
        }
        if applied.members.is_empty() {
            let Layout::Flatten(dropped) = self.layout else {
                return Err(Error::new(
                    ErrorKind::DestinationUnrepresentable,
                    "select a member that has a path".to_owned(),
                ));
            };
            return Err(Error::new(
                ErrorKind::DestinationUnrepresentable,
                format!(
                    "flatten fewer than {dropped} components, because every selected member is a directory that would be left with no path"
                ),
            ));
        }

        let named: BTreeSet<&str> = applied
            .members
            .iter()
            .map(|member| member.path.as_str())
            .collect();
        let mut ancestors = BTreeSet::new();
        for member in &applied.members {
            let path = member.path.as_str();
            for (at, _) in path.match_indices('/') {
                let ancestor = &path[..at];
                if !named.contains(ancestor) {
                    ancestors.insert(ancestor.to_owned());
                }
            }
        }
        for ancestor in ancestors {
            let path = EntryPath::new(&ancestor)
                .map_err(|reason| unrepresentable(&ancestor, &reason.to_string()))?;
            applied.directories.push(path);
        }
        Ok(applied)
    }

    fn patterns(&self) -> String {
        if self.include.is_empty() {
            return "every member".to_owned();
        }
        self.include
            .iter()
            .map(Glob::as_str)
            .collect::<Vec<&str>>()
            .join(" ")
    }

    fn rewrite(&self, member: Candidate<'_>) -> Result<Option<EntryPath>, Error> {
        let path = member.path;
        let Layout::Flatten(depth) = self.layout else {
            return EntryPath::new(path)
                .map(Some)
                .map_err(|reason| unrepresentable(path, &reason.to_string()));
        };
        let dropped = depth as usize;
        let components: Vec<&str> = path.split('/').collect();
        if components.len() <= dropped {
            if member.directory {
                return Ok(None);
            }
            return Err(Error::new(
                ErrorKind::DestinationUnrepresentable,
                format!(
                    "flatten fewer than {dropped} components, because {path} has {} and would be left with no path",
                    components.len()
                ),
            ));
        }
        let flattened = components[dropped..].join("/");
        EntryPath::new(&flattened)
            .map(Some)
            .map_err(|reason| unrepresentable(path, &reason.to_string()))
    }
}

fn unrepresentable(member: &str, reason: &str) -> Error {
    Error::new(
        ErrorKind::DestinationUnrepresentable,
        format!("rename {member} so that it {reason}"),
    )
}

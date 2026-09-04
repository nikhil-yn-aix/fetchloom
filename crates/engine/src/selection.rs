//! Which members of an artifact a run takes, and where they land.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::{Error, ErrorKind};
use crate::tree::EntryPath;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct Glob {
    pattern: String,
    components: Box<[Box<str>]>,
}

impl Glob {
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        let components = pattern.split('/').map(Box::from).collect();
        Self {
            pattern,
            components,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        let components: Vec<&str> = path.split('/').collect();
        matches_components(&self.components, &components)
    }
}

impl From<String> for Glob {
    fn from(pattern: String) -> Self {
        Self::new(pattern)
    }
}

impl From<Glob> for String {
    fn from(glob: Glob) -> Self {
        glob.pattern
    }
}

impl PartialEq for Glob {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}

impl Eq for Glob {}

impl PartialOrd for Glob {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Glob {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.pattern.cmp(&other.pattern)
    }
}

impl std::hash::Hash for Glob {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.pattern.hash(state);
    }
}

fn matches_components(pattern: &[Box<str>], path: &[&str]) -> bool {
    let mut at_pattern = 0;
    let mut at_path = 0;
    let mut star = None;
    let mut resumed = 0;
    while at_path < path.len() {
        if at_pattern < pattern.len() && &*pattern[at_pattern] == "**" {
            star = Some(at_pattern);
            resumed = at_path;
            at_pattern += 1;
        } else if at_pattern < pattern.len()
            && matches_component(&pattern[at_pattern], path[at_path])
        {
            at_pattern += 1;
            at_path += 1;
        } else if let Some(last) = star {
            at_pattern = last + 1;
            resumed += 1;
            at_path = resumed;
        } else {
            return false;
        }
    }
    pattern[at_pattern..].iter().all(|part| &**part == "**")
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

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(into = "String", try_from = "String")]
pub enum Layout {
    #[default]
    Keep,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub path: &'a str,
    pub directory: bool,
}

impl<'a> Candidate<'a> {
    #[must_use]
    pub fn file(path: &'a str) -> Self {
        Self {
            path,
            directory: false,
        }
    }

    #[must_use]
    pub fn directory(path: &'a str) -> Self {
        Self {
            path,
            directory: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedMember {
    pub index: usize,
    pub path: EntryPath,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub members: Vec<AppliedMember>,
    pub directories: Vec<EntryPath>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub include: Vec<Glob>,
    pub exclude: Vec<Glob>,
    pub layout: Layout,
}

impl Selection {
    #[must_use]
    pub fn takes(&self, path: &str) -> bool {
        let included =
            self.include.is_empty() || self.include.iter().any(|glob| glob.matches(path));
        included && !self.exclude.iter().any(|glob| glob.matches(path))
    }

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

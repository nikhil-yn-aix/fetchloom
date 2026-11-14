//! a recipe dataset as data.toml writes it: each word of its `run`, and the order its inputs
//! give every dataset, so data.toml's checks refuse a cycle before anything runs.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

use super::Entry;

/// the name a word of `run` uses for the folder the result goes in, so no input may have it.
pub(crate) const OUT: &str = "out";

/// one word of a recipe's `run` as data.toml writes it: text, with `{name}` for the folder of an
/// input and `{out}` for the folder the result goes in. `{{` and `}}` stand for one brace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Template {
    text: String,
    pieces: Vec<Piece>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Text(String),
    Folder(String),
}

/// a folder a word of `run` names: an input's, by its name, or the result's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot<'a> {
    Input(&'a str),
    Out,
}

impl Template {
    /// reads one word of `run`, or says why it is refused: a brace that is not part of `{name}`,
    /// `{{` or `}}`, or a control character, which no program reads as meant.
    pub(crate) fn parse(text: &str) -> Result<Template, &'static str> {
        if text.contains(crate::name::is_control) {
            return Err("holds a control character");
        }
        let mut pieces = Vec::new();
        let mut plain = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    plain.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    plain.push('}');
                }
                '}' => return Err("has a } with no { before it"),
                '{' => {
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some('{') | None => return Err("has a { with no } after it"),
                            Some(c) => name.push(c),
                        }
                    }
                    if name.is_empty() {
                        return Err("has {} with no name in it");
                    }
                    if !plain.is_empty() {
                        pieces.push(Piece::Text(std::mem::take(&mut plain)));
                    }
                    pieces.push(Piece::Folder(name));
                }
                c => plain.push(c),
            }
        }
        if !plain.is_empty() {
            pieces.push(Piece::Text(plain));
        }
        Ok(Template {
            text: text.to_owned(),
            pieces,
        })
    }

    /// the word as data.toml writes it, which the key and data.lock keep.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// the folders the word names, in order.
    pub(crate) fn slots(&self) -> impl Iterator<Item = Slot<'_>> {
        self.pieces.iter().filter_map(|piece| match piece {
            Piece::Text(_) => None,
            Piece::Folder(name) if name == OUT => Some(Slot::Out),
            Piece::Folder(name) => Some(Slot::Input(name)),
        })
    }

    /// the word as the step gets it, with each folder `folder` gives in place. a folder stays
    /// inside one argument, spaces and all, since no shell splits it.
    pub(crate) fn expand(&self, folder: &dyn Fn(Slot<'_>) -> OsString) -> OsString {
        let mut word = OsString::new();
        for piece in &self.pieces {
            match piece {
                Piece::Text(text) => word.push(text),
                Piece::Folder(name) if name == OUT => word.push(folder(Slot::Out)),
                Piece::Folder(name) => word.push(folder(Slot::Input(name))),
            }
        }
        word
    }
}

/// every dataset after the datasets it is made from, ties by name, or a cycle's path, as `a`, `b`,
/// `a`. every name a recipe reads must be in `datasets`.
pub(crate) fn order(datasets: &BTreeMap<String, Entry>) -> Result<Vec<&str>, Vec<&str>> {
    let inputs = |name: &str| -> &[String] {
        datasets
            .get(name)
            .and_then(Entry::recipe)
            .map_or(&[], |recipe| recipe.from.as_slice())
    };
    let mut waiting: BTreeMap<&str, usize> = BTreeMap::new();
    let mut readers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for name in datasets.keys() {
        let from = inputs(name);
        waiting.insert(name, from.len());
        for input in from {
            readers.entry(input).or_default().push(name);
        }
    }
    let mut ready: BTreeSet<&str> = waiting
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(name, _)| *name)
        .collect();
    let mut ordered = Vec::with_capacity(datasets.len());
    while let Some(name) = ready.pop_first() {
        ordered.push(name);
        for reader in readers.get(name).into_iter().flatten() {
            if let Some(count) = waiting.get_mut(reader) {
                *count -= 1;
                if *count == 0 {
                    ready.insert(reader);
                }
            }
        }
    }
    if ordered.len() == datasets.len() {
        return Ok(ordered);
    }
    // every dataset left reads another one left, so following inputs from the first name must
    // come back to a name it passed
    let placed: BTreeSet<&str> = ordered.iter().copied().collect();
    let left = |name: &&str| !placed.contains(name);
    let mut path: Vec<&str> = Vec::new();
    let mut at = datasets
        .keys()
        .map(String::as_str)
        .find(left)
        .unwrap_or_default();
    while !path.contains(&at) {
        path.push(at);
        at = inputs(at)
            .iter()
            .map(String::as_str)
            .filter(left)
            .min()
            .unwrap_or(at);
    }
    let start = path.iter().position(|name| *name == at).unwrap_or(0);
    let mut cycle = path.split_off(start);
    cycle.push(at);
    Err(cycle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_name_folders_and_keep_literal_braces() {
        let template = Template::parse("--images={pets}/{{a}}:{out}").unwrap();
        assert_eq!(template.text(), "--images={pets}/{{a}}:{out}");
        assert_eq!(
            template.slots().collect::<Vec<_>>(),
            [Slot::Input("pets"), Slot::Out]
        );
        let folder = |slot: Slot<'_>| match slot {
            Slot::Input(name) => OsString::from(format!("/in/{name} and more")),
            Slot::Out => OsString::from("/out"),
        };
        assert_eq!(
            template.expand(&folder),
            "--images=/in/pets and more/{a}:/out",
            "a folder with spaces stays inside its one argument"
        );
        assert_eq!(Template::parse("plain").unwrap().expand(&folder), "plain");
        for (text, reason) in [
            ("{pets", "has a { with no } after it"),
            ("{a{b}", "has a { with no } after it"),
            ("a}", "has a } with no { before it"),
            ("{}", "has {} with no name in it"),
            ("a\tb", "holds a control character"),
            ("a\nb", "holds a control character"),
        ] {
            assert_eq!(Template::parse(text), Err(reason), "{text}");
        }
    }
}

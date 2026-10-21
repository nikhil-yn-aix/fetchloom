//! What a source answers for a reference: the record's metadata and the files it holds.

use fl_model::digest::Digest;
use fl_model::path::DataPath;
use fl_model::reference::Reference;
use fl_transfer::url::SafeUrl;

/// One file of a listing.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Where the file sits inside the dataset.
    pub path: DataPath,
    /// Its length in bytes, when the source states it.
    pub size: Option<u64>,
    /// Every digest the source states for it.
    pub stated: Vec<Digest>,
    /// Where its bytes can be fetched, in order of preference.
    pub urls: Vec<SafeUrl>,
}

/// What a record says about itself. Every field is optional because sources differ in what
/// they state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// The record's title.
    pub title: Option<String>,
    /// The record's version.
    pub version: Option<String>,
    /// The license identifier.
    pub license: Option<String>,
    /// The DOI.
    pub doi: Option<String>,
}

/// A reference resolved to the files it names.
#[derive(Clone, Debug)]
pub struct Listing {
    /// The reference that names this record exactly, such as a version rather than a concept.
    pub resolved: Reference,
    /// What the record says about itself.
    pub meta: Meta,
    /// The files, in the order the source lists them.
    pub entries: Vec<Entry>,
}

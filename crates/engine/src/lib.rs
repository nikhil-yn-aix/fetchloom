//! Seam traits, domain types, errors, and events for Fetchloom.

pub mod canonical;
pub mod capability;
pub mod conformance;
pub mod credential;
pub mod degrade;
pub mod digest;
pub mod durability;
pub mod error;
pub mod event;
pub mod hashing;
pub mod identity;
pub mod license;
pub mod limits;
pub mod lock;
pub mod manifest;
pub mod outboard;
pub mod outcome;
pub mod partial_key;
pub mod plan;
pub mod pool;
pub mod receipt;
pub mod reconcile;
pub mod redact;
pub mod reference;
pub mod resume;
pub mod seam;
pub mod selection;
pub mod source_record;
pub mod threads;
pub mod timestamp;
pub mod transfer;
pub mod tree;
pub mod trust;
pub mod verification;
pub mod work;

#[cfg(test)]
use serde_json as _;

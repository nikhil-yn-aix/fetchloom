//! The content addressed store of fetchloom: tiers, object layout, linking into projects, fingerprints, roots and garbage collection.

pub mod data_dir;
pub mod error;
mod files;
pub mod fingerprint;
pub mod gc;
mod index;
mod layout;
pub mod link;
mod mode;
mod parallel;
pub mod partial;
#[cfg(unix)]
#[path = "unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;
pub mod roots;
pub mod staged;
pub mod tier;
pub mod verify;

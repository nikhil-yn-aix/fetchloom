//! The Windows calls the seam needs, each wrapped once.

mod clone;
mod credential;
mod encode;
mod file;
mod handle;
mod minifilter;
mod process;
mod registry;
mod security;
mod volume;

pub(crate) use clone::{CLONE_CEILING, clone_spans, duplicate_extents};
pub(crate) use credential::read_credential;
pub(crate) use file::{create_symlink, is_compressed, preallocate, rename};
pub(crate) use handle::{basic_info, id_info, open_for_query};
pub(crate) use minifilter::loaded_minifilters;
pub(crate) use process::{ProcessQuery, process_start, usable_processors};
pub(crate) use registry::{registry_number, registry_string};
pub(crate) use security::{file_owner, process_owners};
pub(crate) use volume::{cluster_bytes, free_space, is_remote_drive, volume_information};

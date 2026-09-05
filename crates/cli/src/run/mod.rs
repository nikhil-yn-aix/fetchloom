//! Executing the commands this build implements, one module per thing a run
//! materializes.

mod adapters;
mod archive;
mod cached;
mod container;
mod context;
mod dataset;
mod local;
mod object;
mod paths;
mod remote;
mod selection;
mod verify;

pub use adapters::{Tuning, adapters_for};
pub(crate) use adapters::{host_of, is_container, is_served};
pub(crate) use cached::materialize_cached;
pub use container::materialize_remote_container;
pub(crate) use context::RunResult;
pub use context::{Materialization, Moved};
pub use dataset::materialize_manifest;
pub(crate) use dataset::write_receipt;
pub(crate) use dataset::{
    DatasetRun, ResolvedArtifact, dataset_name, manifest_at, resolved_object, synthesized_manifest,
};
pub(crate) use local::materialize_local;
pub use paths::resolve_path;
pub(crate) use paths::{default_destination, local_path, remote_name};
pub(crate) use remote::infer_remote;
pub use remote::materialize_remote;
pub use selection::allowed_offline;
pub(crate) use selection::assert_terms;
pub(crate) use verify::verify_tree;

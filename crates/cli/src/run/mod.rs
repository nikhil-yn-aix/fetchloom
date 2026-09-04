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

pub use adapters::{Tuning, adapters_for, host_of, is_container, is_served};
pub use cached::materialize_cached;
pub use container::materialize_remote_container;
pub use context::{Materialization, Moved, RecordedArtifact, RunResult};
pub use dataset::{
    DatasetRun, Provenance, ResolvedArtifact, dataset_name, manifest_at, materialize_manifest,
    resolved_object, synthesized_manifest, write_receipt,
};
pub use local::materialize_local;
pub use paths::{default_destination, executable_paths, local_path, remote_name, resolve_path};
pub use remote::{infer_remote, materialize_remote};
pub use selection::{allowed_offline, assert_terms};
pub use verify::verify_tree;

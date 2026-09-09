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
pub(crate) mod ranged;
mod remote;
mod selection;
mod space;
mod staged;
mod threeway;
mod verify;

pub(crate) use adapters::credential_for;
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
pub(crate) use local::with_resolved_modes;
pub use paths::resolve_path;
pub(crate) use paths::{default_destination, local_path, remote_name, resolve_source_path};
pub(crate) use remote::fetch_into_cache;
pub(crate) use remote::infer_remote;
pub use remote::materialize_remote;
pub use selection::allowed_offline;
pub(crate) use selection::assert_terms;
pub use selection::forbid_when_offline;
pub(crate) use threeway::ASIDE;
pub(crate) use verify::verify_tree;
pub(crate) use verify::{Base, destination_entries};

pub(crate) fn revert_to_record(
    with: &Materialization<'_>,
    destination: &std::path::Path,
    record: &fetchloom_engine::receipt::Receipt,
    yours: &[fetchloom_engine::tree::TreeEntry],
    decisions: &[fetchloom_engine::merge::Merged],
    emit: &dyn Fn(fetchloom_engine::event::EventPayload),
) -> Result<(), fetchloom_engine::error::Error> {
    threeway::to_record(with, destination, record, yours, decisions, emit).map(|_| ())
}

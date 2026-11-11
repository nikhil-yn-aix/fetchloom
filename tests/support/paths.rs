use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) fn relocated(runtime: Option<OsString>, compiled: &str) -> PathBuf {
    runtime.map_or_else(|| PathBuf::from(compiled), PathBuf::from)
}

pub(crate) fn workspace() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        relocated(
            std::env::var_os("CARGO_MANIFEST_DIR"),
            env!("CARGO_MANIFEST_DIR"),
        )
    })
}

//! Turning transferred bytes into files on disk.

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_faults as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use fetchloom_view as _;
use flate2 as _;
#[cfg(target_env = "musl")]
use mimalloc as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use serde_json as _;
use tempfile as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

#[path = "../support/mod.rs"]
mod support;

mod adversarial;
mod archive_reconcile;
mod cache;
mod cancel;
mod diagnose;
mod extract;
mod harness;
mod inference;
mod kernel;
mod manifest;
mod reconcile;
mod relative;
mod symlink;
mod tracked;
mod work;

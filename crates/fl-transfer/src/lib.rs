//! The transfer engine of fetchloom: HTTP connections, retries, resumable ranges, hashing while streaming, credentials, address policy and counters.

pub mod address;
pub mod client;
pub mod counters;
pub mod credentials;
pub mod download;
pub mod error;
pub mod hash;
mod resolve;
mod retry;
pub mod sink;
pub mod url;
mod writer;

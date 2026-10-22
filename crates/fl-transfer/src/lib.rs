//! The transfer engine of fetchloom: HTTP connections, retries, resumable ranges, hashing while streaming, credentials, address policy and counters.

pub mod address;
pub mod client;
pub mod counters;
pub mod credentials;
pub mod download;
pub mod error;
pub mod fetch;
pub mod hash;
pub mod hosts;
mod resolve;
mod retry;
pub mod schedule;
pub mod sink;
mod split;
pub mod url;
mod writer;

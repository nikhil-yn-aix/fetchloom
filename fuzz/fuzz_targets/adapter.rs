#![no_main]

//! Fuzzes `Declarative::from_toml`: loading an adapter never panics, and reading an empty
//! record through any adapter it accepts never panics either.

use std::sync::{Arc, OnceLock};

use fl_sources::{Declarative, Source};
use fl_transfer::client::{Client, Config};
use fl_transfer::credentials::Chain;
use libfuzzer_sys::fuzz_target;
use serde_json::json;

fn client() -> &'static Arc<Client> {
    static CLIENT: OnceLock<Arc<Client>> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let client = Client::new(Config::default(), Arc::new(Chain::new(Vec::new())))
            .expect("a client with the default configuration builds");
        Arc::new(client)
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(adapter) = Declarative::from_toml(text, Arc::clone(client())) else {
        return;
    };
    let scheme = adapter.schemes().first().cloned().unwrap_or_default();
    if let Ok(reference) = format!("{scheme}:1").parse() {
        let _ = adapter.read(&reference, &json!({}));
    }
});

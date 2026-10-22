#![no_main]

//! Fuzzes the `JSONPath` walk: any JSON value read through the zenodo, figshare and dataverse
//! adapters never panics.

use std::sync::{Arc, OnceLock};

use fl_model::reference::Reference;
use fl_sources::Declarative;
use fl_transfer::client::{Client, Config};
use fl_transfer::credentials::Chain;
use libfuzzer_sys::fuzz_target;

fn adapters() -> &'static [(Declarative, Reference)] {
    static ADAPTERS: OnceLock<Vec<(Declarative, Reference)>> = OnceLock::new();
    ADAPTERS.get_or_init(|| {
        let client = Arc::new(
            Client::new(Config::default(), Arc::new(Chain::new(Vec::new())))
                .expect("a client with the default configuration builds"),
        );
        [
            (include_str!("../../sources/zenodo.toml"), "zenodo:1"),
            (include_str!("../../sources/figshare.toml"), "figshare:1"),
            (
                include_str!("../../sources/dataverse.toml"),
                "dataverse:example.org/doi:10.1/x",
            ),
        ]
        .into_iter()
        .map(|(text, reference)| {
            let adapter = Declarative::from_toml(text, Arc::clone(&client))
                .expect("an embedded adapter loads");
            let reference = reference.parse().expect("a fixed reference parses");
            (adapter, reference)
        })
        .collect()
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    for (adapter, reference) in adapters() {
        let _ = adapter.read(reference, &record);
    }
});

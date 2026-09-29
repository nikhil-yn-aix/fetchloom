#![no_main]

//! Fuzzes `Manifest::from_toml`: parsing never panics, valid or not.

use fl_model::manifest::Manifest;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = Manifest::from_toml(text);
});

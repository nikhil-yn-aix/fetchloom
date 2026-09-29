#![no_main]

//! Fuzzes `Lock::from_toml`: parsing never panics, and any lock it accepts round trips to the
//! same bytes and back to an equal value.

use fl_model::lock::Lock;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(lock) = Lock::from_toml(text) else {
        return;
    };
    let written = lock.to_toml();
    let reread = Lock::from_toml(&written).expect("a lock written by fetchloom parses back");
    assert_eq!(reread, lock, "a lock changed value after a round trip");
    assert_eq!(
        reread.to_toml(),
        written,
        "a lock changed bytes after a round trip"
    );
});

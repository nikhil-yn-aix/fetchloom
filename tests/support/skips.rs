use std::sync::OnceLock;

/// whether `FETCHLOOM_TEST_REQUIRE`, read once, names `capability`. ci lists what its runners
/// have, as `script,git,hardlinks`, so a test that lacks one there fails instead of passing
/// without having run.
fn required(capability: &str) -> bool {
    static REQUIRED: OnceLock<Vec<String>> = OnceLock::new();
    REQUIRED
        .get_or_init(|| {
            std::env::var("FETCHLOOM_TEST_REQUIRE")
                .unwrap_or_default()
                .split(',')
                .map(|name| name.trim().to_owned())
                .collect()
        })
        .iter()
        .any(|name| name == capability)
}

/// says `skipped: <reason>` on stderr for a test that stops early, as this machine lacks
/// `capability`, or fails the test when `FETCHLOOM_TEST_REQUIRE` names that capability.
pub(crate) fn skip(capability: &str, reason: &str) {
    assert!(
        !required(capability),
        "{reason}, yet FETCHLOOM_TEST_REQUIRE names {capability}"
    );
    eprintln!("skipped: {reason}");
}

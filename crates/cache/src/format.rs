//! The fingerprint of the on-disk cache format.
//!
//! The format is defined by a set of statements, each naming one fact about
//! what is written and where. The fingerprint is an unordered hash of that set,
//! so nothing about it can be branched on and no statement is first.

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::identity::CacheFormatFingerprint;
use fetchloom_engine::limits::{OUTBOARD_CHUNK_GROUP, OUTBOARD_THRESHOLD};

/// The domain this fingerprint is derived under.
const FORMAT_CONTEXT: &str = "fetchloom cache format";

/// Every statement that defines the on-disk format.
///
/// A statement is added or edited only when what is written to disk changes,
/// and that change makes every existing cache fail with `cache.format_mismatch`.
fn statements() -> Vec<String> {
    vec![
        "the cache root holds objects, outboard, partial, staging, quarantine, receipts, meta, locks, pins, and format"
            .to_owned(),
        "an object is objects/<hex>, where hex is the lowercase hexadecimal of the content digest"
            .to_owned(),
        "an outboard tree is outboard/<hex> and is stored only above the threshold".to_owned(),
        "an in-progress transfer is partial/<hex> and its owner record is partial/<hex>.owner"
            .to_owned(),
        "a staging tree is staging/<name> and its owner record is staging/<name>.owner".to_owned(),
        "a lock is locks/<hex>.lock and is empty, and its holder is locks/<hex>.owner".to_owned(),
        "a pin is pins/<hex> and is empty".to_owned(),
        "an object that failed verification is quarantine/<hex>".to_owned(),
        "a receipt is receipts/<hex>, where hex is the derived-key digest of the absolute destination path"
            .to_owned(),
        "a prune mark is meta/prune/<hex> and holds the instant it was marked".to_owned(),
        "one record per object is meta/object/<hex> and holds its interop digest and its fingerprint"
            .to_owned(),
        "the boot of the last recovery is meta/recovered".to_owned(),
        "the content digest is BLAKE3 over the object bytes".to_owned(),
        "every record is canonical JSON with no unknown keys".to_owned(),
        format!("the outboard chunk group is {OUTBOARD_CHUNK_GROUP} bytes"),
        format!("an outboard tree is stored above {OUTBOARD_THRESHOLD} bytes"),
        "an owner record holds machine, boot, pid, and start".to_owned(),
        "a bundle is an uncompressed tar whose member names are object digests".to_owned(),
        "a prune mark holds digest and marked_nanos".to_owned(),
    ]
}

/// Returns the fingerprint of the format this build writes.
///
/// Combines the statements by exclusive or, so the value cannot depend on the
/// order they are written in.
#[must_use]
pub fn fingerprint() -> CacheFormatFingerprint {
    let mut accumulator = [0u8; 32];
    for statement in statements() {
        let hashed = blake3::derive_key(FORMAT_CONTEXT, statement.as_bytes());
        for (into, from) in accumulator.iter_mut().zip(hashed) {
            *into ^= from;
        }
    }
    let sealed = blake3::derive_key(FORMAT_CONTEXT, &accumulator);
    CacheFormatFingerprint::new(ContentDigest::from_bytes(sealed))
}

/// Renders a fingerprint as the text the format file holds.
#[must_use]
pub fn render(fingerprint: CacheFormatFingerprint) -> String {
    format!("{}\n", fingerprint.digest())
}

#[cfg(test)]
mod tests {
    use super::{FORMAT_CONTEXT, fingerprint, statements};

    fn combine(of: &[String]) -> [u8; 32] {
        let mut accumulator = [0u8; 32];
        for statement in of {
            let hashed = blake3::derive_key(FORMAT_CONTEXT, statement.as_bytes());
            for (into, from) in accumulator.iter_mut().zip(hashed) {
                *into ^= from;
            }
        }
        accumulator
    }

    #[test]
    fn the_order_of_the_statements_does_not_change_the_fingerprint() {
        let forward = statements();
        let mut backward = forward.clone();
        backward.reverse();
        assert_eq!(
            combine(&forward),
            combine(&backward),
            "the fingerprint depends on the order the format is written in"
        );
    }

    #[test]
    fn changing_any_statement_changes_the_fingerprint() {
        let original = statements();
        for index in 0..original.len() {
            let mut changed = original.clone();
            changed[index].push_str(" and one more thing");
            assert_ne!(
                combine(&original),
                combine(&changed),
                "statement {index} does not affect the fingerprint"
            );
        }
    }

    #[test]
    fn no_statement_is_written_twice() {
        let mut seen = statements();
        seen.sort();
        let before = seen.len();
        seen.dedup();
        assert_eq!(
            before,
            seen.len(),
            "a repeated statement cancels itself out of an unordered hash"
        );
    }

    #[test]
    fn the_fingerprint_is_the_same_every_time_it_is_taken() {
        assert_eq!(fingerprint(), fingerprint());
    }
}

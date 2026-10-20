//! Every hash of one stream, computed in one pass over its bytes.

use fl_model::digest::{Algorithm, Digest};
use md5::Digest as _;

/// The hashes of a stream as it passes: BLAKE3 always, and each algorithm a publisher stated
/// beside it, all fed the same buffers on one thread.
#[derive(Clone, Debug)]
pub struct Hasher {
    blake3: blake3::Hasher,
    sha256: Option<sha2::Sha256>,
    sha1: Option<sha1::Sha1>,
    md5: Option<md5::Md5>,
}

/// The hashes a finished stream produced.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hashes {
    /// Its BLAKE3, the object's name.
    pub blake3: [u8; 32],
    /// Every other digest computed, for the algorithms that were stated.
    pub others: Vec<Digest>,
}

/// A stated digest that the bytes do not match.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("the bytes hash to {actual}, not the stated {stated}")]
pub struct Mismatch {
    /// What was stated.
    pub stated: Digest,
    /// What the bytes hash to.
    pub actual: Digest,
}

impl Hasher {
    /// Hashes BLAKE3 and every algorithm among `stated`.
    #[must_use]
    pub fn new(stated: &[Digest]) -> Self {
        let wants = |algorithm| stated.iter().any(|digest| digest.algorithm() == algorithm);
        Self {
            blake3: blake3::Hasher::new(),
            sha256: wants(Algorithm::Sha256).then(sha2::Sha256::new),
            sha1: wants(Algorithm::Sha1).then(sha1::Sha1::new),
            md5: wants(Algorithm::Md5).then(md5::Md5::new),
        }
    }

    /// Feeds `bytes` to every hash.
    pub fn update(&mut self, bytes: &[u8]) {
        self.blake3.update(bytes);
        if let Some(sha256) = &mut self.sha256 {
            sha256.update(bytes);
        }
        if let Some(sha1) = &mut self.sha1 {
            sha1.update(bytes);
        }
        if let Some(md5) = &mut self.md5 {
            md5.update(bytes);
        }
    }

    /// The digests of everything fed so far.
    #[must_use]
    pub fn finish(self) -> Hashes {
        let mut others = Vec::new();
        others.extend(
            self.sha256
                .map(|hash| Digest::Sha256(hash.finalize().into())),
        );
        others.extend(self.sha1.map(|hash| Digest::Sha1(hash.finalize().into())));
        others.extend(self.md5.map(|hash| Digest::Md5(hash.finalize().into())));
        Hashes {
            blake3: *self.blake3.finalize().as_bytes(),
            others,
        }
    }
}

impl Hashes {
    /// The digest computed with `algorithm`, if it was.
    #[must_use]
    pub fn get(&self, algorithm: Algorithm) -> Option<Digest> {
        match algorithm {
            Algorithm::Blake3 => Some(Digest::Blake3(self.blake3)),
            _ => self
                .others
                .iter()
                .copied()
                .find(|digest| digest.algorithm() == algorithm),
        }
    }

    /// Compares every stated digest with the bytes.
    ///
    /// # Errors
    ///
    /// Returns the first [`Mismatch`], or one naming nothing computed when a digest's algorithm
    /// was not hashed.
    pub fn check(&self, stated: &[Digest]) -> Result<(), Mismatch> {
        for digest in stated {
            match self.get(digest.algorithm()) {
                Some(actual) if actual == *digest => {}
                Some(actual) => {
                    return Err(Mismatch {
                        stated: *digest,
                        actual,
                    });
                }
                None => {
                    return Err(Mismatch {
                        stated: *digest,
                        actual: Digest::Blake3(self.blake3),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BYTES: &[u8] = b"the quick brown fox jumps over the lazy dog";

    fn stated() -> Vec<Digest> {
        vec![
            Digest::Md5(md5::Md5::digest(BYTES).into()),
            Digest::Sha1(sha1::Sha1::digest(BYTES).into()),
            Digest::Sha256(sha2::Sha256::digest(BYTES).into()),
            Digest::Blake3(*blake3::hash(BYTES).as_bytes()),
        ]
    }

    #[test]
    fn one_pass_in_pieces_matches_each_hash_alone() {
        let stated = stated();
        let mut pass = Hasher::new(&stated);
        for piece in BYTES.chunks(5) {
            pass.update(piece);
        }
        let hashes = pass.finish();
        assert_eq!(hashes.blake3, *blake3::hash(BYTES).as_bytes());
        assert_eq!(hashes.others.len(), 3);
        assert_eq!(hashes.check(&stated), Ok(()));
    }

    #[test]
    fn only_stated_algorithms_are_computed() {
        let md5 = stated()[0];
        let hashes = {
            let mut hasher = Hasher::new(&[md5]);
            hasher.update(BYTES);
            hasher.finish()
        };
        assert_eq!(hashes.others, [md5]);
        assert_eq!(hashes.get(Algorithm::Sha256), None);
    }

    #[test]
    fn a_mismatch_names_what_was_stated_and_what_came() {
        let mut wrong = [0; 16];
        wrong[0] = 1;
        let stated = [stated()[3], Digest::Md5(wrong)];
        let mut hasher = Hasher::new(&stated);
        hasher.update(BYTES);
        let error = hasher.finish().check(&stated).unwrap_err();
        assert_eq!(error.stated, Digest::Md5(wrong));
        assert_eq!(error.actual, stated_md5());
        let unhashed = Hasher::new(&[])
            .finish()
            .check(&[stated_md5()])
            .unwrap_err();
        assert_eq!(unhashed.stated, stated_md5());
    }

    fn stated_md5() -> Digest {
        stated()[0]
    }
}

const FLUSH_AT: usize = 64 * 1024;

/// Encodes fields into BLAKE3 under a domain context, in a byte form that no library can change.
///
/// Variable length fields carry their length as a little endian u64 before their bytes, so no two
/// sequences of fields encode alike. Fields are gathered in a buffer and hashed in large updates.
pub(crate) struct Encoder {
    hasher: blake3::Hasher,
    buffer: Vec<u8>,
}

impl Encoder {
    /// Starts an encoding under `context`, which separates this use of BLAKE3 from every other.
    pub(crate) fn new(context: &'static str) -> Self {
        Self {
            hasher: blake3::Hasher::new_derive_key(context),
            buffer: Vec::new(),
        }
    }

    /// Adds a variable length byte field.
    pub(crate) fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.u64(bytes.len() as u64);
        self.fixed(bytes)
    }

    /// Adds a string field.
    pub(crate) fn str(&mut self, text: &str) -> &mut Self {
        self.bytes(text.as_bytes())
    }

    /// Adds a number as eight little endian bytes.
    pub(crate) fn u64(&mut self, value: u64) -> &mut Self {
        self.fixed(&value.to_le_bytes())
    }

    /// Adds bytes whose length is fixed by the field, such as a digest of a known algorithm.
    pub(crate) fn fixed(&mut self, bytes: &[u8]) -> &mut Self {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() >= FLUSH_AT {
            self.hasher.update(&self.buffer);
            self.buffer.clear();
        }
        self
    }

    /// Finishes the encoding and returns its BLAKE3 hash.
    pub(crate) fn finish(&mut self) -> [u8; 32] {
        self.hasher.update(&self.buffer);
        self.buffer.clear();
        *self.hasher.finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(context: &str, bytes: &[u8]) -> [u8; 32] {
        *blake3::Hasher::new_derive_key(context)
            .update(bytes)
            .finalize()
            .as_bytes()
    }

    #[test]
    fn prefixes_variable_fields_with_their_length() {
        let mut encoder = Encoder::new("test context");
        let got = encoder.str("ab").bytes(&[1]).u64(7).fixed(&[9, 9]).finish();
        let mut expected = Vec::new();
        expected.extend_from_slice(&2_u64.to_le_bytes());
        expected.extend_from_slice(b"ab");
        expected.extend_from_slice(&1_u64.to_le_bytes());
        expected.push(1);
        expected.extend_from_slice(&7_u64.to_le_bytes());
        expected.extend_from_slice(&[9, 9]);
        assert_eq!(got, hash("test context", &expected));
    }

    #[test]
    fn the_context_separates_encodings() {
        let a = Encoder::new("one").str("x").finish();
        let b = Encoder::new("two").str("x").finish();
        assert_ne!(a, b);
    }

    #[test]
    fn splitting_a_field_changes_the_hash() {
        let joined = Encoder::new("c").str("ab").finish();
        let split = Encoder::new("c").str("a").str("b").finish();
        assert_ne!(joined, split);
    }

    #[test]
    fn flushing_does_not_change_the_hash() {
        let chunk = vec![5_u8; 1000];
        let mut encoder = Encoder::new("c");
        let mut expected = Vec::new();
        for _ in 0..200 {
            encoder.fixed(&chunk);
            expected.extend_from_slice(&chunk);
        }
        assert_eq!(encoder.finish(), hash("c", &expected));
    }
}

use std::fmt;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Writes `bytes` as lowercase hex.
pub(crate) fn write(bytes: &[u8], out: &mut impl fmt::Write) -> fmt::Result {
    for byte in bytes {
        out.write_char(char::from(DIGITS[usize::from(byte >> 4)]))?;
        out.write_char(char::from(DIGITS[usize::from(byte & 0x0f)]))?;
    }
    Ok(())
}

/// Decodes exactly `N` bytes from lowercase hex, refusing any other length or character.
pub(crate) fn decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    let digits = text.as_bytes();
    if digits.len() != 2 * N {
        return None;
    }
    let mut bytes = [0; N];
    let (pairs, _) = digits.as_chunks::<2>();
    for (byte, [high, low]) in bytes.iter_mut().zip(pairs) {
        *byte = (nibble(*high)? << 4) | nibble(*low)?;
    }
    Some(bytes)
}

fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_lowercase_hex() {
        let mut out = String::new();
        write(&[0x00, 0x0f, 0xa0, 0xff], &mut out).unwrap();
        assert_eq!(out, "000fa0ff");
    }

    #[test]
    fn decodes_exact_lowercase_hex_only() {
        assert_eq!(decode::<2>("0aff"), Some([0x0a, 0xff]));
        assert_eq!(decode::<2>("09af"), Some([0x09, 0xaf]));
        assert_eq!(decode::<2>("0AFF"), None);
        assert_eq!(decode::<2>("0af"), None);
        assert_eq!(decode::<2>("0aff00"), None);
        assert_eq!(decode::<1>("g0"), None);
        assert_eq!(decode::<1>("0g"), None);
        assert_eq!(decode::<1>("/0"), None);
        assert_eq!(decode::<1>(":0"), None);
        assert_eq!(decode::<1>("`0"), None);
    }
}

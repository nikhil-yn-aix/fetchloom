//! Reading a hexadecimal digest and a path off one line, which three of the
//! metadata formats do identically.

pub(super) fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(super) fn decode_hex_32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    let raw = text.as_bytes();
    for (index, slot) in bytes.iter_mut().enumerate() {
        let high = hex_nibble(raw[index * 2])?;
        let low = hex_nibble(raw[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }
    Some(bytes)
}

pub(super) fn split_digest_and_path(line: &str) -> Option<(&str, &str)> {
    let boundary = line.find([' ', '\t'])?;
    let (digest, rest) = line.split_at(boundary);
    let rest = rest.trim_start_matches([' ', '\t']);
    if rest.is_empty() {
        return None;
    }
    Some((digest, rest))
}

pub(super) fn algorithm_name_for_length(length: usize) -> Option<&'static str> {
    match length {
        32 => Some("MD5"),
        40 => Some("SHA-1"),
        128 => Some("SHA-512"),
        _ => None,
    }
}

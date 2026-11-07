#[cfg(feature = "json")]
fn sextet(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    }
}

#[cfg(feature = "json")]
pub(crate) fn decode_unpadded(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for chunk in bytes.chunks(4) {
        let mut word: u32 = 0;
        for (index, byte) in chunk.iter().enumerate() {
            word |= sextet(*byte)? << (18 - 6 * index);
        }
        let produced = chunk.len() - 1;
        let octets = [(word >> 16) as u8, (word >> 8) as u8, word as u8];
        if word & ((1 << (24 - 8 * produced)) - 1) != 0 {
            return None;
        }
        out.extend_from_slice(&octets[..produced]);
    }
    Some(out)
}

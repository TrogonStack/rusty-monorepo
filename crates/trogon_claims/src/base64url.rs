use crate::constants::BASE64URL_ALPHABET;

pub(crate) fn encode_unpadded(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let word = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let sextets = [(word >> 18) & 63, (word >> 12) & 63, (word >> 6) & 63, word & 63];
        let emitted = chunk.len() + 1;
        for sextet in sextets.iter().take(emitted) {
            out.push(char::from(BASE64URL_ALPHABET[*sextet as usize]));
        }
    }
    out
}

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

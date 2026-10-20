use crate::constants::BASE64_STANDARD_ALPHABET;

pub(crate) fn encode_padded(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let word = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let sextets = [(word >> 18) & 63, (word >> 12) & 63, (word >> 6) & 63, word & 63];
        let emitted = chunk.len() + 1;
        for (index, sextet) in sextets.iter().enumerate() {
            if index < emitted {
                out.push(char::from(BASE64_STANDARD_ALPHABET[*sextet as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

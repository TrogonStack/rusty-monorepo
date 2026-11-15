//! Standard-alphabet base64 with padding, which is all SSH key blobs, SSHSIG
//! armor and Transit payloads use.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not valid base64")]
pub struct DecodeError;

fn byte_of(sextet: u32) -> u8 {
    match sextet & 63 {
        n @ 0..=25 => b'A' + n as u8,
        n @ 26..=51 => b'a' + (n - 26) as u8,
        n @ 52..=61 => b'0' + (n - 52) as u8,
        62 => b'+',
        _ => b'/',
    }
}

fn sextet_of(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(chunk[0]) << 16) | (u32::from(second) << 8) | u32::from(third);
        out.push(sextet(n >> 18));
        out.push(sextet(n >> 12));
        out.push(if chunk.len() > 1 { sextet(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { sextet(n) } else { '=' });
    }
    out
}

fn sextet(n: u32) -> char {
    char::from(byte_of(n))
}

pub fn decode(text: &str) -> Result<Vec<u8>, DecodeError> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(DecodeError);
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let groups = bytes.len() / 4;
    for (index, group) in bytes.chunks(4).enumerate() {
        let padding = group.iter().rev().take_while(|&&b| b == b'=').count();
        if padding > 2 || (padding > 0 && index + 1 != groups) {
            return Err(DecodeError);
        }
        let mut n = 0u32;
        for &b in &group[..4 - padding] {
            n = (n << 6) | sextet_of(b).ok_or(DecodeError)?;
        }
        n <<= 6 * padding;
        let produced = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&produced[..3 - padding]);
    }
    Ok(out)
}

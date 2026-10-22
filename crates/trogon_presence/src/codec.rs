use std::borrow::Cow;

use crate::constants::{EMPTY_TOKEN, ESCAPE_MARKER, UPPER_HEX_DIGITS};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("token is empty")]
    Empty,
    #[error("byte {byte:#04x} at offset {offset} is outside the token alphabet")]
    InvalidByte { byte: u8, offset: usize },
    #[error("escape at offset {offset} is truncated")]
    TruncatedEscape { offset: usize },
    #[error("escape at offset {offset} is not two uppercase hex digits")]
    InvalidEscape { offset: usize },
    #[error("escape at offset {offset} encodes an in-alphabet byte")]
    NonCanonicalEscape { offset: usize },
    #[error("token does not re-encode to itself")]
    NotCanonical,
}

const fn build_passthrough_table() -> [bool; 256] {
    let mut table = [false; 256];
    let mut byte = 0usize;
    while byte < 256 {
        let b = byte as u8;
        table[byte] = b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
        byte += 1;
    }
    table
}

static PASSTHROUGH: [bool; 256] = build_passthrough_table();

#[inline]
fn passes_through(byte: u8) -> bool {
    PASSTHROUGH[usize::from(byte)]
}

pub fn encode(input: &[u8]) -> Cow<'_, str> {
    if input.is_empty() {
        return Cow::Borrowed(EMPTY_TOKEN);
    }
    let Some(first_escape) = input.iter().position(|&b| !passes_through(b)) else {
        if let Ok(ascii) = std::str::from_utf8(input) {
            return Cow::Borrowed(ascii);
        }
        return Cow::Owned(escape_from(input, input.len()));
    };
    Cow::Owned(escape_from(input, first_escape))
}

fn escape_from(input: &[u8], first_escape: usize) -> String {
    let escapes = input[first_escape..].iter().filter(|&&b| !passes_through(b)).count();
    let mut out = String::with_capacity(input.len() + escapes * 2);
    for &byte in &input[..first_escape] {
        out.push(char::from(byte));
    }
    for &byte in &input[first_escape..] {
        if passes_through(byte) {
            out.push(char::from(byte));
        } else {
            out.push(char::from(ESCAPE_MARKER));
            out.push(char::from(UPPER_HEX_DIGITS[usize::from(byte >> 4)]));
            out.push(char::from(UPPER_HEX_DIGITS[usize::from(byte & 0x0f)]));
        }
    }
    out
}

fn upper_hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

pub fn decode(token: &str) -> Result<Vec<u8>, CodecError> {
    if token == EMPTY_TOKEN {
        return Ok(Vec::new());
    }
    if token.is_empty() {
        return Err(CodecError::Empty);
    }
    let bytes = token.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut offset = 0;
    while offset < bytes.len() {
        let byte = bytes[offset];
        if passes_through(byte) {
            out.push(byte);
            offset += 1;
            continue;
        }
        if byte != ESCAPE_MARKER {
            return Err(CodecError::InvalidByte { byte, offset });
        }
        let (Some(&high), Some(&low)) = (bytes.get(offset + 1), bytes.get(offset + 2)) else {
            return Err(CodecError::TruncatedEscape { offset });
        };
        let (Some(high), Some(low)) = (upper_hex_value(high), upper_hex_value(low)) else {
            return Err(CodecError::InvalidEscape { offset });
        };
        let decoded = (high << 4) | low;
        if passes_through(decoded) {
            return Err(CodecError::NonCanonicalEscape { offset });
        }
        out.push(decoded);
        offset += 3;
    }
    if encode(&out) != token {
        return Err(CodecError::NotCanonical);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn encodes_design_examples() {
        assert_eq!(encode(b"ana@x.io"), "ana=40x=2Eio");
        assert_eq!(encode("jos\u{e9}".as_bytes()), "jos=C3=A9");
        assert_eq!(encode(b""), "=");
        assert_eq!(encode(b"="), "=3D");
        assert_eq!(encode(b"a.b*>c d"), "a=2Eb=2A=3Ec=20d");
    }

    #[test]
    fn in_alphabet_input_is_borrowed() {
        assert!(matches!(encode(b"room_lobby-1"), Cow::Borrowed(_)));
        assert!(matches!(encode(b"room:lobby"), Cow::Owned(_)));
    }

    #[test]
    fn decodes_design_examples() {
        assert_eq!(decode("ana=40x=2Eio"), Ok(b"ana@x.io".to_vec()));
        assert_eq!(decode("jos=C3=A9"), Ok("jos\u{e9}".as_bytes().to_vec()));
        assert_eq!(decode("="), Ok(Vec::new()));
    }

    #[test]
    fn rejects_non_canonical_tokens() {
        assert_eq!(decode(""), Err(CodecError::Empty));
        assert_eq!(decode("a=2e"), Err(CodecError::InvalidEscape { offset: 1 }));
        assert_eq!(decode("a=2"), Err(CodecError::TruncatedEscape { offset: 1 }));
        assert_eq!(decode("a="), Err(CodecError::TruncatedEscape { offset: 1 }));
        assert_eq!(decode("=="), Err(CodecError::TruncatedEscape { offset: 0 }));
        assert_eq!(decode("=41"), Err(CodecError::NonCanonicalEscape { offset: 0 }));
        assert_eq!(decode("=5F"), Err(CodecError::NonCanonicalEscape { offset: 0 }));
        assert_eq!(decode("a.b"), Err(CodecError::InvalidByte { byte: b'.', offset: 1 }));
        assert_eq!(decode("=G0"), Err(CodecError::InvalidEscape { offset: 0 }));
    }

    fn token_unit() -> impl Strategy<Value = String> {
        prop_oneof![
            "[A-Za-z0-9_-]",
            any::<u8>()
                .prop_filter("escaped bytes only", |b| !passes_through(*b))
                .prop_map(|b| format!("={b:02X}")),
        ]
    }

    fn valid_token() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(EMPTY_TOKEN.to_owned()),
            prop::collection::vec(token_unit(), 1..64).prop_map(|units| units.concat()),
        ]
    }

    proptest! {
        #[test]
        fn decode_inverts_encode(input in prop::collection::vec(any::<u8>(), 0..256)) {
            let token = encode(&input);
            prop_assert!(token.bytes().all(|b| passes_through(b) || b == ESCAPE_MARKER));
            prop_assert_eq!(decode(&token), Ok(input));
        }

        #[test]
        fn encode_inverts_decode(token in valid_token()) {
            let decoded = decode(&token);
            prop_assert!(decoded.is_ok(), "{token} rejected: {decoded:?}");
            if let Ok(bytes) = decoded {
                prop_assert_eq!(encode(&bytes), token);
            }
        }

        #[test]
        fn accepted_tokens_are_canonical(token in "[A-Za-z0-9_=a-f-]{0,24}") {
            if let Ok(bytes) = decode(&token) {
                prop_assert_eq!(encode(&bytes), token);
            }
        }
    }
}

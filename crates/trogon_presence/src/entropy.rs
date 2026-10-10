use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use crate::constants::{RANDOM_ID_BYTES, RANDOM_ID_ENCODED_LEN};

#[derive(Debug, thiserror::Error)]
#[error("operating system randomness unavailable: {0}")]
pub struct EntropyError(getrandom::Error);

pub(crate) fn random_id_bytes() -> Result<[u8; RANDOM_ID_BYTES], EntropyError> {
    let mut bytes = [0u8; RANDOM_ID_BYTES];
    getrandom::fill(&mut bytes).map_err(EntropyError)?;
    Ok(bytes)
}

pub(crate) fn encode_id(bytes: &[u8; RANDOM_ID_BYTES]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn decode_id(encoded: &str) -> Option<[u8; RANDOM_ID_BYTES]> {
    if encoded.len() != RANDOM_ID_ENCODED_LEN {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    decoded.try_into().ok()
}

use std::fmt;

use serde_json::Value;
use trogon_presence::{CanonicalJsonV1, Meta, MetaError};

use crate::runtime::HookFailure;

const RAW_OUTPUT_MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputLimit {
    Raw,
    Envelope,
}

impl fmt::Display for OutputLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw => write!(f, "raw output above {RAW_OUTPUT_MAX_BYTES} bytes"),
            Self::Envelope => f.write_str("meta above the presence envelope limit"),
        }
    }
}

pub(crate) struct RawOutput(Vec<u8>);

impl RawOutput {
    pub(crate) fn new(bytes: Vec<u8>) -> Result<Self, HookFailure> {
        if bytes.len() > RAW_OUTPUT_MAX_BYTES {
            return Err(HookFailure::Oversized(OutputLimit::Raw));
        }
        Ok(Self(bytes))
    }

    pub(crate) fn into_meta(self) -> Result<Meta, HookFailure> {
        let parsed = serde_json::from_slice::<CanonicalJsonV1>(&self.0)
            .map_err(|err| HookFailure::Malformed(err.to_string()))?
            .into_value();
        let Value::Object(map) = parsed else {
            return Err(HookFailure::Malformed("hook output is not a JSON object".to_owned()));
        };
        Meta::try_from(map).map_err(|err| match err {
            MetaError::TooLarge { .. } => HookFailure::Oversized(OutputLimit::Envelope),
            other => HookFailure::Malformed(other.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_raw_output_above_sixteen_kib_before_parsing() {
        assert!(RawOutput::new(vec![b' '; RAW_OUTPUT_MAX_BYTES]).is_ok());
        assert!(matches!(
            RawOutput::new(vec![b' '; RAW_OUTPUT_MAX_BYTES + 1]),
            Err(HookFailure::Oversized(OutputLimit::Raw))
        ));
    }

    #[test]
    fn rejects_meta_above_the_envelope_cap() -> Result<(), HookFailure> {
        let padding = "x".repeat(5 * 1024);
        let output = RawOutput::new(format!("{{\"padding\":\"{padding}\"}}").into_bytes())?;
        assert!(matches!(
            output.into_meta(),
            Err(HookFailure::Oversized(OutputLimit::Envelope))
        ));
        Ok(())
    }

    #[test]
    fn rejects_framework_refs_and_non_objects() -> Result<(), HookFailure> {
        for body in [&br#"{"phx_ref":"forged"}"#[..], b"[1]", b"{", &[255]] {
            assert!(matches!(
                RawOutput::new(body.to_vec())?.into_meta(),
                Err(HookFailure::Malformed(_))
            ));
        }
        Ok(())
    }
}

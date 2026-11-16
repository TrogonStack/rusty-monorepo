use std::sync::Arc;

use crate::claim::Claim;
use crate::claim_set::ClaimSet;
use crate::dynamic_claims::ClaimShape;
use crate::dynamic_claims::DynamicClaims;
use crate::ingest::projection::project_member;
use crate::ingest_error::IngestError;
use crate::principal_key::PrincipalKey;
use crate::raw_claims::RawClaims;
use crate::raw_json::RawKind;

pub trait IngestClaims: ClaimSet + Sized {
    fn try_from_raw(raw: &RawClaims, key: &PrincipalKey) -> Result<Self, IngestError>;
}

impl IngestClaims for DynamicClaims {
    fn try_from_raw(raw: &RawClaims, key: &PrincipalKey) -> Result<Self, IngestError> {
        let issuer = Arc::new(key.issuer().clone());
        let mut claims: Vec<Claim> = Vec::with_capacity(raw.len());
        let mut unprojected = Vec::new();
        let mut source_shapes = Vec::with_capacity(raw.len());
        for (name, value) in raw.iter() {
            let shape = if value.kind() == RawKind::Array {
                ClaimShape::Array
            } else {
                ClaimShape::Scalar
            };
            source_shapes.push((name.clone(), shape));
            project_member(&mut claims, &mut unprojected, name, value, &issuer)?;
        }
        Ok(Self {
            claims,
            unprojected,
            source_shapes,
        })
    }
}

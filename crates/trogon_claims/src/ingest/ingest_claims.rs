use std::sync::Arc;

use crate::claim::Claim;
use crate::claim_set::ClaimSet;
use crate::dynamic_claims::DynamicClaims;
use crate::ingest::projection::project_member;
use crate::ingest_error::IngestError;
use crate::principal_key::PrincipalKey;
use crate::raw_claims::RawClaims;

pub trait IngestClaims: ClaimSet + Sized {
    fn try_from_raw(raw: &RawClaims, key: &PrincipalKey) -> Result<Self, IngestError>;
}

impl IngestClaims for DynamicClaims {
    fn try_from_raw(raw: &RawClaims, key: &PrincipalKey) -> Result<Self, IngestError> {
        let issuer = Arc::new(key.issuer().clone());
        let mut claims: Vec<Claim> = Vec::with_capacity(raw.len());
        let mut unprojected = Vec::new();
        for (name, value) in raw.iter() {
            project_member(&mut claims, &mut unprojected, name, value, &issuer)?;
        }
        Ok(Self { claims, unprojected })
    }
}

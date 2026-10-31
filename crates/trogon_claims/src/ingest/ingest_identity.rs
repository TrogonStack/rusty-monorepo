use std::sync::Arc;

use crate::binding::Binding;
use crate::claim_set::ClaimSet;
use crate::delegation::Delegation;
use crate::identity::{AuthenticatedIdentity, Identity, IdentityParts};
use crate::ingest::actor::{parse_act, parse_may_act};
use crate::ingest::confirmation::parse_confirmation;
use crate::ingest::context::parse_context;
use crate::ingest::IngestClaims;
use crate::ingest_error::{ClaimValueError, IngestError};
use crate::ingest_profile::IngestProfile;
use crate::ingested::Ingested;
use crate::issuer::Issuer;
use crate::principal_key::PrincipalKey;
use crate::raw_claims::RawClaims;
use crate::subject_id::SubjectId;
use crate::well_known_claim::WellKnownClaim;

fn string_member(raw: &RawClaims, claim: WellKnownClaim) -> Result<Option<String>, IngestError> {
    match raw.get_named(claim.as_str()) {
        Some(value) => value
            .as_string()
            .map(Some)
            .ok_or_else(|| IngestError::claim(claim, ClaimValueError::WrongType { expected: "string" })),
        None => Ok(None),
    }
}

fn principal_key(raw: &RawClaims, profile: &IngestProfile) -> Result<PrincipalKey, IngestError> {
    let sub = string_member(raw, WellKnownClaim::Sub)?
        .ok_or_else(|| IngestError::claim(WellKnownClaim::Sub, ClaimValueError::MissingSubject))?;
    let sub = SubjectId::try_new(sub).map_err(|error| IngestError::claim(WellKnownClaim::Sub, error))?;
    let issuer = match string_member(raw, WellKnownClaim::Iss)? {
        Some(text) => Issuer::try_new(text).map_err(|error| IngestError::claim(WellKnownClaim::Iss, error))?,
        None => profile
            .default_issuer()
            .cloned()
            .ok_or_else(|| IngestError::claim(WellKnownClaim::Iss, ClaimValueError::MissingIssuer))?,
    };
    Ok(PrincipalKey::new(issuer, sub))
}

impl<C: IngestClaims + ClaimSet> Identity<C> {
    pub fn ingest(raw: &RawClaims, profile: &IngestProfile) -> Result<Ingested<C>, IngestError> {
        let key = principal_key(raw, profile)?;
        let claims = C::try_from_raw(raw, &key)?;
        let token_issuer = Arc::new(key.issuer().clone());
        let actor = match raw.get_named(WellKnownClaim::Act.as_str()) {
            Some(value) => Some(Arc::new(parse_act(value, &token_issuer)?)),
            None => None,
        };
        let may_act = match raw.get_named(WellKnownClaim::MayAct.as_str()) {
            Some(value) => Some(parse_may_act(value, &token_issuer)?),
            None => None,
        };
        let binding = match raw.get_named(WellKnownClaim::Cnf.as_str()) {
            Some(value) => Binding::Asserted(Box::new(
                parse_confirmation(value).map_err(|error| IngestError::claim(WellKnownClaim::Cnf, error))?,
            )),
            None => Binding::Bearer,
        };
        let context = parse_context(raw)?;
        let delegation = match &actor {
            Some(actor) => Delegation::Delegated(Arc::clone(actor)),
            None => Delegation::Direct,
        };
        let parts = IdentityParts {
            authentication_type: profile.authentication_type().clone(),
            claims,
            name_claim_type: profile.name_claim_type().clone(),
            role_claim_type: profile.role_claim_type().clone(),
            key,
            delegation,
            binding,
            may_act,
            context,
        };
        let identity = Arc::new(AuthenticatedIdentity::from_parts(parts));
        Ok(match actor {
            Some(actor) => Ingested::Delegated { identity, actor },
            None => Ingested::Direct(identity),
        })
    }
}

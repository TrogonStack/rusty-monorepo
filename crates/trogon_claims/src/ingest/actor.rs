use std::sync::Arc;

use crate::actor_error::ActorError;
use crate::actor_issuer::ActorIssuer;
use crate::actor_key::ActorKey;
use crate::authorized_actor::AuthorizedActor;
use crate::claim::Claim;
use crate::claim_type::ClaimType;
use crate::client_id::ClientId;
use crate::constants::MAX_ACTOR_DEPTH;
use crate::current_actor::CurrentActor;
use crate::ingest::projection::project_member;
use crate::ingest_error::{ClaimValueError, IngestError};
use crate::issuer::Issuer;
use crate::prior_actor::PriorActor;
use crate::raw_claims::RawClaims;
use crate::raw_claims_error::RawClaimsError;
use crate::raw_json::RawJson;
use crate::subject_id::SubjectId;
use crate::well_known_claim::WellKnownClaim;

struct ParsedActor {
    issuer: ActorIssuer,
    sub: Option<SubjectId>,
    client_id: Option<ClientId>,
    claims: Vec<Claim>,
    nested: Option<RawJson>,
}

fn object_of(raw: &RawJson) -> Result<RawClaims, ClaimValueError> {
    RawClaims::from_object(raw).map_err(|error| match error {
        RawClaimsError::NotObject => ClaimValueError::WrongType { expected: "object" },
        other => ClaimValueError::Nested(other),
    })
}

fn text_of(raw: &RawJson) -> Result<String, ClaimValueError> {
    raw.as_string().ok_or(ClaimValueError::WrongType { expected: "string" })
}

fn parse_one(object: &RawClaims, token_issuer: &Arc<Issuer>, consume_act: bool) -> Result<ParsedActor, IngestError> {
    let context = |kind: ClaimValueError, owner: WellKnownClaim| IngestError::claim(owner, kind);
    let owner = if consume_act {
        WellKnownClaim::Act
    } else {
        WellKnownClaim::MayAct
    };
    let mut issuer = ActorIssuer::Unstated;
    let mut sub = None;
    let mut client_id = None;
    let mut nested = None;
    for (name, value) in object.iter() {
        match name.well_known() {
            Some(WellKnownClaim::Iss) => {
                let text = text_of(value).map_err(|kind| context(kind, owner))?;
                issuer = ActorIssuer::Stated(Issuer::try_new(text).map_err(|error| context(error.into(), owner))?);
            }
            Some(WellKnownClaim::Sub) => {
                let text = text_of(value).map_err(|kind| context(kind, owner))?;
                sub = Some(SubjectId::try_new(text).map_err(|error| context(error.into(), owner))?);
            }
            Some(WellKnownClaim::ClientId) => {
                let text = text_of(value).map_err(|kind| context(kind, owner))?;
                client_id = Some(ClientId::try_new(text).map_err(|error| context(error.into(), owner))?);
            }
            Some(WellKnownClaim::Act) if consume_act => nested = Some(value.clone()),
            _ => {}
        }
    }
    let carries_sub = sub.is_some();
    let mut claims = Vec::new();
    let mut discarded: Vec<ClaimType> = Vec::new();
    for (name, value) in object.iter() {
        let consumed = match name.well_known() {
            Some(
                WellKnownClaim::Iss
                | WellKnownClaim::Sub
                | WellKnownClaim::Exp
                | WellKnownClaim::Nbf
                | WellKnownClaim::Aud,
            ) => true,
            Some(WellKnownClaim::Act) => consume_act,
            Some(WellKnownClaim::ClientId) => consume_act && !carries_sub,
            _ => false,
        };
        if !consumed {
            project_member(&mut claims, &mut discarded, name, value, token_issuer)?;
        }
    }
    Ok(ParsedActor {
        issuer,
        sub,
        client_id,
        claims,
        nested,
    })
}

fn actor_key(parsed: &ParsedActor) -> Result<ActorKey, ActorError> {
    match (&parsed.sub, &parsed.client_id) {
        (Some(sub), _) => Ok(ActorKey::Subject {
            issuer: parsed.issuer.clone(),
            sub: sub.clone(),
        }),
        (None, Some(client_id)) => Ok(ActorKey::Client {
            issuer: parsed.issuer.clone(),
            client_id: client_id.clone(),
        }),
        (None, None) => Err(ActorError::MissingKey),
    }
}

pub(crate) fn parse_act(raw: &RawJson, token_issuer: &Arc<Issuer>) -> Result<CurrentActor, IngestError> {
    let owner = WellKnownClaim::Act;
    let fail = |kind: ClaimValueError| IngestError::claim(owner, kind);
    let mut object = object_of(raw).map_err(fail)?;
    let mut parsed: Vec<(ActorKey, Vec<Claim>)> = Vec::new();
    loop {
        if parsed.len() >= MAX_ACTOR_DEPTH {
            return Err(fail(
                ActorError::TooDeep {
                    depth: parsed.len() + 1,
                    max: MAX_ACTOR_DEPTH,
                }
                .into(),
            ));
        }
        let actor = parse_one(&object, token_issuer, true)?;
        let key = actor_key(&actor).map_err(|error| fail(error.into()))?;
        parsed.push((key, actor.claims));
        match actor.nested {
            Some(nested) => object = object_of(&nested).map_err(fail)?,
            None => break,
        }
    }
    let mut chain = parsed.into_iter();
    let (current_key, current_claims) = chain.next().ok_or_else(|| fail(ActorError::MissingKey.into()))?;
    let mut prior: Option<PriorActor> = None;
    for (key, claims) in chain.rev() {
        prior = Some(PriorActor::try_new(key, claims, prior).map_err(|error| fail(error.into()))?);
    }
    CurrentActor::try_new(current_key, current_claims, prior).map_err(|error| fail(error.into()))
}

pub(crate) fn parse_may_act(raw: &RawJson, token_issuer: &Arc<Issuer>) -> Result<AuthorizedActor, IngestError> {
    let owner = WellKnownClaim::MayAct;
    let fail = |kind: ClaimValueError| IngestError::claim(owner, kind);
    let object = object_of(raw).map_err(fail)?;
    let actor = parse_one(&object, token_issuer, false)?;
    AuthorizedActor::try_new(actor.issuer, actor.sub, actor.client_id, actor.claims).map_err(|error| fail(error.into()))
}

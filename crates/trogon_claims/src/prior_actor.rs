use crate::actor_error::ActorError;
use crate::actor_key::ActorKey;
use crate::claim::Claim;
use crate::claim_type::ClaimType;
use crate::constants::MAX_ACTOR_DEPTH;

/// A historical actor in a nested `act` chain. Under
/// [RFC 8693 §4.1](https://www.rfc-editor.org/rfc/rfc8693.html#section-4.1), prior
/// actors are informational and must not be used for access-control decisions.
#[derive(Clone, Debug, PartialEq)]
pub struct PriorActor {
    key: ActorKey,
    claims: Vec<Claim>,
    prior: Option<Box<PriorActor>>,
    depth: usize,
}

impl PriorActor {
    pub fn try_new(key: ActorKey, claims: Vec<Claim>, prior: Option<PriorActor>) -> Result<Self, ActorError> {
        let depth = 1 + prior.as_ref().map_or(0, PriorActor::depth);
        if depth >= MAX_ACTOR_DEPTH {
            return Err(ActorError::TooDeep {
                depth: depth + 1,
                max: MAX_ACTOR_DEPTH,
            });
        }
        Ok(Self {
            key,
            claims,
            prior: prior.map(Box::new),
            depth,
        })
    }

    pub fn key(&self) -> &ActorKey {
        &self.key
    }

    pub fn claims(&self) -> &[Claim] {
        &self.claims
    }

    pub fn find_first(&self, claim_type: &ClaimType) -> Option<&Claim> {
        self.claims.iter().find(|claim| claim.claim_type() == claim_type)
    }

    pub fn prior(&self) -> Option<&PriorActor> {
        self.prior.as_deref()
    }

    pub(crate) fn depth(&self) -> usize {
        self.depth
    }
}

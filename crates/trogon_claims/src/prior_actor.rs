use crate::actor_error::ActorError;
use crate::actor_key::ActorKey;
use crate::claim::Claim;
use crate::claim_type::ClaimType;
use crate::constants::MAX_ACTOR_DEPTH;

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

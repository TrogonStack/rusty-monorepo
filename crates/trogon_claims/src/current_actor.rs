use crate::actor_error::ActorError;
use crate::actor_key::ActorKey;
use crate::claim::Claim;
use crate::claim_type::ClaimType;
use crate::constants::MAX_ACTOR_DEPTH;
use crate::prior_actor::PriorActor;

#[derive(Clone, Debug, PartialEq)]
pub struct CurrentActor {
    key: ActorKey,
    claims: Vec<Claim>,
    prior: Option<PriorActor>,
}

impl CurrentActor {
    pub fn try_new(key: ActorKey, claims: Vec<Claim>, prior: Option<PriorActor>) -> Result<Self, ActorError> {
        let depth = 1 + prior.as_ref().map_or(0, PriorActor::depth);
        if depth > MAX_ACTOR_DEPTH {
            return Err(ActorError::TooDeep {
                depth,
                max: MAX_ACTOR_DEPTH,
            });
        }
        Ok(Self { key, claims, prior })
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

    pub fn prior_actors(&self) -> PriorActors<'_> {
        PriorActors {
            next: self.prior.as_ref(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PriorActors<'a> {
    next: Option<&'a PriorActor>,
}

impl<'a> Iterator for PriorActors<'a> {
    type Item = &'a PriorActor;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.next?;
        self.next = current.prior();
        Some(current)
    }
}

use crate::actor_error::ActorError;
use crate::actor_issuer::ActorIssuer;
use crate::actor_key::ActorKey;
use crate::claim::Claim;
use crate::client_id::ClientId;
use crate::subject_id::SubjectId;

#[derive(Clone, Debug, PartialEq)]
pub struct AuthorizedActor {
    issuer: ActorIssuer,
    sub: Option<SubjectId>,
    client_id: Option<ClientId>,
    claims: Vec<Claim>,
}

impl AuthorizedActor {
    pub fn try_new(
        issuer: ActorIssuer,
        sub: Option<SubjectId>,
        client_id: Option<ClientId>,
        claims: Vec<Claim>,
    ) -> Result<Self, ActorError> {
        if sub.is_none() && client_id.is_none() {
            return Err(ActorError::MissingKey);
        }
        Ok(Self {
            issuer,
            sub,
            client_id,
            claims,
        })
    }

    pub fn issuer(&self) -> &ActorIssuer {
        &self.issuer
    }

    pub fn subject(&self) -> Option<&SubjectId> {
        self.sub.as_ref()
    }

    pub fn client_id(&self) -> Option<&ClientId> {
        self.client_id.as_ref()
    }

    pub fn claims(&self) -> &[Claim] {
        &self.claims
    }

    pub fn names(&self, key: &ActorKey) -> bool {
        let (Some(left), Some(right)) = (self.issuer.resolved(), key.issuer().resolved()) else {
            return false;
        };
        if left != right {
            return false;
        }
        match key {
            ActorKey::Subject { sub, .. } => self.sub.as_ref() == Some(sub),
            ActorKey::Client { client_id, .. } => self.client_id.as_ref() == Some(client_id),
        }
    }
}

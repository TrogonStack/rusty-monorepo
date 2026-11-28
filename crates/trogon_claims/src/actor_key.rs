use crate::actor_issuer::ActorIssuer;
use crate::client_id::ClientId;
use crate::issuer::Issuer;
use crate::subject_id::SubjectId;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ActorKey {
    Subject { issuer: ActorIssuer, sub: SubjectId },
    Client { issuer: ActorIssuer, client_id: ClientId },
}

impl ActorKey {
    pub fn issuer(&self) -> &ActorIssuer {
        match self {
            Self::Subject { issuer, .. } | Self::Client { issuer, .. } => issuer,
        }
    }

    pub fn subject(&self) -> Option<&SubjectId> {
        match self {
            Self::Subject { sub, .. } => Some(sub),
            Self::Client { .. } => None,
        }
    }

    pub fn client_id(&self) -> Option<&ClientId> {
        match self {
            Self::Client { client_id, .. } => Some(client_id),
            Self::Subject { .. } => None,
        }
    }

    pub fn assume_token_issuer(self, token_issuer: &Issuer) -> Self {
        match self {
            Self::Subject { issuer, sub } => Self::Subject {
                issuer: issuer.assume_token_issuer(token_issuer),
                sub,
            },
            Self::Client { issuer, client_id } => Self::Client {
                issuer: issuer.assume_token_issuer(token_issuer),
                client_id,
            },
        }
    }
}

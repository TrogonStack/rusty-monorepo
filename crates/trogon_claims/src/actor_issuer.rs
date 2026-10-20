use crate::issuer::Issuer;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ActorIssuer {
    Stated(Issuer),
    Unstated,
    AssumedFromToken(Issuer),
}

impl ActorIssuer {
    pub fn assume_token_issuer(self, token_issuer: &Issuer) -> Self {
        match self {
            Self::Unstated => Self::AssumedFromToken(token_issuer.clone()),
            resolved => resolved,
        }
    }

    pub fn resolved(&self) -> Option<&Issuer> {
        match self {
            Self::Stated(issuer) | Self::AssumedFromToken(issuer) => Some(issuer),
            Self::Unstated => None,
        }
    }
}

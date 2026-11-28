use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ActorError {
    #[error("an authorized actor must name a subject or a client id")]
    MissingKey,
    #[error("actor chain of depth {depth} exceeds the maximum of {max}")]
    TooDeep { depth: usize, max: usize },
}

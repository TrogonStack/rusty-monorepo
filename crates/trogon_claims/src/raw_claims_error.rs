use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RawClaimsError {
    #[error("claims document is not a JSON object")]
    NotObject,
    #[error("claims document repeats the member {name:?}")]
    Duplicate { name: String },
    #[error("claims document has an empty member name")]
    EmptyName,
    #[error("claims document is not valid JSON at line {line} column {column}")]
    InvalidJson { line: usize, column: usize },
    #[error("compact token does not have exactly three segments")]
    NotCompactJwt,
    #[error("token payload is not unpadded base64url")]
    InvalidBase64,
}

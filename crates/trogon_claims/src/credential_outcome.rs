#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RejectReason {
    Expired,
    NotYetValid,
    Malformed,
    SignatureInvalid,
    AudienceMismatch,
    IssuerNotTrusted,
    Revoked,
    Unspecified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CredentialOutcome {
    Absent,
    Rejected(RejectReason),
}

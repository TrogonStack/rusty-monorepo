use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::claims::{AccountName, ConnectClaims, ConnectIdentity, TokenId, UnixSeconds};

pub const CONNECT_TOKEN_MAX_LIFETIME: Duration = Duration::from_secs(60);
pub const CONNECT_TOKEN_LEEWAY: Duration = Duration::from_secs(30);
const CONNECT_TOKEN_MAX_BYTES: usize = 4096;
const KEY_ID_MAX_BYTES: usize = 64;
const JWS_ALGORITHM: &str = "EdDSA";
const JWS_TYPE: &str = "JWT";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KeyId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("kid must be 1 to {KEY_ID_MAX_BYTES} bytes of [A-Za-z0-9_.-]")]
    KeyId,
    #[error("key must be 32 bytes in unpadded base64url")]
    Encoding,
    #[error("public key is not a valid Ed25519 point")]
    Point,
    #[error("key ring entry must be <kid>=<base64url public key>")]
    Entry,
    #[error("key id {0:?} is configured twice")]
    Duplicate(KeyId),
}

impl KeyId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for KeyId {
    type Error = KeyError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        let valid = !raw.is_empty()
            && raw.len() <= KEY_ID_MAX_BYTES
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
        if !valid {
            return Err(KeyError::KeyId);
        }
        Ok(Self(raw))
    }
}

impl FromStr for KeyId {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<KeyId> for String {
    fn from(id: KeyId) -> Self {
        id.0
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn decode_key_bytes(text: &str) -> Result<[u8; 32], KeyError> {
    B64.decode(text.trim())
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or(KeyError::Encoding)
}

#[derive(Clone)]
pub struct TokenSigningKey(SigningKey);

impl TokenSigningKey {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed)?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    pub fn verifying_key(&self) -> TokenVerifyingKey {
        TokenVerifyingKey(self.0.verifying_key())
    }

    pub fn to_base64(&self) -> String {
        B64.encode(self.0.to_bytes())
    }
}

impl FromStr for TokenSigningKey {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(SigningKey::from_bytes(&decode_key_bytes(s)?)))
    }
}

impl fmt::Debug for TokenSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenSigningKey(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenVerifyingKey(VerifyingKey);

impl TokenVerifyingKey {
    pub fn to_base64(&self) -> String {
        B64.encode(self.0.as_bytes())
    }
}

impl FromStr for TokenVerifyingKey {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        VerifyingKey::from_bytes(&decode_key_bytes(s)?)
            .map(Self)
            .map_err(|_| KeyError::Point)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRingEntry {
    pub kid: KeyId,
    pub key: TokenVerifyingKey,
}

impl FromStr for KeyRingEntry {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (kid, key) = s.split_once('=').ok_or(KeyError::Entry)?;
        Ok(Self {
            kid: kid.parse()?,
            key: key.parse()?,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct KeyRing(HashMap<KeyId, TokenVerifyingKey>);

impl KeyRing {
    pub fn new(entries: impl IntoIterator<Item = KeyRingEntry>) -> Result<Self, KeyError> {
        let mut keys = HashMap::new();
        for entry in entries {
            if keys.insert(entry.kid.clone(), entry.key).is_some() {
                return Err(KeyError::Duplicate(entry.kid));
            }
        }
        Ok(Self(keys))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn get(&self, kid: &KeyId) -> Option<&TokenVerifyingKey> {
        self.0.get(kid)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConnectToken(String);

impl ConnectToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ConnectToken {
    fn from(raw: String) -> Self {
        Self(raw)
    }
}

impl fmt::Debug for ConnectToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectToken(<redacted>)")
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct JwsHeader {
    alg: String,
    typ: String,
    kid: KeyId,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("connect token is missing")]
    Missing,
    #[error("connect token exceeds {CONNECT_TOKEN_MAX_BYTES} bytes")]
    TooLarge,
    #[error("connect token is not a compact JWS")]
    Malformed,
    #[error("connect token header is invalid")]
    Header,
    #[error("connect token uses an unsupported algorithm")]
    Algorithm,
    #[error("connect token kid is not configured")]
    UnknownKey,
    #[error("connect token signature is invalid")]
    Signature,
    #[error("connect token claims are invalid")]
    Claims,
    #[error("connect token lifetime exceeds the maximum")]
    Lifetime,
    #[error("connect token was issued in the future")]
    NotYetValid,
    #[error("connect token is expired")]
    Expired,
    #[error("connect token could not be signed")]
    Encode,
    #[error("session lifetime is exhausted")]
    SessionExhausted,
}

impl TokenError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Missing => "token_missing",
            Self::TooLarge | Self::Malformed | Self::Header | Self::Claims => "token_malformed",
            Self::Algorithm => "token_algorithm",
            Self::UnknownKey => "token_unknown_key",
            Self::Signature => "token_signature",
            Self::Lifetime => "token_lifetime",
            Self::NotYetValid => "token_not_yet_valid",
            Self::Expired => "token_expired",
            Self::Encode => "token_encode",
            Self::SessionExhausted => "session_exhausted",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TokenIssuer {
    kid: KeyId,
    key: TokenSigningKey,
}

impl TokenIssuer {
    pub fn new(kid: KeyId, key: TokenSigningKey) -> Self {
        Self { kid, key }
    }

    pub fn key_ring_entry(&self) -> KeyRingEntry {
        KeyRingEntry {
            kid: self.kid.clone(),
            key: self.key.verifying_key(),
        }
    }

    pub fn claims(
        &self,
        identity: &ConnectIdentity,
        aud: AccountName,
        now: UnixSeconds,
    ) -> Result<ConnectClaims, TokenError> {
        let exp = now
            .saturating_add(CONNECT_TOKEN_MAX_LIFETIME)
            .min(identity.session_expires_at);
        if exp <= now {
            return Err(TokenError::SessionExhausted);
        }
        Ok(ConnectClaims {
            sub: identity.sub.clone(),
            tenant: identity.tenant.clone(),
            aud,
            sid: identity.sid.clone(),
            cid: identity.cid,
            auth_realm: identity.auth_realm.clone(),
            auth_epoch: identity.auth_epoch,
            asv: identity.asv,
            iat: now,
            exp,
            jti: TokenId::random().map_err(|_| TokenError::Encode)?,
            kid: self.kid.clone(),
        })
    }

    pub fn mint(&self, claims: &ConnectClaims) -> Result<ConnectToken, TokenError> {
        let header = JwsHeader {
            alg: JWS_ALGORITHM.to_owned(),
            typ: JWS_TYPE.to_owned(),
            kid: self.kid.clone(),
        };
        let header = serde_json::to_vec(&header).map_err(|_| TokenError::Encode)?;
        let payload = serde_json::to_vec(claims).map_err(|_| TokenError::Encode)?;
        let input = format!("{}.{}", B64.encode(header), B64.encode(payload));
        let signature = self.key.0.sign(input.as_bytes());
        Ok(ConnectToken(format!("{input}.{}", B64.encode(signature.to_bytes()))))
    }
}

#[derive(Debug, Clone)]
pub struct TokenVerifier {
    keys: KeyRing,
}

impl TokenVerifier {
    pub fn new(keys: KeyRing) -> Self {
        Self { keys }
    }

    pub fn verify(&self, token: &str, now: UnixSeconds) -> Result<ConnectClaims, TokenError> {
        if token.is_empty() {
            return Err(TokenError::Missing);
        }
        if token.len() > CONNECT_TOKEN_MAX_BYTES {
            return Err(TokenError::TooLarge);
        }
        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        let header: JwsHeader = B64
            .decode(header)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(TokenError::Header)?;
        if header.alg != JWS_ALGORITHM || header.typ != JWS_TYPE {
            return Err(TokenError::Algorithm);
        }
        let key = self.keys.get(&header.kid).ok_or(TokenError::UnknownKey)?;
        let signature = B64
            .decode(signature)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
            .ok_or(TokenError::Signature)?;
        let signed_len = token.len() - token.rsplit('.').next().map_or(0, str::len) - 1;
        key.0
            .verify_strict(&token.as_bytes()[..signed_len], &signature)
            .map_err(|_| TokenError::Signature)?;
        let claims: ConnectClaims = B64
            .decode(payload)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(TokenError::Claims)?;
        if claims.kid != header.kid {
            return Err(TokenError::Claims);
        }
        validate_times(&claims, now)?;
        Ok(claims)
    }
}

fn validate_times(claims: &ConnectClaims, now: UnixSeconds) -> Result<(), TokenError> {
    if claims.exp < claims.iat || claims.exp > claims.iat.saturating_add(CONNECT_TOKEN_MAX_LIFETIME) {
        return Err(TokenError::Lifetime);
    }
    if claims.iat > now.saturating_add(CONNECT_TOKEN_LEEWAY) {
        return Err(TokenError::NotYetValid);
    }
    if now > claims.exp.saturating_add(CONNECT_TOKEN_LEEWAY) {
        return Err(TokenError::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::Subject;
    use crate::ids::{AuthEpoch, AuthVersion};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn issuer(kid: &str) -> Result<TokenIssuer, Box<dyn std::error::Error>> {
        Ok(TokenIssuer::new(kid.parse()?, TokenSigningKey::generate()?))
    }

    fn identity(session_expires_at: UnixSeconds) -> Result<ConnectIdentity, Box<dyn std::error::Error>> {
        Ok(ConnectIdentity {
            sub: Subject::try_from("ana@x.io".to_owned())?,
            tenant: "acme".parse()?,
            sid: "s1".parse()?,
            cid: trogon_presence::ConnectionId::generate()?,
            auth_realm: "realm".parse()?,
            auth_epoch: AuthEpoch::INITIAL,
            asv: AuthVersion::new(3),
            session_expires_at,
        })
    }

    fn claims(issuer: &TokenIssuer, now: UnixSeconds) -> Result<ConnectClaims, Box<dyn std::error::Error>> {
        let far = now.saturating_add(Duration::from_secs(3600));
        Ok(issuer.claims(&identity(far)?, AccountName::try_from("APP".to_owned())?, now)?)
    }

    #[test]
    fn connect_token_exp_never_outlives_the_session() -> TestResult {
        let a = issuer("a")?;
        let now = UnixSeconds::new(1_000_000);
        let soon = now.saturating_add(Duration::from_secs(10));
        let claims = a.claims(&identity(soon)?, "APP".parse()?, now)?;
        assert_eq!(claims.exp, soon);
        assert_eq!(
            a.claims(&identity(now)?, "APP".parse()?, now),
            Err(TokenError::SessionExhausted)
        );
        Ok(())
    }

    #[test]
    fn round_trips_and_selects_key_by_kid() -> TestResult {
        let a = issuer("a")?;
        let b = issuer("b")?;
        let verifier = TokenVerifier::new(KeyRing::new([a.key_ring_entry(), b.key_ring_entry()])?);
        let now = UnixSeconds::new(1_000_000);
        let c = claims(&b, now)?;
        assert_eq!(verifier.verify(b.mint(&c)?.as_str(), now)?, c);
        Ok(())
    }

    #[test]
    fn rejects_unknown_kid_and_bad_signature() -> TestResult {
        let a = issuer("a")?;
        let impostor = TokenIssuer::new("a".parse()?, TokenSigningKey::generate()?);
        let other = issuer("z")?;
        let verifier = TokenVerifier::new(KeyRing::new([a.key_ring_entry()])?);
        let now = UnixSeconds::new(1_000_000);
        let c = claims(&a, now)?;
        assert_eq!(
            verifier.verify(impostor.mint(&c)?.as_str(), now),
            Err(TokenError::Signature)
        );
        assert_eq!(
            verifier.verify(other.mint(&c)?.as_str(), now),
            Err(TokenError::UnknownKey)
        );
        assert_eq!(verifier.verify("a.b", now), Err(TokenError::Malformed));
        assert_eq!(verifier.verify("", now), Err(TokenError::Missing));
        Ok(())
    }

    #[test]
    fn enforces_lifetime_and_leeway() -> TestResult {
        let a = issuer("a")?;
        let verifier = TokenVerifier::new(KeyRing::new([a.key_ring_entry()])?);
        let iat = UnixSeconds::new(1_000_000);
        let mut c = claims(&a, iat)?;
        let at = |s: u64| UnixSeconds::new(1_000_000 + s);
        assert!(verifier.verify(a.mint(&c)?.as_str(), at(90)).is_ok());
        assert_eq!(verifier.verify(a.mint(&c)?.as_str(), at(91)), Err(TokenError::Expired));
        assert_eq!(
            verifier.verify(a.mint(&c)?.as_str(), UnixSeconds::new(999_969)),
            Err(TokenError::NotYetValid)
        );
        c.exp = iat.saturating_add(Duration::from_secs(61));
        assert_eq!(verifier.verify(a.mint(&c)?.as_str(), at(1)), Err(TokenError::Lifetime));
        Ok(())
    }

    #[test]
    fn parses_key_ring_entries() -> TestResult {
        let key = TokenSigningKey::generate()?;
        let entry: KeyRingEntry = format!("k1={}", key.verifying_key().to_base64()).parse()?;
        assert_eq!(entry.key, key.verifying_key());
        let restored: TokenSigningKey = key.to_base64().parse()?;
        assert_eq!(restored.verifying_key(), key.verifying_key());
        assert_eq!("nokid".parse::<KeyRingEntry>(), Err(KeyError::Entry));
        assert!(KeyRing::new([entry.clone(), entry]).is_err());
        Ok(())
    }
}

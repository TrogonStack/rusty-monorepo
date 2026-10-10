use std::fmt;
use std::str::FromStr;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use nkeys::{KeyPair, KeyPairType, XKey};
use serde::{Deserialize, Serialize};

use crate::claims::{AccountName, TokenId, UnixSeconds};
use crate::grants::Permissions;
use crate::ids::{BrokerClientId, ServerNkey, UserNkey};

const NATS_JWT_HEADER: &str = r#"{"typ":"JWT","alg":"ed25519-nkey"}"#;
const NATS_JWT_ALGORITHM: &str = "ed25519-nkey";
const CLAIMS_VERSION: u8 = 2;
const AUTHORIZATION_REQUEST_AUDIENCE: &str = "nats-authorization-request";
const AUTHORIZATION_REQUEST_TYPE: &str = "authorization_request";
const AUTHORIZATION_RESPONSE_TYPE: &str = "authorization_response";
const USER_TYPE: &str = "user";
const UNLIMITED: i64 = -1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NkeyError {
    #[error("issuer must be an account seed (SA...)")]
    Issuer,
    #[error("issuer account must be an account public key (A...)")]
    IssuerAccount,
    #[error("xkey must be a curve seed (SX...)")]
    XKey,
}

pub struct IssuerKey(KeyPair);

impl IssuerKey {
    pub fn public_key(&self) -> String {
        self.0.public_key()
    }

    pub fn generate() -> Self {
        Self(KeyPair::new_account())
    }

    pub fn seed(&self) -> Option<String> {
        self.0.seed().ok()
    }

    fn sign(&self, claims: &impl Serialize) -> Result<String, JwtError> {
        let payload = serde_json::to_vec(claims).map_err(|_| JwtError::Encode)?;
        let input = format!("{}.{}", B64.encode(NATS_JWT_HEADER), B64.encode(payload));
        let signature = self.0.sign(input.as_bytes()).map_err(|_| JwtError::Encode)?;
        Ok(format!("{input}.{}", B64.encode(signature)))
    }
}

impl FromStr for IssuerKey {
    type Err = NkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let pair = KeyPair::from_seed(s.trim()).map_err(|_| NkeyError::Issuer)?;
        if !matches!(pair.key_pair_type(), KeyPairType::Account) {
            return Err(NkeyError::Issuer);
        }
        Ok(Self(pair))
    }
}

impl fmt::Debug for IssuerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IssuerKey({})", self.0.public_key())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuerAccount(String);

impl FromStr for IssuerAccount {
    type Err = NkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let pair = KeyPair::from_public_key(s.trim()).map_err(|_| NkeyError::IssuerAccount)?;
        if !matches!(pair.key_pair_type(), KeyPairType::Account) {
            return Err(NkeyError::IssuerAccount);
        }
        Ok(Self(pair.public_key()))
    }
}

pub struct CalloutXKey(XKey);

impl CalloutXKey {
    pub fn generate() -> Self {
        Self(XKey::new())
    }

    pub fn public_key(&self) -> String {
        self.0.public_key()
    }

    pub fn seed(&self) -> Option<String> {
        self.0.seed().ok()
    }

    pub fn open(&self, payload: &[u8], server: &ServerXKey) -> Result<Vec<u8>, JwtError> {
        self.0.open(payload, &server.0).map_err(|_| JwtError::Decrypt)
    }

    pub fn seal(&self, payload: &[u8], server: &ServerXKey) -> Result<Vec<u8>, JwtError> {
        self.0.seal(payload, &server.0).map_err(|_| JwtError::Encrypt)
    }
}

impl FromStr for CalloutXKey {
    type Err = NkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        XKey::from_seed(s.trim()).map(Self).map_err(|_| NkeyError::XKey)
    }
}

impl fmt::Debug for CalloutXKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CalloutXKey({})", self.0.public_key())
    }
}

pub struct ServerXKey(XKey);

impl fmt::Debug for ServerXKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ServerXKey({})", self.0.public_key())
    }
}

impl FromStr for ServerXKey {
    type Err = NkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        XKey::from_public_key(s.trim()).map(Self).map_err(|_| NkeyError::XKey)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwtError {
    #[error("authorization request is not a JWT")]
    Malformed,
    #[error("authorization request header is not ed25519-nkey")]
    Algorithm,
    #[error("authorization request signature is invalid")]
    Signature,
    #[error("authorization request claims are invalid")]
    Claims,
    #[error("authorization request has the wrong audience or type")]
    NotARequest,
    #[error("authorization request could not be decrypted")]
    Decrypt,
    #[error("authorization response could not be encrypted")]
    Encrypt,
    #[error("JWT could not be encoded or signed")]
    Encode,
    #[error("user JWT would carry an empty publish or subscribe allow list")]
    EmptyPermissions,
}

#[derive(Debug, Deserialize)]
struct RawHeader {
    alg: String,
}

#[derive(Debug, Deserialize)]
struct RawRequest {
    aud: String,
    iss: String,
    nats: RawRequestNats,
}

#[derive(Debug, Deserialize)]
struct RawRequestNats {
    #[serde(rename = "type")]
    kind: String,
    server_id: RawServerId,
    user_nkey: UserNkey,
    #[serde(default)]
    client_info: RawClientInfo,
    #[serde(default)]
    connect_opts: RawConnectOpts,
}

#[derive(Debug, Deserialize)]
struct RawServerId {
    id: ServerNkey,
}

#[derive(Debug, Default, Deserialize)]
struct RawClientInfo {
    #[serde(default)]
    id: Option<BrokerClientId>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default, rename = "type")]
    transport: Option<String>,
}

/// How the client reached the server, as the server reports it in `client_info.kind` and
/// `client_info.type`. The server does not apply a callout user's `allowed_connection_types`, so
/// the callout checks this against its configured connection types itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTransport {
    Standard,
    Websocket,
    Mqtt,
    Leafnode,
    Unknown,
}

impl ClientTransport {
    fn of(info: &RawClientInfo) -> Self {
        match (info.kind.as_deref(), info.transport.as_deref()) {
            (Some("Client"), Some("nats")) => Self::Standard,
            (Some("Client"), Some("websocket")) => Self::Websocket,
            (Some("Client"), Some("mqtt")) => Self::Mqtt,
            (Some("Leafnode"), _) => Self::Leafnode,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawConnectOpts {
    #[serde(default)]
    auth_token: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizationRequest {
    pub server: ServerNkey,
    pub client: BrokerClientId,
    pub user_nkey: UserNkey,
    pub transport: ClientTransport,
    pub auth_token: Option<String>,
}

impl fmt::Debug for AuthorizationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizationRequest")
            .field("server", &self.server)
            .field("client", &self.client)
            .field("user_nkey", &self.user_nkey)
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl AuthorizationRequest {
    pub fn decode(jwt: &str) -> Result<Self, JwtError> {
        let mut parts = jwt.trim().split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(JwtError::Malformed);
        };
        let header: RawHeader = B64
            .decode(header)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(JwtError::Malformed)?;
        if header.alg != NATS_JWT_ALGORITHM {
            return Err(JwtError::Algorithm);
        }
        let raw: RawRequest = B64
            .decode(payload)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(JwtError::Claims)?;
        let server = KeyPair::from_public_key(&raw.iss).map_err(|_| JwtError::Signature)?;
        if !matches!(server.key_pair_type(), KeyPairType::Server) {
            return Err(JwtError::Signature);
        }
        let signature = B64.decode(signature).map_err(|_| JwtError::Signature)?;
        let signed = &jwt.trim().as_bytes()[..header_and_payload_len(jwt.trim())];
        server.verify(signed, &signature).map_err(|_| JwtError::Signature)?;
        if raw.aud != AUTHORIZATION_REQUEST_AUDIENCE || raw.nats.kind != AUTHORIZATION_REQUEST_TYPE {
            return Err(JwtError::NotARequest);
        }
        if raw.nats.server_id.id.as_str() != raw.iss {
            return Err(JwtError::Signature);
        }
        let client = raw.nats.client_info.id.ok_or(JwtError::Claims)?;
        let transport = ClientTransport::of(&raw.nats.client_info);
        Ok(Self {
            server: raw.nats.server_id.id,
            client,
            user_nkey: raw.nats.user_nkey,
            transport,
            auth_token: raw.nats.connect_opts.auth_token.filter(|token| !token.is_empty()),
        })
    }
}

fn header_and_payload_len(jwt: &str) -> usize {
    jwt.rfind('.').unwrap_or(0)
}

#[derive(Debug, Serialize)]
struct UserClaims<'a> {
    jti: &'a str,
    iat: UnixSeconds,
    exp: UnixSeconds,
    iss: String,
    name: &'a str,
    sub: &'a str,
    aud: &'a str,
    nats: UserNats<'a>,
}

#[derive(Debug, Serialize)]
struct UserNats<'a> {
    #[serde(flatten)]
    permissions: &'a Permissions,
    subs: i64,
    data: i64,
    payload: i64,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    tags: &'a [String],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    allowed_connection_types: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    issuer_account: Option<&'a str>,
    #[serde(rename = "type")]
    kind: &'static str,
    version: u8,
}

#[derive(Debug, Serialize)]
struct ResponseClaims<'a> {
    jti: &'a str,
    iat: UnixSeconds,
    iss: String,
    sub: &'a str,
    aud: &'a str,
    nats: ResponseNats<'a>,
}

#[derive(Debug, Serialize)]
struct ResponseNats<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    jwt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issuer_account: Option<&'a str>,
    #[serde(rename = "type")]
    kind: &'static str,
    version: u8,
}

pub struct UserJwt<'a> {
    pub name: &'a str,
    pub account: &'a AccountName,
    pub permissions: &'a Permissions,
    pub tags: &'a [String],
    pub allowed_connection_types: &'a [String],
    pub issued_at: UnixSeconds,
    pub expires_at: UnixSeconds,
}

pub struct ResponseSigner<'a> {
    pub issuer: &'a IssuerKey,
    pub issuer_account: Option<&'a IssuerAccount>,
}

impl ResponseSigner<'_> {
    pub fn user_jwt(&self, request: &AuthorizationRequest, user: &UserJwt<'_>) -> Result<String, JwtError> {
        if user.permissions.publish.allow.is_empty() || user.permissions.subscribe.allow.is_empty() {
            return Err(JwtError::EmptyPermissions);
        }
        let jti = TokenId::random().map_err(|_| JwtError::Encode)?;
        self.issuer.sign(&UserClaims {
            jti: jti.as_str(),
            iat: user.issued_at,
            exp: user.expires_at,
            iss: self.issuer.public_key(),
            name: user.name,
            sub: request.user_nkey.as_str(),
            aud: user.account.as_str(),
            nats: UserNats {
                permissions: user.permissions,
                subs: UNLIMITED,
                data: UNLIMITED,
                payload: UNLIMITED,
                tags: user.tags,
                allowed_connection_types: user.allowed_connection_types,
                issuer_account: self.issuer_account.map(|account| account.0.as_str()),
                kind: USER_TYPE,
                version: CLAIMS_VERSION,
            },
        })
    }

    pub fn response(
        &self,
        request: &AuthorizationRequest,
        outcome: Result<&str, &str>,
        now: UnixSeconds,
    ) -> Result<String, JwtError> {
        let jti = TokenId::random().map_err(|_| JwtError::Encode)?;
        let (jwt, error) = match outcome {
            Ok(jwt) => (Some(jwt), None),
            Err(error) => (None, Some(error)),
        };
        self.issuer.sign(&ResponseClaims {
            jti: jti.as_str(),
            iat: now,
            iss: self.issuer.public_key(),
            sub: request.user_nkey.as_str(),
            aud: request.server.as_str(),
            nats: ResponseNats {
                jwt,
                error,
                issuer_account: self.issuer_account.map(|account| account.0.as_str()),
                kind: AUTHORIZATION_RESPONSE_TYPE,
                version: CLAIMS_VERSION,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_jwt(server: &KeyPair, aud: &str) -> Result<String, Box<dyn std::error::Error>> {
        let claims = serde_json::json!({
            "aud": aud,
            "iss": server.public_key(),
            "sub": "x",
            "nats": {
                "type": "authorization_request",
                "server_id": {"id": server.public_key(), "name": "s"},
                "user_nkey": KeyPair::new_user().public_key(),
                "client_info": {"id": 7, "host": "127.0.0.1"},
                "connect_opts": {"auth_token": "tok"},
                "version": 2
            }
        });
        let input = format!(
            "{}.{}",
            B64.encode(NATS_JWT_HEADER),
            B64.encode(serde_json::to_vec(&claims)?)
        );
        let signature = server.sign(input.as_bytes())?;
        Ok(format!("{input}.{}", B64.encode(signature)))
    }

    #[test]
    fn decodes_and_verifies_requests() -> Result<(), Box<dyn std::error::Error>> {
        let server = KeyPair::new_server();
        let request = AuthorizationRequest::decode(&request_jwt(&server, AUTHORIZATION_REQUEST_AUDIENCE)?)?;
        assert_eq!(request.server.as_str(), server.public_key());
        assert_eq!(request.client, BrokerClientId::new(7));
        assert_eq!(request.auth_token.as_deref(), Some("tok"));
        assert_eq!(
            AuthorizationRequest::decode(&request_jwt(&server, "other")?),
            Err(JwtError::NotARequest)
        );
        let mut tampered = request_jwt(&server, AUTHORIZATION_REQUEST_AUDIENCE)?;
        tampered.insert(10, 'A');
        assert!(AuthorizationRequest::decode(&tampered).is_err());
        let account = KeyPair::new_account();
        assert!(AuthorizationRequest::decode(&request_jwt(&account, AUTHORIZATION_REQUEST_AUDIENCE)?).is_err());
        Ok(())
    }

    #[test]
    fn reads_the_client_transport() {
        let transport = |info: serde_json::Value| {
            serde_json::from_value::<RawClientInfo>(info).map(|info| ClientTransport::of(&info))
        };
        let cases = [
            (
                serde_json::json!({"kind": "Client", "type": "nats"}),
                ClientTransport::Standard,
            ),
            (
                serde_json::json!({"kind": "Client", "type": "websocket"}),
                ClientTransport::Websocket,
            ),
            (
                serde_json::json!({"kind": "Client", "type": "mqtt"}),
                ClientTransport::Mqtt,
            ),
            (
                serde_json::json!({"kind": "Leafnode", "type": ""}),
                ClientTransport::Leafnode,
            ),
            (
                serde_json::json!({"kind": "Client", "type": "carrier-pigeon"}),
                ClientTransport::Unknown,
            ),
            (serde_json::json!({"id": 7}), ClientTransport::Unknown),
        ];
        for (info, expected) in cases {
            assert_eq!(transport(info.clone()).ok(), Some(expected), "{info}");
        }
    }

    #[test]
    fn xkey_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let ours = CalloutXKey::generate();
        let server_pair = XKey::new();
        let server: ServerXKey = server_pair.public_key().parse()?;
        let sealed = ours.seal(b"hello", &server)?;
        let ours_public = XKey::from_public_key(&ours.public_key())?;
        assert_eq!(server_pair.open(&sealed, &ours_public)?, b"hello");
        Ok(())
    }

    #[test]
    fn refuses_to_sign_an_empty_allow_list() -> Result<(), Box<dyn std::error::Error>> {
        let server = KeyPair::new_server();
        let request = AuthorizationRequest::decode(&request_jwt(&server, AUTHORIZATION_REQUEST_AUDIENCE)?)?;
        let issuer = IssuerKey::generate();
        let signer = ResponseSigner {
            issuer: &issuer,
            issuer_account: None,
        };
        let account: AccountName = "APP".parse()?;
        let mut permissions = Permissions::default();
        permissions.subscribe.allow.push("a".to_owned());
        fn user<'a>(account: &'a AccountName, permissions: &'a Permissions) -> UserJwt<'a> {
            UserJwt {
                name: "ana",
                account,
                permissions,
                tags: &[],
                allowed_connection_types: &[],
                issued_at: UnixSeconds::new(1),
                expires_at: UnixSeconds::new(2),
            }
        }
        assert_eq!(
            signer.user_jwt(&request, &user(&account, &permissions)),
            Err(JwtError::EmptyPermissions)
        );
        permissions.publish.allow.push("b".to_owned());
        permissions.subscribe.allow.clear();
        assert_eq!(
            signer.user_jwt(&request, &user(&account, &permissions)),
            Err(JwtError::EmptyPermissions)
        );
        permissions.subscribe.allow.push("a".to_owned());
        assert!(signer.user_jwt(&request, &user(&account, &permissions)).is_ok());
        Ok(())
    }

    #[test]
    fn issuer_must_be_an_account_seed() {
        let user = KeyPair::new_user();
        assert!(user.seed().is_ok_and(|seed| seed.parse::<IssuerKey>().is_err()));
        let account = KeyPair::new_account();
        assert!(account.seed().is_ok_and(|seed| seed.parse::<IssuerKey>().is_ok()));
    }
}

use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{field, Instrument};

use crate::claims::{ConnectClaims, ConnectIdentity, UnixSeconds};
use crate::commands::{AdmissionKeys, AdmissionRead, AdmissionView, AdmitOutcome, MissingRow, ROW_GRACE};
use crate::connect_token::{KeyRing, TokenError, TokenVerifier};
use crate::grants::{GrantError, GrantPlan, GrantPolicy, GrantRequest};
use crate::ids::{AuthRealmId, TenantRegistry};
use crate::nats_jwt::{
    AuthorizationRequest, CalloutXKey, ClientTransport, IssuerAccount, IssuerKey, JwtError, ResponseSigner, ServerXKey,
    UserJwt,
};
use crate::rate_limit::{Admission, IdentityBound, RateLimit, RateLimiter};
use crate::rows::{AuthKey, GrantRow, RowStatus, AUTH_ROW_SCHEMA_V1};
use crate::store::{AuthStore, StoreError};
use crate::telemetry::{self, Outcome};

pub const AUTH_CALLOUT_SUBJECT: &str = "$SYS.REQ.USER.AUTH";
pub const CALLOUT_QUEUE_GROUP: &str = "trogon-presence-auth";
pub const SERVER_XKEY_HEADER: &str = "Nats-Server-Xkey";
pub const TAG_GRANTED: &str = "presence-granted";
pub const TAG_PUBLIC: &str = "presence-public";
pub const TAG_ASV: &str = "presence-asv";
pub const DEFAULT_USER_JWT_LIFETIME: Duration = Duration::from_secs(600);
pub const DEFAULT_USER_JWT_CEILING: Duration = Duration::from_secs(900);
pub const DEFAULT_CALLOUT_CONCURRENCY: usize = 64;
pub const DEFAULT_CALLOUT_DEADLINE: Duration = Duration::from_secs(1);
const MIN_USER_JWT_LIFETIME: Duration = Duration::from_secs(60);
const ADMISSION_ATTEMPTS: usize = 3;
const CONNECTION_TYPES: [&str; 6] = ["STANDARD", "WEBSOCKET", "LEAFNODE", "LEAFNODE_WS", "MQTT", "MQTT_WS"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserJwtLifetime {
    lifetime: Duration,
    ceiling: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("user JWT lifetime needs 60 s <= lifetime <= ceiling, in whole seconds")]
pub struct UserJwtLifetimeError;

impl UserJwtLifetime {
    pub fn new(lifetime: Duration, ceiling: Duration) -> Result<Self, UserJwtLifetimeError> {
        let whole = lifetime.subsec_nanos() == 0 && ceiling.subsec_nanos() == 0;
        if !whole || lifetime < MIN_USER_JWT_LIFETIME || lifetime > ceiling {
            return Err(UserJwtLifetimeError);
        }
        Ok(Self { lifetime, ceiling })
    }

    pub fn lifetime(self) -> Duration {
        self.lifetime
    }

    pub fn ceiling(self) -> Duration {
        self.ceiling
    }

    pub fn expires_at(self, now: UnixSeconds, session_expires_at: UnixSeconds) -> UnixSeconds {
        now.saturating_add(self.lifetime.min(self.ceiling))
            .min(session_expires_at)
    }
}

impl Default for UserJwtLifetime {
    fn default() -> Self {
        Self {
            lifetime: DEFAULT_USER_JWT_LIFETIME,
            ceiling: DEFAULT_USER_JWT_CEILING,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalloutConcurrency(usize);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("callout concurrency must be at least 1")]
pub struct CalloutConcurrencyError;

impl CalloutConcurrency {
    pub fn new(permits: usize) -> Result<Self, CalloutConcurrencyError> {
        if permits == 0 {
            return Err(CalloutConcurrencyError);
        }
        Ok(Self(permits))
    }

    pub fn get(self) -> usize {
        self.0
    }
}

impl Default for CalloutConcurrency {
    fn default() -> Self {
        Self(DEFAULT_CALLOUT_CONCURRENCY)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalloutDeadline(Duration);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("callout deadline must be positive")]
pub struct CalloutDeadlineError;

impl CalloutDeadline {
    pub fn new(deadline: Duration) -> Result<Self, CalloutDeadlineError> {
        if deadline.is_zero() {
            return Err(CalloutDeadlineError);
        }
        Ok(Self(deadline))
    }

    pub fn get(self) -> Duration {
        self.0
    }
}

impl Default for CalloutDeadline {
    fn default() -> Self {
        Self(DEFAULT_CALLOUT_DEADLINE)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionType(&'static str);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("connection type must be one of {CONNECTION_TYPES:?}")]
pub struct ConnectionTypeError;

impl ConnectionType {
    /// Whether a client on `transport` may connect under this type. The server reports MQTT and
    /// leafnode clients without saying whether they came over websocket, so either variant admits them.
    pub fn admits(&self, transport: ClientTransport) -> bool {
        matches!(
            (self.0, transport),
            ("STANDARD", ClientTransport::Standard)
                | ("WEBSOCKET", ClientTransport::Websocket)
                | ("MQTT" | "MQTT_WS", ClientTransport::Mqtt)
                | ("LEAFNODE" | "LEAFNODE_WS", ClientTransport::Leafnode)
        )
    }
}

impl FromStr for ConnectionType {
    type Err = ConnectionTypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let upper = s.trim().to_ascii_uppercase();
        CONNECTION_TYPES
            .iter()
            .find(|known| **known == upper)
            .map(|known| Self(known))
            .ok_or(ConnectionTypeError)
    }
}

#[derive(Debug)]
pub struct CalloutConfig {
    pub issuer: IssuerKey,
    pub issuer_account: Option<IssuerAccount>,
    pub xkey: CalloutXKey,
    pub keys: KeyRing,
    pub realm: AuthRealmId,
    pub tenants: TenantRegistry,
    pub policy: GrantPolicy,
    pub user_jwt_lifetime: UserJwtLifetime,
    pub rate_limit: RateLimit,
    pub identity_bound: IdentityBound,
    pub concurrency: CalloutConcurrency,
    pub deadline: CalloutDeadline,
    pub allowed_connection_types: Vec<ConnectionType>,
}

#[derive(Debug, thiserror::Error)]
pub enum Rejection {
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error("tenant is not served by this account")]
    Account,
    #[error("token belongs to another realm")]
    Realm,
    #[error("rate limited")]
    RateLimited,
    #[error("callout is at capacity")]
    Busy,
    #[error("callout deadline exceeded")]
    Deadline,
    #[error("callout requires an xkey-encrypted request")]
    XKeyRequired,
    #[error("connection type is not allowed")]
    ConnectionType,
    #[error("{0:?} row is not enrolled")]
    NotEnrolled(MissingRow),
    #[error("user is disabled")]
    UserDisabled,
    #[error("session is revoked")]
    SessionRevoked,
    #[error("session is expired")]
    SessionExpired,
    #[error("connect token outlives the session")]
    TokenOutlivesSession,
    #[error("auth epoch does not match")]
    EpochMismatch,
    #[error("auth set version does not match")]
    VersionMismatch,
    #[error("auth state kept changing")]
    Contended,
    #[error("broker client id already holds another grant")]
    GrantConflict,
    #[error(transparent)]
    Grant(#[from] GrantError),
    #[error("authorization response exceeds the response budget")]
    ResponseTooLarge,
    #[error("auth store unavailable")]
    Store(#[source] StoreError),
    #[error("could not sign the user JWT")]
    Sign(#[source] JwtError),
}

impl Rejection {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Token(err) => err.kind(),
            Self::Account => "account",
            Self::Realm => "realm",
            Self::RateLimited => "rate_limited",
            Self::Busy => "busy",
            Self::Deadline => "deadline",
            Self::XKeyRequired => "xkey_required",
            Self::ConnectionType => "connection_type",
            Self::NotEnrolled(_) => "not_enrolled",
            Self::UserDisabled => "user_disabled",
            Self::SessionRevoked => "session_revoked",
            Self::SessionExpired => "session_expired",
            Self::TokenOutlivesSession => "token_outlives_session",
            Self::EpochMismatch => "epoch_mismatch",
            Self::VersionMismatch => "version_mismatch",
            Self::Contended => "contended",
            Self::GrantConflict => "grant_conflict",
            Self::Grant(_) => "grant",
            Self::ResponseTooLarge => "response_too_large",
            Self::Store(_) => "store",
            Self::Sign(_) => "sign",
        }
    }

    fn outcome(&self) -> Outcome {
        match self {
            Self::RateLimited | Self::Busy => Outcome::RateLimited,
            _ => Outcome::Denied,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Authorized {
    pub claims: ConnectClaims,
    pub plan: GrantPlan,
    pub user_jwt: String,
    pub expires_at: UnixSeconds,
    pub response: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CalloutContext<'a> {
    pub server_xkey: &'a ServerXKey,
    pub max_payload: usize,
    pub now: UnixSeconds,
}

#[derive(Debug, thiserror::Error)]
pub enum CalloutError {
    #[error("subscribing to {AUTH_CALLOUT_SUBJECT}: {0}")]
    Subscribe(#[from] async_nats::SubscribeError),
}

#[derive(Debug)]
pub struct CalloutSlot {
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug, Clone)]
pub struct Callout {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    config: CalloutConfig,
    verifier: TokenVerifier,
    limiter: RateLimiter,
    store: AuthStore,
    slots: Arc<Semaphore>,
}

enum Decoded {
    Sealed(AuthorizationRequest, ServerXKey),
    Plain(AuthorizationRequest),
}

impl Callout {
    pub fn new(config: CalloutConfig, store: AuthStore) -> Self {
        let verifier = TokenVerifier::new(config.keys.clone());
        let limiter = RateLimiter::new(config.rate_limit, config.identity_bound);
        let slots = Arc::new(Semaphore::new(config.concurrency.get()));
        Self {
            inner: Arc::new(Inner {
                config,
                verifier,
                limiter,
                store,
                slots,
            }),
        }
    }

    pub fn reserve_slot(&self) -> Option<CalloutSlot> {
        self.inner
            .slots
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|permit| CalloutSlot { _permit: permit })
    }

    pub async fn serve(&self, client: async_nats::Client) -> Result<(), CalloutError> {
        let mut requests = client
            .queue_subscribe(AUTH_CALLOUT_SUBJECT, CALLOUT_QUEUE_GROUP.to_owned())
            .await?;
        while let Some(message) = requests.next().await {
            let this = self.clone();
            let client = client.clone();
            match self.reserve_slot() {
                Some(slot) => {
                    tokio::spawn(async move {
                        this.handle(&client, message, Some(slot)).await;
                    });
                }
                None => this.handle(&client, message, None).await,
            }
        }
        Ok(())
    }

    pub async fn authorize(
        &self,
        request: &AuthorizationRequest,
        context: &CalloutContext<'_>,
    ) -> Result<Authorized, Rejection> {
        let inner = &self.inner;
        let now = context.now;
        let allowed = &inner.config.allowed_connection_types;
        if !allowed.is_empty() && !allowed.iter().any(|kind| kind.admits(request.transport)) {
            return Err(Rejection::ConnectionType);
        }
        let token = request.auth_token.as_deref().ok_or(TokenError::Missing)?;
        let claims = inner.verifier.verify(token, now)?;
        if claims.auth_realm != inner.config.realm {
            return Err(Rejection::Realm);
        }
        if inner.config.tenants.account(&claims.tenant) != Some(&claims.aud) {
            return Err(Rejection::Account);
        }
        match inner.limiter.check(&claims.tenant, &claims.sub, Instant::now()) {
            Admission::Admitted => {}
            Admission::Limited => return Err(Rejection::RateLimited),
            Admission::Saturated => return Err(Rejection::Busy),
        }
        let identity = ConnectIdentity {
            sub: claims.sub.clone(),
            tenant: claims.tenant.clone(),
            sid: claims.sid.clone(),
            cid: claims.cid,
            auth_realm: claims.auth_realm.clone(),
            auth_epoch: claims.auth_epoch,
            asv: claims.asv,
            session_expires_at: claims.exp,
        };
        let keys = AdmissionKeys::of(&identity);
        let grant_key = AuthKey::grant(&request.server, request.client);
        for _ in 0..ADMISSION_ATTEMPTS {
            let view = match inner.store.read_admission(&keys).await.map_err(Rejection::Store)? {
                AdmissionRead::Consistent(view) => view,
                AdmissionRead::Changed => continue,
                AdmissionRead::Missing(row) => return Err(Rejection::NotEnrolled(row)),
            };
            self.validate(&claims, &view, now)?;
            let expires_at = inner.config.user_jwt_lifetime.expires_at(now, view.session.expires_at);
            if expires_at <= now {
                return Err(TokenError::SessionExhausted.into());
            }
            let plan = inner.config.policy.plan(&GrantRequest {
                sub: &claims.sub,
                connection: &claims.cid,
                topics: &view.connection.topics,
            })?;
            let user_jwt = self.user_jwt(request, &claims, &plan, now, expires_at)?;
            let response = self
                .encode(request, Ok(&user_jwt), Some(context.server_xkey), now)
                .map_err(Rejection::Sign)?;
            let budget = inner.config.policy.response_budget.effective(context.max_payload);
            if response.len() > budget {
                return Err(Rejection::ResponseTooLarge);
            }
            let grant = GrantRow {
                v: AUTH_ROW_SCHEMA_V1,
                tenant: claims.tenant.clone(),
                sub: claims.sub.clone(),
                account: claims.aud.clone(),
                sid: claims.sid.clone(),
                cid: claims.cid,
                epoch: claims.auth_epoch,
                version: claims.asv,
                expires_at,
            };
            let ttl = now.until(expires_at) + ROW_GRACE;
            match inner
                .store
                .admit(&view, &keys, &grant_key, &grant, ttl)
                .await
                .map_err(Rejection::Store)?
            {
                AdmitOutcome::Admitted => {
                    return Ok(Authorized {
                        claims,
                        plan,
                        user_jwt,
                        expires_at,
                        response,
                    })
                }
                AdmitOutcome::PolicyMoved => continue,
                AdmitOutcome::GrantConflict => return Err(Rejection::GrantConflict),
            }
        }
        Err(Rejection::Contended)
    }

    fn validate(&self, claims: &ConnectClaims, view: &AdmissionView, now: UnixSeconds) -> Result<(), Rejection> {
        if view.policy.status == RowStatus::Disabled {
            return Err(Rejection::UserDisabled);
        }
        if view.policy.realm != claims.auth_realm {
            return Err(Rejection::Realm);
        }
        if view.session.status == RowStatus::Disabled {
            return Err(Rejection::SessionRevoked);
        }
        if view.session.expires_at <= now {
            return Err(Rejection::SessionExpired);
        }
        if claims.exp > view.session.expires_at {
            return Err(Rejection::TokenOutlivesSession);
        }
        if view.policy.epoch != claims.auth_epoch || view.connection.epoch != claims.auth_epoch {
            return Err(Rejection::EpochMismatch);
        }
        if view.connection.version != claims.asv {
            return Err(Rejection::VersionMismatch);
        }
        Ok(())
    }

    fn user_jwt(
        &self,
        request: &AuthorizationRequest,
        claims: &ConnectClaims,
        plan: &GrantPlan,
        now: UnixSeconds,
        expires_at: UnixSeconds,
    ) -> Result<String, Rejection> {
        let tags = [
            format!("{TAG_GRANTED}:{}", plan.granted.len()),
            format!("{TAG_PUBLIC}:{}", plan.public.len()),
            format!("{TAG_ASV}:{}", claims.asv),
        ];
        let connection_types: Vec<String> = self
            .inner
            .config
            .allowed_connection_types
            .iter()
            .map(|kind| kind.0.to_owned())
            .collect();
        self.signer()
            .user_jwt(
                request,
                &UserJwt {
                    name: claims.sub.token(),
                    account: &claims.aud,
                    permissions: &plan.permissions,
                    tags: &tags,
                    allowed_connection_types: &connection_types,
                    issued_at: now,
                    expires_at,
                },
            )
            .map_err(Rejection::Sign)
    }

    fn signer(&self) -> ResponseSigner<'_> {
        ResponseSigner {
            issuer: &self.inner.config.issuer,
            issuer_account: self.inner.config.issuer_account.as_ref(),
        }
    }

    fn decode(&self, message: &async_nats::Message) -> Result<Decoded, JwtError> {
        let server_xkey = message
            .headers
            .as_ref()
            .and_then(|headers| headers.get(SERVER_XKEY_HEADER))
            .map(|value| value.as_str().parse::<ServerXKey>().map_err(|_| JwtError::Malformed))
            .transpose()?;
        let to_request = |bytes: Vec<u8>| {
            String::from_utf8(bytes)
                .map_err(|_| JwtError::Malformed)
                .and_then(|jwt| AuthorizationRequest::decode(&jwt))
        };
        match server_xkey {
            Some(theirs) => {
                let opened = self.inner.config.xkey.open(&message.payload, &theirs)?;
                Ok(Decoded::Sealed(to_request(opened)?, theirs))
            }
            None => Ok(Decoded::Plain(to_request(message.payload.to_vec())?)),
        }
    }

    async fn handle(&self, client: &async_nats::Client, message: async_nats::Message, slot: Option<CalloutSlot>) {
        let span = tracing::info_span!(
            telemetry::CALLOUT_SPAN,
            "presence.auth.outcome" = field::Empty,
            "error.type" = field::Empty,
            "presence.auth.granted_topics" = field::Empty,
            "presence.auth.public_topics" = field::Empty,
            "presence.auth.jwt.subjects" = field::Empty,
        );
        self.handle_in_span(client, message, slot, &span)
            .instrument(span.clone())
            .await;
    }

    async fn handle_in_span(
        &self,
        client: &async_nats::Client,
        message: async_nats::Message,
        slot: Option<CalloutSlot>,
        span: &tracing::Span,
    ) {
        let started = Instant::now();
        let Some(reply) = message.reply.clone() else {
            return;
        };
        let decoded = match self.decode(&message) {
            Ok(decoded) => decoded,
            Err(err) => {
                tracing::warn!(error = %err, "dropping undecodable authorization request");
                span.record("presence.auth.outcome", Outcome::Denied.as_str());
                span.record("error.type", "request");
                telemetry::record_decision(Outcome::Denied, Some("request"), started.elapsed());
                return;
            }
        };
        let now = UnixSeconds::now();
        let (request, server_xkey) = match decoded {
            Decoded::Plain(request) => (request, None),
            Decoded::Sealed(request, theirs) => (request, Some(theirs)),
        };
        let decision = match (&server_xkey, slot) {
            (None, _) => Err(Rejection::XKeyRequired),
            (Some(_), None) => Err(Rejection::Busy),
            (Some(theirs), Some(_slot)) => {
                let context = CalloutContext {
                    server_xkey: theirs,
                    max_payload: client.server_info().max_payload,
                    now,
                };
                tokio::time::timeout(self.inner.config.deadline.get(), self.authorize(&request, &context))
                    .await
                    .unwrap_or(Err(Rejection::Deadline))
            }
        };
        let response = match &decision {
            Ok(authorized) => Ok(authorized.response.clone()),
            Err(rejection) => self.encode(&request, Err(&rejection.to_string()), server_xkey.as_ref(), now),
        };
        let response = match response {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::error!(error = %err, "could not encode the authorization response");
                telemetry::record_decision(Outcome::Denied, Some("encode"), started.elapsed());
                return;
            }
        };
        if let Err(err) = client.publish(reply, response.into()).await {
            tracing::warn!(error = %err, "could not publish the authorization response");
        }
        let (outcome, error_type) = match &decision {
            Ok(authorized) => {
                let subjects = authorized.plan.permissions.subject_count();
                span.record("presence.auth.granted_topics", authorized.plan.granted.len());
                span.record("presence.auth.public_topics", authorized.plan.public.len());
                span.record("presence.auth.jwt.subjects", subjects);
                telemetry::record_subjects(subjects);
                (Outcome::Allowed, None)
            }
            Err(rejection) => {
                tracing::info!(reason = rejection.kind(), "connection rejected");
                (rejection.outcome(), Some(rejection.kind()))
            }
        };
        span.record("presence.auth.outcome", outcome.as_str());
        if let Some(error_type) = error_type {
            span.record("error.type", error_type);
        }
        telemetry::record_decision(outcome, error_type, started.elapsed());
    }

    fn encode(
        &self,
        request: &AuthorizationRequest,
        outcome: Result<&str, &str>,
        server_xkey: Option<&ServerXKey>,
        now: UnixSeconds,
    ) -> Result<Vec<u8>, JwtError> {
        let jwt = self.signer().response(request, outcome, now)?;
        match server_xkey {
            Some(theirs) => self.inner.config.xkey.seal(jwt.as_bytes(), theirs),
            None => Ok(jwt.into_bytes()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_types_admit_only_their_transport() -> Result<(), ConnectionTypeError> {
        let websocket: ConnectionType = "websocket".parse()?;
        assert!(websocket.admits(ClientTransport::Websocket));
        assert!(!websocket.admits(ClientTransport::Standard));
        assert!(!websocket.admits(ClientTransport::Unknown));
        let standard: ConnectionType = "STANDARD".parse()?;
        assert!(standard.admits(ClientTransport::Standard));
        assert!(!standard.admits(ClientTransport::Websocket));
        assert!("mqtt_ws".parse::<ConnectionType>()?.admits(ClientTransport::Mqtt));
        assert!("leafnode".parse::<ConnectionType>()?.admits(ClientTransport::Leafnode));
        Ok(())
    }

    #[test]
    fn user_jwt_lifetime_is_capped_by_session_and_ceiling() -> Result<(), UserJwtLifetimeError> {
        let lifetime = UserJwtLifetime::default();
        let now = UnixSeconds::new(1_000);
        assert_eq!(
            lifetime.expires_at(now, UnixSeconds::new(10_000)),
            UnixSeconds::new(1_600)
        );
        assert_eq!(
            lifetime.expires_at(now, UnixSeconds::new(1_100)),
            UnixSeconds::new(1_100)
        );
        let long = UserJwtLifetime::new(Duration::from_secs(900), Duration::from_secs(900))?;
        assert_eq!(long.expires_at(now, UnixSeconds::new(10_000)), UnixSeconds::new(1_900));
        assert!(UserJwtLifetime::new(Duration::from_secs(30), Duration::from_secs(900)).is_err());
        assert!(UserJwtLifetime::new(Duration::from_secs(901), Duration::from_secs(900)).is_err());
        Ok(())
    }

    #[test]
    fn connection_types_are_known_names() {
        assert_eq!("websocket".parse::<ConnectionType>(), Ok(ConnectionType("WEBSOCKET")));
        assert!("carrier-pigeon".parse::<ConnectionType>().is_err());
    }

    #[test]
    fn bounds_reject_zero() {
        assert!(CalloutConcurrency::new(0).is_err());
        assert!(CalloutDeadline::new(Duration::ZERO).is_err());
    }
}

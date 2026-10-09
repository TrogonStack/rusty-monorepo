//! OAuth 2.1 support for `trg mcp proxy`.
//!
//! Storage lives in [`store`]; the interactive browser/loopback dance lives in [`flow`].

pub mod flow;
pub mod store;
pub mod telemetry;

use http::header::AUTHORIZATION;
use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;
use rmcp::transport::auth::{AuthError, AuthorizationManager};
use secrecy::ExposeSecret;
use tracing::{field::Empty, Instrument};

use crate::{
    config::ResolvedMcpServer,
    oauth::{
        flow::{run_authorization, FlowConfig, FlowError},
        store::{OAuthCredentialStore, StorageFailure},
        telemetry::{auth_error_type, record_error, AuthOutcome, OAuthHttp, OAuthStep},
    },
    secrets::{Backend, SecretPath},
    telemetry::semconv::trg::{MCP_AUTH_OUTCOME, MCP_SERVER_NAME},
};

pub enum EnsureOutcome {
    NoAuthRequired,
    AlreadyAuthorized(AuthorizationManager),
    Authorized(AuthorizationManager),
}

impl EnsureOutcome {
    pub fn auth_outcome(&self) -> AuthOutcome {
        match self {
            Self::NoAuthRequired => AuthOutcome::None,
            Self::AlreadyAuthorized(_) => AuthOutcome::AlreadyAuthorized,
            Self::Authorized(_) => AuthOutcome::Authorized,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnsureError {
    /// Reported bare, and separately from [`EnsureError::Auth`], because the
    /// backend refusing a token is not an OAuth problem and naming it one sends
    /// the reader to re-authorize the wrong service.
    #[error("{0}")]
    Storage(String),

    #[error("OAuth: {0}")]
    Auth(#[from] AuthError),

    #[error("OAuth: {0}")]
    Flow(#[from] FlowError),

    #[error("OAuth completed but credentials are missing from the secrets backend, refusing to start")]
    MissingAfterFlow,

    /// The commands that only read or delete stored credentials never reach the
    /// endpoint, so no endpoint is resolved for them. Reaching this means a
    /// command that does reach one was wired as though it did not.
    #[error("no endpoint was resolved for `{0}`")]
    NoEndpoint(String),
}

impl EnsureError {
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Storage(_) => "storage_error",
            Self::Auth(error) => auth_error_type(error),
            Self::Flow(error) => error.error_type(),
            Self::MissingAfterFlow => "missing_after_flow",
            Self::NoEndpoint(_) => "no_endpoint",
        }
    }
}

/// Prefer what the credential store recorded over what rmcp made of it.
fn storage_or(failure: &StorageFailure, err: AuthError) -> EnsureError {
    match failure.take() {
        Some(message) => EnsureError::Storage(message),
        None => EnsureError::Auth(err),
    }
}

/// Return a ready-to-use `AuthorizationManager` (running the interactive flow
/// if needed) or signal that no OAuth is required.
///
/// The backend and the path it stores under are injected: this function neither
/// reads config nor decides where credentials live. See `main`.
///
/// `fallback` decides whether a credential still sitting at the
/// pre-`machine_id` shared path counts as already being authorized. An
/// explicit `trg mcp auth login` is the act of re-authorizing this machine,
/// so it must pass `None`: seeing only the machine-scoped path is what
/// forces the flow to run and actually write `cred_path`, rather than
/// reading the shared credential and reporting `AlreadyAuthorized` without
/// writing anything. Callers that merely need a working credential, such as
/// the proxy, pass the real fallback so a machine that has not logged in yet
/// keeps working off the shared path without a forced re-login.
///
/// The fallback is a read-time courtesy for a grant that already exists, and
/// it applies to exactly one read: the initial `initialize_from_store` above.
/// Once that has failed to find an already-authorized credential, every store
/// built past that point, for the authorization flow and for confirming it
/// persisted, is scoped to `cred_path` alone. The `AlreadyAuthorized` path is
/// untouched, so a machine still on the shared path keeps reading and
/// refreshing it in place.
pub async fn ensure_credentials_for(
    profile: &ResolvedMcpServer,
    server_name: &str,
    backend: &Backend,
    cred_path: &SecretPath,
    fallback: Option<&SecretPath>,
    http: &OAuthHttp,
) -> Result<EnsureOutcome, EnsureError> {
    let span = tracing::info_span!(
        "ensure credentials",
        "otel.status_code" = Empty,
        { MCP_SERVER_NAME } = server_name,
        { MCP_AUTH_OUTCOME } = Empty,
        { ERROR_TYPE } = Empty,
    );
    let result = ensure(profile, server_name, backend, cred_path, fallback, http)
        .instrument(span.clone())
        .await;
    match &result {
        Ok(outcome) => {
            span.record(MCP_AUTH_OUTCOME, outcome.auth_outcome().as_str());
        }
        Err(error) => record_error(&span, error.error_type()),
    }
    result
}

async fn ensure(
    profile: &ResolvedMcpServer,
    server_name: &str,
    backend: &Backend,
    cred_path: &SecretPath,
    fallback: Option<&SecretPath>,
    http: &OAuthHttp,
) -> Result<EnsureOutcome, EnsureError> {
    if profile.http_headers.contains_key(&AUTHORIZATION) {
        return Ok(EnsureOutcome::NoAuthRequired);
    }

    let url = profile.url.expose_secret();

    let mut manager = AuthorizationManager::new_with_oauth_http_client(url, http.client()).await?;
    let discovery = OAuthStep::DiscoverMetadata.span(server_name);
    let resolution = manager.resolve_metadata().instrument(discovery.clone()).await;
    if let Err(error) = &resolution {
        if !matches!(error, AuthError::NoAuthorizationSupport) {
            record_error(&discovery, auth_error_type(error));
        }
    }
    let resolution = match resolution {
        Ok(resolution) => resolution,
        Err(AuthError::NoAuthorizationSupport) => return Ok(EnsureOutcome::NoAuthRequired),
        Err(e) => return Err(e.into()),
    };

    // rmcp 3 synthesizes legacy `/authorize` and `/token` endpoints rather than
    // reporting that discovery found nothing, so a server with no OAuth at all
    // would otherwise be taken through the browser flow against URLs it never
    // published.
    if !resolution.source.is_discovered() {
        return Ok(EnsureOutcome::NoAuthRequired);
    }

    manager.set_metadata(resolution.metadata);
    let store = OAuthCredentialStore::new(backend.clone(), cred_path.clone(), server_name, fallback.cloned());
    let failure = store.failure();
    manager.set_credential_store(store);

    if manager
        .initialize_from_store()
        .await
        .map_err(|e| storage_or(&failure, e))?
    {
        return Ok(EnsureOutcome::AlreadyAuthorized(manager));
    }

    // Past this point we are minting a brand new grant, not reading an
    // existing one: swap in a store with no fallback so it can only ever be
    // written to `cred_path`. `fallback` is only `Some` when `machine_id` is
    // set, in which case `cred_path` is the machine-scoped path and a fresh
    // grant belongs there and nowhere else. Carrying the fallback past this
    // point would let a fresh grant land on the shared path the moment
    // `initialize_from_store` loads a credential and declines it (expired,
    // wrong scopes, or a future rmcp reporting that differently), which is
    // the exact replay `machine_id` exists to prevent, arrived at from the
    // other direction.
    manager.set_credential_store(OAuthCredentialStore::new(
        backend.clone(),
        cred_path.clone(),
        server_name,
        None,
    ));

    let _ = run_authorization(manager, server_name, &[], FlowConfig::default()).await?;

    // Same reasoning: this only has to confirm the flow persisted to
    // `cred_path`. A fallback-capable store could answer `MissingAfterFlow`
    // from the shared path even when the flow wrote nothing, and would hand
    // back a manager still pointed at the shared path for refreshes on a
    // machine that just logged in and should own its own credential.
    let mut manager = AuthorizationManager::new_with_oauth_http_client(url, http.client()).await?;
    let store = OAuthCredentialStore::new(backend.clone(), cred_path.clone(), server_name, None);
    let failure = store.failure();
    manager.set_credential_store(store);
    if !manager
        .initialize_from_store()
        .await
        .map_err(|e| storage_or(&failure, e))?
    {
        return Err(EnsureError::MissingAfterFlow);
    }
    Ok(EnsureOutcome::Authorized(manager))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use secrecy::SecretString;

    use super::*;
    use crate::secrets::fake::FakeBackend;
    use crate::telemetry::testing::capture;

    #[test]
    fn a_static_authorization_header_needs_no_oauth_and_says_so_on_the_span() {
        let profile = ResolvedMcpServer {
            secrets: None,
            url: SecretString::from("https://example.com/mcp".to_string()),
            transport: None,
            max_disconnected_time: None,
            initial_retry_interval: None,
            override_protocol_version: None,
            http_headers: HashMap::from([(AUTHORIZATION, SecretString::from("Bearer token".to_string()))]),
        };
        let backend = Backend::Fake(FakeBackend::new());
        let path = SecretPath::parse("mcp/weather").expect("path");
        let http = OAuthHttp::traced().expect("client");

        let (outcome, trace) = capture(|| {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime")
                .block_on(ensure_credentials_for(
                    &profile, "weather", &backend, &path, None, &http,
                ))
        });

        assert!(matches!(outcome, Ok(EnsureOutcome::NoAuthRequired)));
        assert_eq!(
            trace
                .attribute("ensure credentials", MCP_AUTH_OUTCOME)
                .map(|value| value.to_string())
                .as_deref(),
            Some("none")
        );
        assert_eq!(trace.spans.len(), 1);
    }
}

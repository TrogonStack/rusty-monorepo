//! OAuth 2.1 support for `trg mcp proxy`.
//!
//! Storage lives in [`store`]; the interactive browser/loopback dance lives in [`flow`].

pub mod flow;
pub mod store;

use http::header::AUTHORIZATION;
use rmcp::transport::auth::{AuthError, AuthorizationManager};
use secrecy::ExposeSecret;

use crate::{
    config::ResolvedMcpServer,
    oauth::{
        flow::{run_authorization, FlowConfig, FlowError},
        store::{OAuthCredentialStore, StorageFailure},
    },
    secrets::{Backend, SecretPath},
};

pub enum EnsureOutcome {
    NoAuthRequired,
    AlreadyAuthorized(AuthorizationManager),
    Authorized(AuthorizationManager),
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
pub async fn ensure_credentials_for(
    profile: &ResolvedMcpServer,
    server_name: &str,
    backend: &Backend,
    cred_path: &SecretPath,
) -> Result<EnsureOutcome, EnsureError> {
    if profile.http_headers.contains_key(&AUTHORIZATION) {
        return Ok(EnsureOutcome::NoAuthRequired);
    }

    let url = profile.url.expose_secret();

    let mut manager = AuthorizationManager::new(url).await?;
    let resolution = match manager.resolve_metadata().await {
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
    let store = OAuthCredentialStore::new(backend.clone(), cred_path.clone(), server_name);
    let failure = store.failure();
    manager.set_credential_store(store);

    if manager
        .initialize_from_store()
        .await
        .map_err(|e| storage_or(&failure, e))?
    {
        return Ok(EnsureOutcome::AlreadyAuthorized(manager));
    }

    let _ = run_authorization(manager, server_name, &[], FlowConfig::default()).await?;

    let mut manager = AuthorizationManager::new(url).await?;
    let store = OAuthCredentialStore::new(backend.clone(), cred_path.clone(), server_name);
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

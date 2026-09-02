//! OAuth 2.1 support for `trg mcp proxy`.
//!
//! Storage lives in [`store`]; the interactive browser/loopback dance lives in [`flow`].
//! See `crates/trg/PLAN.md` for the milestone scope (macOS Keychain only for now).

pub mod flow;
pub mod store;

use http::header::AUTHORIZATION;
use rmcp::transport::auth::{AuthError, AuthorizationManager};
use secrecy::ExposeSecret;

use crate::{
    config::{self, ResolvedMcpServer},
    oauth::{
        flow::{run_authorization, FlowConfig, FlowError},
        store::KeychainCredentialStore,
    },
};

pub enum EnsureOutcome {
    NoAuthRequired,
    AlreadyAuthorized(AuthorizationManager),
    Authorized(AuthorizationManager),
}

#[derive(Debug, thiserror::Error)]
pub enum EnsureError {
    #[error("{0}")]
    Config(#[from] config::ConfigError),

    #[error("OAuth: {0}")]
    Auth(#[from] AuthError),

    #[error("OAuth: {0}")]
    Flow(#[from] FlowError),

    #[error("OAuth completed but credentials are missing from the keychain — refusing to start")]
    MissingAfterFlow,
}

/// Resolve `server_name` from config and return a ready-to-use
/// `AuthorizationManager` (running the interactive flow if needed) or signal
/// that no OAuth is required.
pub async fn ensure_credentials(server_name: &str) -> Result<EnsureOutcome, EnsureError> {
    let resolved = config::load_mcp_server(server_name)?;
    ensure_credentials_for(&resolved, server_name).await
}

pub async fn ensure_credentials_for(
    profile: &ResolvedMcpServer,
    server_name: &str,
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
    manager.set_credential_store(KeychainCredentialStore::new(server_name));

    if manager.initialize_from_store().await? {
        return Ok(EnsureOutcome::AlreadyAuthorized(manager));
    }

    let _ = run_authorization(manager, server_name, &[], FlowConfig::default()).await?;

    let mut manager = AuthorizationManager::new(url).await?;
    manager.set_credential_store(KeychainCredentialStore::new(server_name));
    if !manager.initialize_from_store().await? {
        return Err(EnsureError::MissingAfterFlow);
    }
    Ok(EnsureOutcome::Authorized(manager))
}

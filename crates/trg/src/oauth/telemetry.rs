//! Spans for the OAuth steps `trg` drives and the HTTP requests rmcp makes
//! on their behalf.
//!
//! Nothing a request carried reaches a span: no header, no body, no query
//! string. Codes, tokens and client secrets travel in exactly those places.

mod constants;
use constants::TRANSPORT_ERROR;
use std::future::Future;
use std::sync::Arc;

use oauth2::{HttpRequest, HttpResponse};
use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, HTTP_REQUEST_METHOD, HTTP_RESPONSE_STATUS_CODE, SERVER_ADDRESS, SERVER_PORT, URL_FULL,
};
use rmcp::transport::auth::{
    default_oauth_http_client, AuthError, OAuthHttpClient, OAuthHttpClientFuture, OAuthHttpRequest,
};
use tracing::field::Empty;
use tracing::{Instrument, Span};

use crate::telemetry::semconv::trg::MCP_SERVER_NAME;

/// Marks `span` failed with a low-cardinality `error_type`.
pub(crate) fn record_error(span: &Span, error_type: &str) {
    span.record(ERROR_TYPE, error_type);
    span.record("otel.status_code", "ERROR");
}

/// Low-cardinality `error.type` for an rmcp OAuth failure.
pub(crate) fn auth_error_type(error: &AuthError) -> &'static str {
    match error {
        AuthError::AuthorizationRequired => "authorization_required",
        AuthError::AuthorizationFailed(_) => "authorization_failed",
        AuthError::TokenExchangeFailed(_) => "token_exchange_failed",
        AuthError::TokenRefreshFailed(_) => "token_refresh_failed",
        AuthError::TokenRefreshRejected(_) => "token_refresh_rejected",
        AuthError::CredentialStoreError(_) => "credential_store_error",
        AuthError::HttpError(_) => "http_error",
        AuthError::OAuthError(_) => "oauth_error",
        AuthError::MetadataError(_) => "metadata_error",
        AuthError::PkceUnsupported => "pkce_unsupported",
        AuthError::UrlError(_) => "url_error",
        AuthError::NoAuthorizationSupport => "no_authorization_support",
        AuthError::InternalError(_) => "internal_error",
        AuthError::InvalidTokenType(_) => "invalid_token_type",
        AuthError::TokenExpired => "token_expired",
        AuthError::InvalidScope(_) => "invalid_scope",
        AuthError::RegistrationFailed(_) => "registration_failed",
        AuthError::InsufficientScope { .. } => "insufficient_scope",
        AuthError::AuthorizationServerMismatch { .. } => "authorization_server_mismatch",
        AuthError::AuthorizationServerMissingIssuer { .. } => "authorization_server_missing_issuer",
        AuthError::ClientCredentialsError(_) => "client_credentials_error",
        _ => "auth_error",
    }
}

/// How a session came by the credentials it runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthOutcome {
    None,
    AlreadyAuthorized,
    Authorized,
}

impl AuthOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::AlreadyAuthorized => "already_authorized",
            Self::Authorized => "authorized",
        }
    }
}

/// The client every OAuth HTTP request rmcp makes goes through, one HTTP
/// client span per request.
#[derive(Clone)]
pub struct OAuthHttp(Arc<dyn OAuthHttpClient>);

impl OAuthHttp {
    /// rmcp's default client, traced.
    pub fn traced() -> Result<Self, AuthError> {
        Ok(Self(Arc::new(TracedOAuthHttpClient {
            inner: Box::new(default_oauth_http_client()?),
        })))
    }

    pub(crate) fn client(&self) -> Arc<dyn OAuthHttpClient> {
        self.0.clone()
    }
}

struct TracedOAuthHttpClient {
    inner: Box<dyn OAuthHttpClient>,
}

impl OAuthHttpClient for TracedOAuthHttpClient {
    fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
        let span = http_span(&request.request);
        let response = self.inner.execute(request);
        Box::pin(
            async move {
                let result = response.await;
                record_response(&Span::current(), &result);
                result
            }
            .instrument(span),
        )
    }
}

fn record_response<E>(span: &Span, result: &Result<HttpResponse, E>) {
    match result {
        Ok(response) => {
            let status = response.status();
            span.record(HTTP_RESPONSE_STATUS_CODE, i64::from(status.as_u16()));
            if status.is_client_error() || status.is_server_error() {
                record_error(span, status.as_str());
            }
        }
        Err(_) => record_error(span, TRANSPORT_ERROR),
    }
}

fn http_span(request: &HttpRequest) -> Span {
    let method = request.method().as_str();
    let uri = request.uri();
    let scheme = uri.scheme_str().unwrap_or("https");
    let host = uri.host().unwrap_or_default();
    let port = uri.port_u16().or(match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    });
    let url = match uri.port_u16() {
        Some(port) => format!("{scheme}://{host}:{port}{}", uri.path()),
        None => format!("{scheme}://{host}{}", uri.path()),
    };
    tracing::info_span!(
        "oauth http",
        "otel.name" = method,
        "otel.kind" = "client",
        "otel.status_code" = Empty,
        { HTTP_REQUEST_METHOD } = method,
        { SERVER_ADDRESS } = host,
        { SERVER_PORT } = port.map(i64::from),
        { URL_FULL } = url,
        { HTTP_RESPONSE_STATUS_CODE } = Empty,
        { ERROR_TYPE } = Empty,
    )
}

/// One access to the secrets backend holding a server's OAuth credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialAccess {
    Load,
    Save,
    Clear,
    CheckShared,
}

impl CredentialAccess {
    fn span_name(self) -> &'static str {
        match self {
            Self::Load => "load credentials",
            Self::Save => "save credentials",
            Self::Clear => "clear credentials",
            Self::CheckShared => "check shared_credentials",
        }
    }

    pub(crate) async fn traced<T>(
        self,
        server: &str,
        work: impl Future<Output = Result<T, AuthError>>,
    ) -> Result<T, AuthError> {
        let span = tracing::info_span!(
            "oauth credentials",
            "otel.name" = self.span_name(),
            "otel.status_code" = Empty,
            { MCP_SERVER_NAME } = server,
            { ERROR_TYPE } = Empty,
        );
        let result = work.instrument(span.clone()).await;
        if let Err(error) = &result {
            record_error(&span, auth_error_type(error));
        }
        result
    }
}

/// An OAuth step that is neither an HTTP request nor a store access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OAuthStep {
    DiscoverMetadata,
    RefreshAccessToken,
    Authorize,
    RegisterClient,
    ExchangeCode,
}

impl OAuthStep {
    fn span_name(self) -> &'static str {
        match self {
            Self::DiscoverMetadata => "discover oauth_metadata",
            Self::RefreshAccessToken => "refresh access_token",
            Self::Authorize => "authorize mcp_server",
            Self::RegisterClient => "register oauth_client",
            Self::ExchangeCode => "exchange oauth_code",
        }
    }

    pub(crate) fn span(self, server: &str) -> Span {
        tracing::info_span!(
            "oauth step",
            "otel.name" = self.span_name(),
            "otel.status_code" = Empty,
            { MCP_SERVER_NAME } = server,
            { ERROR_TYPE } = Empty,
        )
    }
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;

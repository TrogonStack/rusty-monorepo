use super::UrlTemplate;

/// Total request budget when the backend does not override it.
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;

/// Connect budget, capped by the total so a tight `timeout_ms` stays honest.
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 2_000;

/// Matches reqwest's own default. Named here only because following redirects
/// at all is a deliberate choice once a token rides along.
pub const MAX_REDIRECTS: usize = 10;

pub const HEALTH_ROUTE: UrlTemplate = UrlTemplate::new("/v1/sys/health");

pub const DATA_ROUTE: UrlTemplate = UrlTemplate::new("/v1/{mount}/data/{path}");

pub const METADATA_ROUTE: UrlTemplate = UrlTemplate::new("/v1/{mount}/metadata/{path}");

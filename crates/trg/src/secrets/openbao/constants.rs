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

pub const TRANSIT_KEY_ROUTE: UrlTemplate = UrlTemplate::new("/v1/{mount}/keys/{name}");

pub const TRANSIT_SIGN_ROUTE: UrlTemplate = UrlTemplate::new("/v1/{mount}/sign/{name}");

pub const TRANSIT_WRAPPING_KEY_ROUTE: UrlTemplate = UrlTemplate::new("/v1/{mount}/wrapping_key");

pub const TRANSIT_IMPORT_ROUTE: UrlTemplate = UrlTemplate::new("/v1/{mount}/keys/{name}/import");

pub const TRANSIT_IMPORT_VERSION_ROUTE: UrlTemplate = UrlTemplate::new("/v1/{mount}/keys/{name}/import_version");

/// The AES key that wraps an imported key is AES-256.
pub const EPHEMERAL_KEY_LEN: usize = 32;

/// AES-KWP pads its input to whole 8-byte blocks and adds one block.
pub const KWP_BLOCK: usize = 8;

pub const PEM_FENCE: &str = "-----";

/// The hash both OAEP and its MGF1 use, which is what Transit assumes.
pub const OAEP_HASH_FUNCTION: &str = "SHA256";

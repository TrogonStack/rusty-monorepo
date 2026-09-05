//! The KV v2 wire shapes, defined once and shared by the client and its stubs.
//!
//! A test that hand-spells JSON can assert a shape the server never sends, and
//! one did: a soft-deleted version was mocked as `200` for a while when
//! OpenBao answers `404`. Naming the shapes here means a fixture has to be
//! built out of the same types the client parses, so the two cannot drift
//! independently.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The envelope every KV v2 endpoint wraps its payload in, and the same shape
/// a write sends up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub data: T,
}

/// The payload of `GET <mount>/data/<path>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadPayload {
    /// `None` once the version is soft-deleted, which the server reports with
    /// a `404` rather than a `200`.
    pub data: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<VersionMetadata>,
}

/// The payload of `POST <mount>/data/<path>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WritePayload {
    pub version: u64,
    pub created_time: String,
    pub deletion_time: String,
    pub destroyed: bool,
    pub custom_metadata: Option<Value>,
}

/// The payload of `LIST <mount>/metadata/<path>`. A key naming a folder rather
/// than a secret ends in `/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPayload {
    pub keys: Vec<String>,
}

/// What a read reports about the version it returned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionMetadata {
    pub version: u64,
    pub created_time: String,
    /// Empty until the version is soft-deleted.
    pub deletion_time: String,
    pub destroyed: bool,
    pub custom_metadata: Option<Value>,
}

/// The body behind any refusal.
///
/// Present but empty on the `404` for a path that was never written, which is
/// the only thing separating a miss from a misconfigured mount.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorBody {
    #[serde(default)]
    pub errors: Vec<String>,
}

impl ErrorBody {
    /// Pull the `errors` array out of a body, treating anything unparseable as
    /// carrying none. Only this array is ever surfaced: the rest of a body may
    /// hold secret material.
    pub fn of(body: &str) -> Vec<String> {
        serde_json::from_str::<Self>(body).unwrap_or_default().errors
    }
}

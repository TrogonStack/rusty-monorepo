//! Resource attributes attached to every exported signal.

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use sha2::{Digest, Sha256};

use super::env::EnvLookup;
use super::identity::CommandIdentity;
use crate::telemetry::semconv::trg;

/// Builds the process-wide [`Resource`].
///
/// `service.name` is only set here when neither `OTEL_SERVICE_NAME` nor a
/// `service.name` key in `OTEL_RESOURCE_ATTRIBUTES` is present:
/// `Resource::builder()` already merges both of those automatically, but an
/// attribute set in code always wins over the env-derived one for the same
/// key, so setting it unconditionally would make either env var impossible
/// to override.
pub fn build(command: &CommandIdentity, env: &impl EnvLookup) -> Resource {
    let mut builder = Resource::builder();
    if !has_service_name_override(env) {
        builder = builder.with_attribute(KeyValue::new("service.name", "trg"));
    }
    builder
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .with_attribute(KeyValue::new("service.instance.id", instance_id()))
        .with_attribute(KeyValue::new(trg::COMMAND, command.as_str().to_string()))
        .build()
}

fn has_service_name_override(env: &impl EnvLookup) -> bool {
    env.get("OTEL_SERVICE_NAME").is_some()
        || env
            .get("OTEL_RESOURCE_ATTRIBUTES")
            .is_some_and(|attrs| resource_attributes_has_key(&attrs, "service.name"))
}

fn resource_attributes_has_key(attrs: &str, key: &str) -> bool {
    attrs
        .split(',')
        .any(|pair| pair.split_once('=').is_some_and(|(name, _)| name.trim() == key))
}

/// A per-process identifier. Not cryptographically random: it only needs to
/// distinguish concurrent `trg` processes reporting the same `service.name`
/// from the same host, which a hash of the PID, the current time and a
/// stack address already does.
fn instance_id() -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_ne_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    hasher.update(now.as_nanos().to_ne_bytes());
    let stack_marker = 0_u8;
    hasher.update((std::ptr::addr_of!(stack_marker) as usize).to_ne_bytes());
    hasher.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "resource_tests.rs"]
mod tests;

//! Resource attributes attached to every exported signal.

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use sha2::{Digest, Sha256};

use super::identity::CommandIdentity;
use crate::telemetry::semconv::trg;

/// Builds the process-wide [`Resource`].
///
/// `service.name` is only set here when `OTEL_SERVICE_NAME` is absent:
/// `Resource::builder()` already merges `OTEL_RESOURCE_ATTRIBUTES` and
/// `OTEL_SERVICE_NAME` automatically, but an attribute set in code always
/// wins over the env-derived one for the same key, so setting it
/// unconditionally would make the env var impossible to override.
pub fn build(command: &CommandIdentity) -> Resource {
    let mut builder = Resource::builder();
    if std::env::var_os("OTEL_SERVICE_NAME").is_none() {
        builder = builder.with_attribute(KeyValue::new("service.name", "trg"));
    }
    builder
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .with_attribute(KeyValue::new("service.instance.id", instance_id()))
        .with_attribute(KeyValue::new(trg::COMMAND, command.as_str().to_string()))
        .build()
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
mod tests {
    use super::*;

    #[test]
    fn sets_default_service_name_without_env_override() {
        // SAFETY: test-only, single-threaded assumption for this check; no
        // other test in this process reads OTEL_SERVICE_NAME concurrently.
        unsafe {
            std::env::remove_var("OTEL_SERVICE_NAME");
        }
        let resource = build(&CommandIdentity::new("mcp proxy"));
        let value = resource.get(&opentelemetry::Key::from_static_str("service.name"));
        assert_eq!(value.map(|v| v.to_string()), Some("trg".to_string()));
    }

    #[test]
    fn carries_command_identity() {
        let resource = build(&CommandIdentity::new("ai skills eval run"));
        let value = resource.get(&opentelemetry::Key::from_static_str(trg::COMMAND));
        assert_eq!(value.map(|v| v.to_string()), Some("ai skills eval run".to_string()));
    }
}

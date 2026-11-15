use super::*;
use crate::telemetry::env::fixed;

#[test]
fn sets_default_service_name_without_env_override() {
    let resource = build(&CommandIdentity::new("mcp proxy"), &fixed(&[]));
    let value = resource.get(&opentelemetry::Key::from_static_str("service.name"));
    assert_eq!(value.map(|v| v.to_string()), Some("trg".to_string()));
}

#[test]
fn carries_command_identity() {
    let resource = build(&CommandIdentity::new("ai skills eval run"), &fixed(&[]));
    let value = resource.get(&opentelemetry::Key::from_static_str(trg::COMMAND));
    assert_eq!(value.map(|v| v.to_string()), Some("ai skills eval run".to_string()));
}

#[test]
fn skips_default_service_name_when_resource_attributes_override_it() {
    let resource = build(
        &CommandIdentity::new("mcp proxy"),
        &fixed(&[(
            "OTEL_RESOURCE_ATTRIBUTES",
            "deployment.environment=prod,service.name=custom-service",
        )]),
    );
    let value = resource.get(&opentelemetry::Key::from_static_str("service.name"));
    assert_ne!(value.map(|v| v.to_string()), Some("trg".to_string()));
}

#[test]
fn detects_service_name_override_from_otel_service_name() {
    assert!(has_service_name_override(&fixed(&[(
        "OTEL_SERVICE_NAME",
        "custom-service"
    )])));
}

#[test]
fn detects_service_name_override_from_resource_attributes() {
    assert!(has_service_name_override(&fixed(&[(
        "OTEL_RESOURCE_ATTRIBUTES",
        "deployment.environment=prod,service.name=custom-service,foo=bar"
    )])));
}

#[test]
fn no_override_when_neither_env_var_mentions_service_name() {
    assert!(!has_service_name_override(&fixed(&[(
        "OTEL_RESOURCE_ATTRIBUTES",
        "deployment.environment=prod"
    )])));
}

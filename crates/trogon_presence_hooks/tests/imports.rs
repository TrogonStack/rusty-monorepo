#[path = "support/guest.rs"]
mod guest;
#[path = "support/wat.rs"]
mod wat;

use std::error::Error;
use std::path::PathBuf;

use serde_json::json;
use trogon_presence::{Meta, PresenceKey, Topic};
use trogon_presence_hooks::{AllowedHost, HookConfig, HookLoadError, HookOp, HookOutcome, HookRuntime};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const HTTP_TYPES: &str = "wasi:http/types@0.2.12";
const HTTP_OUTGOING: &str = "wasi:http/outgoing-handler@0.2.12";

fn scratch(test: &str) -> PathBuf {
    std::env::temp_dir().join(format!("trogon-hook-imports-{}-{test}", std::process::id()))
}

fn http_on(path: PathBuf) -> Result<HookConfig, Box<dyn Error + Send + Sync>> {
    Ok(HookConfig {
        http_allow: vec!["api.example.com".parse::<AllowedHost>()?],
        ..HookConfig::new(path)
    })
}

fn rejected_import(loaded: Result<HookRuntime, HookLoadError>) -> Option<String> {
    match loaded {
        Err(HookLoadError::Import(unsupported)) => Some(unsupported.name().as_str().to_owned()),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn baseline_component_loads_and_runs() -> TestResult {
    let dir = scratch("baseline");
    let runtime = HookRuntime::load(HookConfig::new(wat::write(&dir, "baseline", &[])?))?;
    let topic: Topic = "room:lobby".parse()?;
    let key: PresenceKey = "ana".parse()?;
    let meta: Meta = serde_json::from_value(json!({ "status": "online" }))?;
    let expected: Meta = serde_json::from_str(wat::OUTPUT)?;
    assert_eq!(
        runtime.enrich(HookOp::Track, &topic, &key, &meta).await,
        HookOutcome::Enriched(expected)
    );
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn unknown_imports_are_rejected() -> TestResult {
    let dir = scratch("unknown");
    for name in [
        "wasi:random/random@0.2.12",
        "wasi:clocks/wall-clock@0.2.12",
        "acme:spy/exfiltrate@1.0.0",
    ] {
        let path = wat::write(&dir, "unknown", &[name])?;
        assert_eq!(
            rejected_import(HookRuntime::load(HookConfig::new(&path))).as_deref(),
            Some(name)
        );
        assert_eq!(
            rejected_import(HookRuntime::load(http_on(path)?)).as_deref(),
            Some(name)
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn future_versions_are_rejected() -> TestResult {
    let dir = scratch("future");
    for name in [
        "wasi:io/poll@0.2.13",
        "trogon:presence/types@0.1.1",
        "wasi:cli/stdout@0.3.0",
    ] {
        let path = wat::write(&dir, "future", &[name])?;
        assert_eq!(
            rejected_import(HookRuntime::load(HookConfig::new(path))).as_deref(),
            Some(name)
        );
    }
    let http = wat::write(&dir, "future-http", &["wasi:http/types@0.2.13"])?;
    assert_eq!(
        rejected_import(HookRuntime::load(http_on(http)?)).as_deref(),
        Some("wasi:http/types@0.2.13")
    );
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn http_off_rejects_wasi_http_imports() -> TestResult {
    let dir = scratch("http-off");
    for name in [HTTP_TYPES, HTTP_OUTGOING] {
        let path = wat::write(&dir, "http-off", &[name])?;
        assert_eq!(
            rejected_import(HookRuntime::load(HookConfig::new(path))).as_deref(),
            Some(name)
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn http_on_allows_exactly_types_and_outgoing_handler() -> TestResult {
    let dir = scratch("http-on");
    let both = wat::write(&dir, "both", &[HTTP_TYPES, HTTP_OUTGOING])?;
    let runtime = HookRuntime::load(http_on(both)?)?;
    assert!(runtime.digest().as_str().starts_with("sha256:"));
    for name in [
        "wasi:http/incoming-handler@0.2.12",
        "wasi:sockets/tcp@0.2.12",
        "wasi:filesystem/preopens@0.2.12",
    ] {
        let path = wat::write(&dir, "extra", &[HTTP_TYPES, HTTP_OUTGOING, name])?;
        assert_eq!(
            rejected_import(HookRuntime::load(http_on(path)?)).as_deref(),
            Some(name)
        );
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn socket_importing_guest_is_rejected_at_load() -> TestResult {
    let Some(path) = guest::socket_probe_component() else {
        return Ok(());
    };
    let rejected = rejected_import(HookRuntime::load(HookConfig::new(&path))).ok_or("expected an import rejection")?;
    assert!(
        rejected.starts_with("wasi:filesystem/")
            || rejected.starts_with("wasi:sockets/")
            || rejected.starts_with("wasi:clocks/wall-clock"),
        "{rejected}"
    );
    assert!(rejected_import(HookRuntime::load(http_on(path)?)).is_some());
    Ok(())
}

#[test]
fn reference_guest_fits_the_baseline() -> TestResult {
    let Some(path) = guest::example_component() else {
        return Ok(());
    };
    HookRuntime::load(HookConfig::new(path))?;
    Ok(())
}

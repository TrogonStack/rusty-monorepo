#[path = "support/guest.rs"]
mod guest;
#[path = "support/wat.rs"]
mod wat;

use std::error::Error;
use std::time::{Duration, Instant};

use serde_json::json;
use trogon_presence::{Meta, PresenceKey, Topic};
use trogon_presence_hooks::{
    HookConfig, HookDeadline, HookFailure, HookInvocation, HookLoadError, HookMemoryLimit, HookOp, HookOutcome,
    HookPolicy, HookRuntime, OutputLimit,
};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

const DEADLINE: Duration = Duration::from_millis(200);
const DEADLINE_SLACK: Duration = Duration::from_millis(100);

macro_rules! component_or_skip {
    () => {
        match guest::example_component() {
            Some(path) => path,
            None => return Ok(()),
        }
    };
}

fn config(path: std::path::PathBuf, policy: HookPolicy) -> Result<HookConfig, Box<dyn Error + Send + Sync>> {
    Ok(HookConfig {
        policy,
        deadline: HookDeadline::try_from(DEADLINE)?,
        memory_limit: "16MiB".parse::<HookMemoryLimit>()?,
        ..HookConfig::new(path)
    })
}

fn meta() -> Result<Meta, serde_json::Error> {
    serde_json::from_value(json!({ "status": "online" }))
}

async fn enrich(runtime: &HookRuntime, key: &str) -> Result<HookOutcome, Box<dyn Error + Send + Sync>> {
    Ok(invoke(runtime, key).await?.into_outcome())
}

async fn invoke(runtime: &HookRuntime, key: &str) -> Result<HookInvocation, Box<dyn Error + Send + Sync>> {
    let topic: Topic = "room:lobby".parse()?;
    let key: PresenceKey = key.parse()?;
    Ok(runtime.invoke(HookOp::Track, &topic, &key, &meta()?).await)
}

fn spawn_invoke(runtime: &HookRuntime, key: &'static str) -> tokio::task::JoinHandle<Result<HookInvocation, String>> {
    let runtime = runtime.clone();
    tokio::spawn(async move { invoke(&runtime, key).await.map_err(|err| err.to_string()) })
}

#[tokio::test(flavor = "multi_thread")]
async fn enrich_adds_the_field() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailClosed)?)?;
    assert!(runtime.digest().as_str().starts_with("sha256:"));
    assert_eq!(runtime.digest().as_str().len(), "sha256:".len() + 64);
    let expected: Meta = serde_json::from_value(json!({ "status": "online", "enriched": true }))?;
    assert_eq!(enrich(&runtime, "ana").await?, HookOutcome::Enriched(expected));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_key_is_rejected_even_under_fail_open() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailOpen)?)?;
    match enrich(&runtime, "blocked").await? {
        HookOutcome::Rejected(reason) => assert!(reason.contains("blocked"), "{reason}"),
        other => panic!("expected a rejection, got {other:?}"),
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_guest_hits_the_deadline() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailClosed)?)?;
    let started = Instant::now();
    let outcome = enrich(&runtime, "slow").await?;
    let elapsed = started.elapsed();
    assert_eq!(outcome, HookOutcome::Unavailable(HookFailure::Deadline));
    assert!(elapsed < DEADLINE + DEADLINE_SLACK, "took {elapsed:?}");
    assert!(matches!(
        enrich(&runtime, "after-slow").await?,
        HookOutcome::Enriched(_)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_cap_is_enforced() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailClosed)?)?;
    assert_eq!(
        enrich(&runtime, "hog").await?,
        HookOutcome::Unavailable(HookFailure::MemoryLimit)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_open_admits_the_original_meta() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailOpen)?)?;
    for key in ["slow", "broken", "garbage"] {
        assert_eq!(enrich(&runtime, key).await?, HookOutcome::Enriched(meta()?), "{key}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_closed_reports_guest_errors_and_malformed_output() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailClosed)?)?;
    assert!(matches!(
        enrich(&runtime, "broken").await?,
        HookOutcome::Unavailable(HookFailure::Guest(_))
    ));
    assert!(matches!(
        enrich(&runtime, "garbage").await?,
        HookOutcome::Unavailable(HookFailure::Malformed(_))
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn every_call_gets_a_fresh_instance() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailClosed)?)?;
    let expected: Meta = serde_json::from_value(json!({ "calls": 1 }))?;
    for _ in 0..3 {
        assert_eq!(
            enrich(&runtime, "fresh").await?,
            HookOutcome::Enriched(expected.clone())
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn third_concurrent_call_is_overloaded() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailClosed)?)?;
    let running = spawn_invoke(&runtime, "slow");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let queued = spawn_invoke(&runtime, "slow");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let started = Instant::now();
    assert_eq!(
        enrich(&runtime, "ana").await?,
        HookOutcome::Unavailable(HookFailure::Overloaded)
    );
    assert!(started.elapsed() < Duration::from_millis(20), "overload must not wait");
    for call in [running, queued] {
        assert_eq!(
            call.await??.into_outcome(),
            HookOutcome::Unavailable(HookFailure::Deadline)
        );
    }
    assert!(matches!(enrich(&runtime, "ana").await?, HookOutcome::Enriched(_)));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn overload_follows_fail_open() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailOpen)?)?;
    let running = spawn_invoke(&runtime, "slow");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let queued = spawn_invoke(&runtime, "slow");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(enrich(&runtime, "ana").await?, HookOutcome::Enriched(meta()?));
    running.await??;
    queued.await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_output_is_capped_before_the_envelope() -> TestResult {
    let runtime = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailClosed)?)?;
    assert_eq!(
        enrich(&runtime, "raw-oversized").await?,
        HookOutcome::Unavailable(HookFailure::Oversized(OutputLimit::Raw))
    );
    assert_eq!(
        enrich(&runtime, "envelope-oversized").await?,
        HookOutcome::Unavailable(HookFailure::Oversized(OutputLimit::Envelope))
    );
    let open = HookRuntime::load(config(component_or_skip!(), HookPolicy::FailOpen)?)?;
    for key in ["raw-oversized", "envelope-oversized"] {
        assert_eq!(enrich(&open, key).await?, HookOutcome::Enriched(meta()?), "{key}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_keeps_the_previous_component_on_a_bad_candidate() -> TestResult {
    let original = component_or_skip!();
    let runtime = HookRuntime::load(HookConfig {
        deadline: HookDeadline::try_from(Duration::from_secs(2))?,
        ..config(original.clone(), HookPolicy::FailClosed)?
    })?;
    let before = runtime.digest();
    let dir = std::env::temp_dir().join(format!("trogon-hook-reload-{}", std::process::id()));
    let bad = wat::write(&dir, "bad", &["wasi:sockets/tcp@0.2.12"])?;
    let good = wat::write(&dir, "good", &[])?;

    let in_flight = spawn_invoke(&runtime, "slow");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(matches!(runtime.reload(&bad), Err(HookLoadError::Import(_))));
    assert!(matches!(
        runtime.reload(&dir.join("missing.wasm")),
        Err(HookLoadError::Read { .. })
    ));
    assert_eq!(runtime.digest(), before);

    let after = runtime.reload(&good)?;
    assert_ne!(after, before);
    assert_eq!(runtime.digest(), after);
    assert!(
        !in_flight.is_finished(),
        "the in-flight call must still be running across the reload"
    );
    let in_flight = in_flight.await??;
    assert_eq!(in_flight.digest(), &before);
    assert_eq!(in_flight.outcome(), &HookOutcome::Unavailable(HookFailure::Deadline));

    let fresh = invoke(&runtime, "ana").await?;
    assert_eq!(fresh.digest(), &after);
    assert_eq!(
        fresh.into_outcome(),
        HookOutcome::Enriched(serde_json::from_str(wat::OUTPUT)?)
    );
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn rejects_a_non_component() -> TestResult {
    let dir = std::env::temp_dir().join(format!("trogon-hook-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("not-a-component.wasm");
    std::fs::write(&path, b"\0asm\x01\0\0\0")?;
    let loaded = HookRuntime::load(HookConfig::new(&path));
    std::fs::remove_dir_all(&dir)?;
    assert!(matches!(loaded, Err(HookLoadError::Compile(_))), "{loaded:?}");
    assert!(matches!(
        HookRuntime::load(HookConfig::new(dir.join("missing.wasm"))),
        Err(HookLoadError::Read { .. })
    ));
    Ok(())
}

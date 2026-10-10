mod common;

use async_nats::jetstream::{self, stream};
use common::NatsServer;
use trogon_presence::{
    BucketField, Incompatible, OpenError, Presence, PresenceConfig, ProvisionError, ProvisionOptions, UnreadyReason,
};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const SERVER_METADATA_PREFIX: &str = "_nats.";
const PROBE_STREAM_PREFIX: &str = "PRESENCE_PROBE_";
const LEGACY_MAX_MESSAGE_BYTES: i32 = 16 * 1024;

macro_rules! server_or_skip {
    () => {
        match NatsServer::start().await {
            Some(server) => server,
            None => return Ok(()),
        }
    };
}

async fn recreate_with(
    client: &async_nats::Client,
    config: &PresenceConfig,
    change: impl FnOnce(&mut stream::Config),
) -> TestResult {
    let context = jetstream::new(client.clone());
    let name = config.bucket().stream_name();
    let mut stream_config = context.get_stream(&name).await?.cached_info().config.clone();
    stream_config
        .metadata
        .retain(|key, _| !key.starts_with(SERVER_METADATA_PREFIX));
    change(&mut stream_config);
    context.delete_stream(&name).await?;
    context.create_stream(stream_config).await?;
    Ok(())
}

fn incompatible_field(result: Result<Presence, ProvisionError>) -> Option<BucketField> {
    match result {
        Err(ProvisionError::Incompatible(Incompatible { field, .. })) => Some(field),
        _ => None,
    }
}

#[tokio::test]
async fn a_fresh_create_succeeds_and_a_second_run_verifies() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let first = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    let second = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    assert_eq!(first.generation(), second.generation());
    assert_eq!(first.fingerprint(), second.fingerprint());
    first.verify_ready().await?;
    let info = jetstream::new(client)
        .get_stream(config.bucket().stream_name())
        .await?
        .cached_info()
        .clone();
    assert!(info.config.allow_atomic_publish);
    assert!(!info.config.allow_direct);
    first.close();
    second.close();
    Ok(())
}

#[tokio::test]
async fn an_existing_stream_with_a_sixteen_kib_message_limit_is_refused() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    Presence::provision(client.clone(), config.clone(), ProvisionOptions::default())
        .await?
        .close();
    recreate_with(&client, &config, |stream| {
        stream.max_message_size = LEGACY_MAX_MESSAGE_BYTES;
    })
    .await?;
    let result = Presence::provision(client, config, ProvisionOptions::default()).await;
    assert_eq!(incompatible_field(result), Some(BucketField::MaxMessageSize));
    Ok(())
}

#[tokio::test]
async fn an_existing_stream_without_atomic_publish_is_refused() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    Presence::provision(client.clone(), config.clone(), ProvisionOptions::default())
        .await?
        .close();
    recreate_with(&client, &config, |stream| {
        stream.allow_atomic_publish = false;
    })
    .await?;
    let result = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await;
    assert_eq!(incompatible_field(result), Some(BucketField::AtomicPublish));
    assert!(matches!(
        Presence::open(client, config).await.err(),
        Some(OpenError::Incompatible(Incompatible {
            field: BucketField::AtomicPublish,
            ..
        }))
    ));
    Ok(())
}

#[tokio::test]
async fn recreating_the_stream_changes_the_fingerprint_and_reports_unready() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let presence = Presence::provision(client.clone(), config.clone(), ProvisionOptions::default()).await?;
    recreate_with(&client, &config, |_| {}).await?;
    assert!(matches!(
        presence.verify_ready().await.err(),
        Some(OpenError::Unready {
            reason: UnreadyReason::FingerprintChanged,
            ..
        })
    ));
    assert!(matches!(
        Presence::open(client.clone(), config.clone()).await.err(),
        Some(OpenError::Unready {
            reason: UnreadyReason::FingerprintChanged,
            ..
        })
    ));
    jetstream::new(client)
        .delete_stream(config.bucket().stream_name())
        .await?;
    assert!(matches!(
        presence.verify_ready().await.err(),
        Some(OpenError::Unready {
            reason: UnreadyReason::StreamMissing,
            ..
        })
    ));
    presence.close();
    Ok(())
}

async fn probe_records(
    client: &async_nats::Client,
    config: &PresenceConfig,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let mut probe = jetstream::new(client.clone())
        .get_stream(config.bucket().probe_stream_name())
        .await?;
    Ok(probe.info().await?.state.messages)
}

#[tokio::test]
async fn the_probe_reuses_its_stream_and_leaves_no_records_behind() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let probe = config.bucket().probe_stream_name();
    assert_eq!(probe, format!("{PROBE_STREAM_PREFIX}{}", config.bucket().as_str()));
    let context = jetstream::new(client.clone());
    let mut created = Vec::new();
    for _ in 0..2 {
        Presence::provision(client.clone(), config.clone(), ProvisionOptions::default())
            .await?
            .close();
        assert_eq!(probe_records(&client, &config).await?, 0, "probe records left behind");
        created.push(context.get_stream(&probe).await?.cached_info().created);
    }
    assert_eq!(created[0], created[1], "a compatible probe stream was recreated");
    Ok(())
}

#[tokio::test]
async fn an_incompatible_probe_stream_is_replaced() -> TestResult {
    let server = server_or_skip!();
    let client = server.client().await;
    let config = PresenceConfig::default();
    let bucket = config.bucket().clone();
    let context = jetstream::new(client.clone());
    context
        .create_stream(stream::Config {
            name: bucket.probe_stream_name(),
            subjects: vec![format!("{}.>", bucket.probe_subject_root())],
            ..Default::default()
        })
        .await?;
    Presence::provision(client.clone(), config.clone(), ProvisionOptions::default())
        .await?
        .close();
    let replaced = context.get_stream(bucket.probe_stream_name()).await?;
    assert!(
        replaced.cached_info().config.allow_atomic_publish,
        "the incompatible probe stream survived"
    );
    assert_eq!(probe_records(&client, &config).await?, 0, "probe records left behind");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_provisions_of_one_bucket_all_succeed() -> TestResult {
    const PROVISIONERS: usize = 8;
    let server = server_or_skip!();
    let mut clients = Vec::with_capacity(PROVISIONERS);
    for _ in 0..PROVISIONERS {
        clients.push(server.client().await);
    }
    let outcomes = futures_util::future::join_all(
        clients
            .into_iter()
            .map(|client| Presence::provision(client, PresenceConfig::default(), ProvisionOptions::default())),
    )
    .await;
    let failures: Vec<String> = outcomes
        .into_iter()
        .filter_map(|outcome| match outcome {
            Ok(presence) => {
                presence.close();
                None
            }
            Err(err) => Some(err.to_string()),
        })
        .collect();
    assert!(failures.is_empty(), "concurrent provisions failed: {failures:?}");
    assert_eq!(
        probe_records(&server.client().await, &PresenceConfig::default()).await?,
        0,
        "probe records left behind"
    );
    Ok(())
}

mod common;

use std::num::NonZeroUsize;
use std::time::Duration;

use common::{
    jwt_claims, jwt_header, topics, Fixture, FixtureOptions, TestResult, APP_A, APP_B, DEFAULT_SESSION, TENANT_A,
    TENANT_B,
};
use futures_util::StreamExt;
use trogon_presence::{OwnerId, Presence, PresenceConfig, ProvisionOptions, SnapshotId, StreamGeneration, WriterShard};
use trogon_presence_auth::{
    AdmissionKeys, AdmissionRead, AdmitOutcome, AuthKey, AuthorizationRequest, CalloutConcurrency, CalloutContext,
    CommandError, CommandState, ConnectIdentity, GrantPolicy, GrantRow, IdentityBound, PolicyRow, RateLimit, Rejection,
    ResponseBudget, SessionTarget, UnixSeconds, AUTH_ROW_SCHEMA_V1,
};
use trogon_presence_service::subjects::{
    diff_subject, internal_write_subject, snapshot_subject, ReadOp, SnapshotReplySubject, WriteOp,
};
use trogon_presence_service::{provision, LeaseKey, LeaseStore, LeaseValue, NodeId, ServiceConfig};

const MAX_PAYLOAD: usize = 1024 * 1024;

macro_rules! fixture {
    ($options:expr) => {
        match Fixture::start($options).await {
            Some(fixture) => fixture,
            None => return Ok(()),
        }
    };
}

async fn authorize(
    fixture: &Fixture,
    identity: &ConnectIdentity,
    max_payload: usize,
) -> Result<Result<trogon_presence_auth::Authorized, Rejection>, common::BoxError> {
    let token = fixture.token(identity)?;
    let direct = fixture.direct_request(&token)?;
    Ok(authorize_request(fixture, &direct.request, &direct.server_xkey, max_payload).await)
}

async fn authorize_request(
    fixture: &Fixture,
    request: &AuthorizationRequest,
    server_xkey: &trogon_presence_auth::ServerXKey,
    max_payload: usize,
) -> Result<trogon_presence_auth::Authorized, Rejection> {
    let context = CalloutContext {
        server_xkey,
        max_payload,
        now: UnixSeconds::now(),
    };
    fixture.callout.authorize(request, &context).await
}

#[tokio::test]
async fn encrypted_round_trip_grants_tenant_scoped_permissions() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let granted = topics(&["rooms.alpha"])?;
    let enrolled = fixture.enroll(TENANT_A, "ana@acme.io", &granted).await?;
    let identity = &enrolled.identity;
    let token = fixture.token(identity)?;
    let mut session = fixture.connect(identity, &token).await?;
    let key = identity.sub.key();
    let shards = GrantPolicy::default().shards;
    let other = "rooms.gamma".parse()?;

    let allowed_subscribe = vec![diff_subject(&granted[0])];
    assert!(session.subscribe_violations(&allowed_subscribe).await?.is_empty());
    let denied_subscribe = vec![diff_subject(&other)];
    assert_eq!(session.subscribe_violations(&denied_subscribe).await?, denied_subscribe);

    let snapshot = snapshot_subject(shards, key, &identity.cid, &granted[0]);
    let allowed_publish = vec![
        snapshot,
        WriteOp::ALL[0].subject(key, &granted[0]),
        ReadOp::ALL[0].subject(shards, key, &granted[0]),
    ];
    assert!(session.publish_violations(&allowed_publish).await?.is_empty());

    let snapshot_reply = SnapshotReplySubject::new(key, &identity.cid, &SnapshotId::generate()?).to_string();
    let denied_publish = vec![
        snapshot_reply,
        internal_write_subject(shards, key, Some(&granted[0])),
        "$JS.API.INFO".to_owned(),
        "$JS.API.STREAM.CREATE.KV_PRESENCE_V1".to_owned(),
        diff_subject(&granted[0]),
        WriteOp::ALL[0].subject(key, &other),
    ];
    let mut violations = session.publish_violations(&denied_publish).await?;
    violations.sort();
    let mut expected = denied_publish.clone();
    expected.sort();
    assert_eq!(violations, expected);
    Ok(())
}

#[tokio::test]
async fn user_jwt_uses_nats_encodings() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let enrolled = fixture.enroll(TENANT_B, "bo", &topics(&["rooms.alpha"])?).await?;
    let authorized = authorize(&fixture, &enrolled.identity, MAX_PAYLOAD).await??;
    let header = jwt_header(&authorized.user_jwt)?;
    assert_eq!(header["alg"], "ed25519-nkey");
    assert_eq!(header["typ"], "JWT");
    let claims = jwt_claims(&authorized.user_jwt)?;
    assert!(claims["iss"].as_str().is_some_and(|iss| iss.starts_with('A')));
    assert!(claims["sub"].as_str().is_some_and(|sub| sub.starts_with('U')));
    assert_eq!(claims["aud"], APP_B);
    assert_eq!(claims["nats"]["type"], "user");
    assert_eq!(claims["nats"]["version"], 2);
    Ok(())
}

#[tokio::test]
async fn server_without_xkey_is_denied() -> TestResult {
    let fixture = fixture!(FixtureOptions {
        xkey: false,
        ..FixtureOptions::default()
    });
    let enrolled = fixture.enroll(TENANT_A, "ana", &topics(&["rooms.alpha"])?).await?;
    let token = fixture.token(&enrolled.identity)?;
    assert!(fixture.connect(&enrolled.identity, &token).await.is_err());
    Ok(())
}

#[tokio::test]
async fn tenants_are_isolated_by_account() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let topic = topics(&["rooms.shared"])?;
    let ana = fixture.enroll(TENANT_A, "ana", &topic).await?;
    let bo = fixture.enroll(TENANT_B, "bo", &topic).await?;
    let ana_session = fixture.connect(&ana.identity, &fixture.token(&ana.identity)?).await?;
    let bo_session = fixture.connect(&bo.identity, &fixture.token(&bo.identity)?).await?;
    let mut ana_diffs = ana_session.client.subscribe(diff_subject(&topic[0])).await?;
    let mut bo_diffs = bo_session.client.subscribe(diff_subject(&topic[0])).await?;
    ana_session.client.flush().await?;
    bo_session.client.flush().await?;

    let runtime_a = fixture.users.app_a.runtime.connect(&fixture.url).await?;
    runtime_a.publish(diff_subject(&topic[0]), "a".into()).await?;
    runtime_a.flush().await?;
    let received = tokio::time::timeout(Duration::from_secs(2), ana_diffs.next()).await?;
    assert!(received.is_some());
    let leaked = tokio::time::timeout(Duration::from_millis(500), bo_diffs.next()).await;
    assert!(leaked.is_err());

    let cross = fixture.token_for_account(&ana.identity, APP_B.parse()?)?;
    assert!(fixture.connect(&ana.identity, &cross).await.is_err());
    let direct = fixture.direct_request(&cross)?;
    let rejection = authorize_request(&fixture, &direct.request, &direct.server_xkey, MAX_PAYLOAD).await;
    assert!(matches!(rejection, Err(Rejection::Account)));
    assert_eq!(fixture.account(&ana.identity.tenant)?.as_str(), APP_A);
    Ok(())
}

#[tokio::test]
async fn short_session_caps_user_jwt_expiry() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let request = fixture.enroll_request(
        TENANT_A,
        "ana",
        "s1",
        &topics(&["rooms.alpha"])?,
        Duration::from_secs(120),
    )?;
    let enrolled = fixture.enroller.enroll(&request).await?;
    let authorized = authorize(&fixture, &enrolled.identity, MAX_PAYLOAD).await??;
    assert_eq!(authorized.expires_at, request.session_expires_at);
    assert_eq!(
        jwt_claims(&authorized.user_jwt)?["exp"].as_u64(),
        Some(request.session_expires_at.get())
    );

    let long = fixture.enroll_request(TENANT_A, "bo", "s1", &topics(&["rooms.alpha"])?, DEFAULT_SESSION)?;
    let enrolled = fixture.enroller.enroll(&long).await?;
    let before = UnixSeconds::now();
    let authorized = authorize(&fixture, &enrolled.identity, MAX_PAYLOAD).await??;
    let lifetime = before.until(authorized.expires_at);
    assert!(lifetime <= Duration::from_secs(601) && lifetime >= Duration::from_secs(599));
    Ok(())
}

#[tokio::test]
async fn expired_session_is_denied_despite_token_leeway() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let request = fixture.enroll_request(
        TENANT_A,
        "ana",
        "s1",
        &topics(&["rooms.alpha"])?,
        Duration::from_secs(2),
    )?;
    let enrolled = fixture.enroller.enroll(&request).await?;
    let token = fixture.token(&enrolled.identity)?;
    tokio::time::sleep(Duration::from_millis(3100)).await;
    let direct = fixture.direct_request(&token)?;
    let rejection = authorize_request(&fixture, &direct.request, &direct.server_xkey, MAX_PAYLOAD).await;
    assert!(matches!(rejection, Err(Rejection::SessionExpired)), "{rejection:?}");
    Ok(())
}

#[tokio::test]
async fn revoked_session_is_denied_and_cannot_re_enroll() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let granted = topics(&["rooms.alpha"])?;
    let request = fixture.enroll_request(TENANT_A, "ana", "s1", &granted, DEFAULT_SESSION)?;
    let enrolled = fixture.enroller.enroll(&request).await?;
    let token = fixture.token(&enrolled.identity)?;
    let target = SessionTarget {
        user: Fixture::user(TENANT_A, "ana")?,
        sid: request.sid.clone(),
    };
    let outcome = fixture.commands.revoke_session(&target).await?;
    assert!(outcome.state_committed());

    assert!(fixture.connect(&enrolled.identity, &token).await.is_err());
    let direct = fixture.direct_request(&token)?;
    let rejection = authorize_request(&fixture, &direct.request, &direct.server_xkey, MAX_PAYLOAD).await;
    assert!(
        matches!(
            rejection,
            Err(Rejection::SessionRevoked) | Err(Rejection::EpochMismatch)
        ),
        "{rejection:?}"
    );

    let again = fixture.enroll_request(TENANT_A, "ana", "s1", &granted, DEFAULT_SESSION)?;
    assert!(matches!(
        fixture.enroller.enroll(&again).await,
        Err(CommandError::SessionRevoked)
    ));
    Ok(())
}

#[tokio::test]
async fn duplicate_command_returns_the_stored_receipt() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let enrolled = fixture.enroll(TENANT_A, "ana", &topics(&["rooms.alpha"])?).await?;
    let target = Fixture::user(TENANT_A, "ana")?;
    let first = fixture.commands.revoke_user(&target).await?;
    let second = fixture.commands.revoke_user(&target).await?;
    assert_eq!(first.state, CommandState::Committed);
    assert_eq!(second.state, CommandState::Replayed);
    assert!(!second.state_committed());
    assert_eq!(first.receipt, second.receipt);
    assert_eq!(second.sweep(), first.sweep());

    let policy_key = AuthKey::policy(&enrolled.identity.tenant, &enrolled.identity.sub);
    let policy = fixture.issuer_store.read::<PolicyRow>(&policy_key).await?;
    let (_, policy) = policy.present().ok_or("policy row is missing")?;
    assert_eq!(policy.epoch, first.receipt.epoch);
    assert_eq!(enrolled.identity.auth_epoch.next(), Some(policy.epoch));

    assert!(matches!(
        fixture.commands.disable_user(&target).await,
        Err(CommandError::OperationReused(_))
    ));
    Ok(())
}

#[tokio::test]
async fn policy_moving_between_read_and_admit_loses_the_cas() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let enrolled = fixture.enroll(TENANT_A, "ana", &topics(&["rooms.alpha"])?).await?;
    let identity = &enrolled.identity;
    let keys = AdmissionKeys::of(identity);
    let AdmissionRead::Consistent(view) = fixture.callout_store.read_admission(&keys).await? else {
        return Err("admission read was not consistent".into());
    };
    fixture.commands.revoke_user(&Fixture::user(TENANT_A, "ana")?).await?;

    let token = fixture.token(identity)?;
    let direct = fixture.direct_request(&token)?;
    let grant_key = AuthKey::grant(&direct.request.server, direct.request.client);
    let now = UnixSeconds::now();
    let grant = GrantRow {
        v: AUTH_ROW_SCHEMA_V1,
        tenant: identity.tenant.clone(),
        sub: identity.sub.clone(),
        account: fixture.account(&identity.tenant)?,
        sid: identity.sid.clone(),
        cid: identity.cid,
        epoch: identity.auth_epoch,
        version: identity.asv,
        expires_at: now.saturating_add(Duration::from_secs(60)),
    };
    let outcome = fixture
        .callout_store
        .admit(&view, &keys, &grant_key, &grant, Duration::from_secs(120))
        .await?;
    assert_eq!(outcome, AdmitOutcome::PolicyMoved);
    assert!(fixture
        .callout_store
        .read::<GrantRow>(&grant_key)
        .await?
        .present()
        .is_none());

    let rejection = authorize_request(&fixture, &direct.request, &direct.server_xkey, MAX_PAYLOAD).await;
    assert!(matches!(rejection, Err(Rejection::EpochMismatch)), "{rejection:?}");
    Ok(())
}

#[tokio::test]
async fn saturated_concurrency_denies_until_a_slot_frees() -> TestResult {
    let fixture = fixture!(FixtureOptions {
        concurrency: CalloutConcurrency::new(1)?,
        ..FixtureOptions::default()
    });
    let enrolled = fixture.enroll(TENANT_A, "ana", &topics(&["rooms.alpha"])?).await?;
    let slot = fixture.callout.reserve_slot().ok_or("no free slot")?;
    assert!(fixture.callout.reserve_slot().is_none());
    let token = fixture.token(&enrolled.identity)?;
    assert!(fixture.connect(&enrolled.identity, &token).await.is_err());
    drop(slot);
    let token = fixture.token(&enrolled.identity)?;
    assert!(fixture.connect(&enrolled.identity, &token).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn identity_bound_denies_new_identities_at_capacity() -> TestResult {
    let fixture = fixture!(FixtureOptions {
        identity_bound: IdentityBound::new(NonZeroUsize::MIN, Duration::from_secs(900))?,
        ..FixtureOptions::default()
    });
    let ana = fixture.enroll(TENANT_A, "ana", &topics(&["rooms.alpha"])?).await?;
    let bo = fixture.enroll(TENANT_A, "bo", &topics(&["rooms.alpha"])?).await?;
    assert!(authorize(&fixture, &ana.identity, MAX_PAYLOAD).await?.is_ok());
    let rejection = authorize(&fixture, &bo.identity, MAX_PAYLOAD).await?;
    assert!(matches!(rejection, Err(Rejection::Busy)), "{rejection:?}");
    let token = fixture.token(&bo.identity)?;
    assert!(fixture.connect(&bo.identity, &token).await.is_err());
    Ok(())
}

#[tokio::test]
async fn rate_limit_denies_beyond_the_burst() -> TestResult {
    let fixture = fixture!(FixtureOptions {
        rate_limit: RateLimit::default(),
        ..FixtureOptions::default()
    });
    let enrolled = fixture.enroll(TENANT_A, "ana", &topics(&["rooms.alpha"])?).await?;
    for _ in 0..5 {
        assert!(authorize(&fixture, &enrolled.identity, MAX_PAYLOAD).await?.is_ok());
    }
    let rejection = authorize(&fixture, &enrolled.identity, MAX_PAYLOAD).await?;
    assert!(matches!(rejection, Err(Rejection::RateLimited)), "{rejection:?}");
    Ok(())
}

#[tokio::test]
async fn response_over_budget_is_denied() -> TestResult {
    let policy = GrantPolicy {
        response_budget: ResponseBudget::bytes(1024)?,
        ..GrantPolicy::default()
    };
    let fixture = fixture!(FixtureOptions {
        policy,
        ..FixtureOptions::default()
    });
    let many: Vec<String> = (0..64).map(|n| format!("rooms.room{n}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let enrolled = fixture.enroll(TENANT_A, "ana", &topics(&many)?).await?;
    let rejection = authorize(&fixture, &enrolled.identity, MAX_PAYLOAD).await?;
    assert!(matches!(rejection, Err(Rejection::ResponseTooLarge)), "{rejection:?}");
    Ok(())
}

#[tokio::test]
async fn too_many_topics_are_refused_at_enroll() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let many: Vec<String> = (0..129).map(|n| format!("rooms.room{n}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let request = fixture.enroll_request(TENANT_A, "ana", "s1", &topics(&many)?, DEFAULT_SESSION)?;
    assert!(matches!(
        fixture.enroller.enroll(&request).await,
        Err(CommandError::TooManyTopics { .. })
    ));
    Ok(())
}

const DENIED_WITHIN: Duration = Duration::from_secs(1);

async fn refused<T, E>(call: impl std::future::Future<Output = Result<T, E>>) -> bool {
    !matches!(tokio::time::timeout(DENIED_WITHIN, call).await, Ok(Ok(_)))
}

async fn create_fails(client: &async_nats::Client, name: &str, subject: &str) -> bool {
    let context = async_nats::jetstream::new(client.clone());
    refused(context.create_stream(async_nats::jetstream::stream::Config {
        name: name.to_owned(),
        subjects: vec![subject.to_owned()],
        ..Default::default()
    }))
    .await
}

#[tokio::test]
async fn provisioner_creates_streams_and_runtime_only_opens() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let service = ServiceConfig::new(PresenceConfig::default(), NodeId::generate()?);
    let lease = service.lease_bucket().clone();
    let runtime = fixture.users.app_a.runtime.connect(&fixture.url).await?;
    assert!(create_fails(&runtime, &lease.stream_name(), &lease.subjects_filter()).await);

    let provisioner = fixture.users.app_a.provisioner.connect(&fixture.url).await?;
    let provisioned = provision(provisioner.clone(), &service, ProvisionOptions::default()).await;
    assert!(provisioned.is_ok(), "{:?}", provisioned.err());
    let again = provision(provisioner.clone(), &service, ProvisionOptions::default()).await;
    assert!(again.is_ok(), "{:?}", again.err());

    let opened = Presence::open(runtime.clone(), service.presence().clone()).await;
    assert!(opened.is_ok(), "{:?}", opened.err());
    let shards = service.presence().shards();
    let leases = LeaseStore::open(
        runtime.clone(),
        lease.clone(),
        shards,
        service.lease_ttl(),
        LeaseValue::new(OwnerId::generate()?, StreamGeneration::generate()?),
    )
    .await?;
    let shard = LeaseKey::Writer(WriterShard::of(&"probe".parse()?, shards));
    assert!(leases.acquire(shard).await?.is_some());

    assert!(create_fails(&runtime, &lease.stream_name(), &lease.subjects_filter()).await);
    assert!(create_fails(&runtime, "ROGUE", "rogue.>").await);
    assert!(create_fails(&provisioner, "ROGUE", "rogue.>").await);
    let context = async_nats::jetstream::new(provisioner);
    assert!(refused(context.delete_stream(service.presence().bucket().stream_name())).await);
    assert!(refused(context.delete_stream(lease.stream_name())).await);
    let observer = async_nats::jetstream::new(runtime);
    assert!(observer
        .get_stream(service.presence().bucket().stream_name())
        .await
        .is_ok());
    assert!(observer.get_stream(lease.stream_name()).await.is_ok());

    let other_tenant = fixture.users.app_b.runtime.connect(&fixture.url).await?;
    let missing = Presence::open(other_tenant, PresenceConfig::default()).await;
    assert!(missing.is_err());
    Ok(())
}

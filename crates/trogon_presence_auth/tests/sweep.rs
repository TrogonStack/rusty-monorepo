mod common;

use std::time::Duration;

use async_nats::connection::State;
use async_nats::HeaderMap;
use common::{topics, Fixture, FixtureOptions, Session, TestResult, DEFAULT_SESSION, TENANT_A};
use trogon_presence_auth::{
    revoke, AuthKey, BrokerClientId, BrokerConnection, CommandState, ConnzPageLimit, Coverage, EnrollOutcome,
    IncompleteReason, RateBurst, RateLimit, ServerNkey, SweepDeadline, SweepRow, SweepState, UserTarget,
};

const KICKED_WITHIN: Duration = Duration::from_secs(3);
const DRIVE_DEADLINE: Duration = Duration::from_secs(20);
const SHORT_DEADLINE: Duration = Duration::from_secs(7);
const DEFAULT_JWT_CEILING: Duration = Duration::from_secs(900);

macro_rules! fixture {
    ($options:expr) => {
        match Fixture::start($options).await {
            Some(fixture) => fixture,
            None => return Ok(()),
        }
    };
}

fn deadline(duration: Duration) -> Result<SweepDeadline, common::BoxError> {
    Ok(SweepDeadline::try_from(duration)?)
}

fn generous_rate() -> Result<FixtureOptions, common::BoxError> {
    Ok(FixtureOptions {
        rate_limit: RateLimit::new(Duration::from_millis(1), RateBurst::try_from(100)?)?,
        ..FixtureOptions::default()
    })
}

fn silent_server() -> Result<ServerNkey, common::BoxError> {
    Ok(nkeys::KeyPair::new_server().public_key().parse()?)
}

async fn live(fixture: &Fixture, sid: &str) -> Result<(EnrollOutcome, Session), common::BoxError> {
    let request = fixture.enroll_request(TENANT_A, "ana", sid, &topics(&["rooms.alpha"])?, DEFAULT_SESSION)?;
    let enrolled = fixture.enroller.enroll(&request).await?;
    let token = fixture.token(&enrolled.identity)?;
    let session = fixture.connect(&enrolled.identity, &token).await?;
    Ok((enrolled, session))
}

async fn revoke_ana(fixture: &Fixture) -> Result<UserTarget, common::BoxError> {
    let target = Fixture::user(TENANT_A, "ana")?;
    let outcome = fixture.commands.revoke_user(&target).await?;
    assert!(outcome.state_committed());
    Ok(target)
}

async fn disconnected_within(session: &mut Session, within: Duration) -> bool {
    let until = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(until, session.events.recv()).await {
            Ok(Some(event)) if event.to_lowercase().contains("disconnected") => return true,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return false,
        }
    }
}

fn still_connected(session: &mut Session) -> bool {
    let mut disconnected = false;
    while let Ok(event) = session.events.try_recv() {
        disconnected |= event.to_lowercase().contains("disconnected");
    }
    !disconnected && session.client.connection_state() == State::Connected
}

async fn stored_sweep(fixture: &Fixture, target: &UserTarget) -> Result<SweepRow, common::BoxError> {
    let key = AuthKey::sweep(&target.tenant, &target.sub, &target.operation);
    let (_, row) = fixture
        .commands
        .store()
        .read::<SweepRow>(&key)
        .await?
        .present()
        .ok_or("sweep row is missing")?;
    Ok(row)
}

#[tokio::test]
async fn revoke_kicks_a_live_session_and_blocks_reconnect() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let (enrolled, mut session) = live(&fixture, "s1").await?;
    let executor = fixture.executor(fixture.sweep_config(vec![fixture.server()?])?).await?;
    let target = Fixture::user(TENANT_A, "ana")?;
    let (report, kicked) = tokio::join!(
        revoke(&fixture.commands, &executor, &target, deadline(DRIVE_DEADLINE)?),
        disconnected_within(&mut session, KICKED_WITHIN),
    );
    let report = report?;
    assert!(kicked, "the live session was not kicked within one repetition");
    assert!(report.state_committed());
    let sweep = report.sweep.ok_or("revoke must create a sweep")?;
    assert!(sweep.state.is_complete(), "{:?}", sweep.state);
    assert_eq!(sweep.kicked.len(), 1);
    assert!(sweep.failed.is_empty());
    assert!(stored_sweep(&fixture, &target).await?.is_complete());
    assert!(executor.status(&target).await?.is_complete());
    let token = fixture.token(&enrolled.identity)?;
    assert!(fixture.connect(&enrolled.identity, &token).await.is_err());
    Ok(())
}

#[tokio::test]
async fn reauthorized_newer_session_survives_the_older_sweep() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let (_, mut old) = live(&fixture, "s1").await?;
    let target = revoke_ana(&fixture).await?;
    let (_, mut newer) = live(&fixture, "s2").await?;
    let executor = fixture.executor(fixture.sweep_config(vec![fixture.server()?])?).await?;
    let report = executor.drive(&target, deadline(DRIVE_DEADLINE)?).await?;
    assert!(report.state.is_complete(), "{:?}", report.state);
    assert_eq!(report.kicked.len(), 1);
    assert!(disconnected_within(&mut old, Duration::from_millis(500)).await);
    newer.client.flush().await?;
    assert!(still_connected(&mut newer));
    Ok(())
}

#[tokio::test]
async fn connz_pages_cover_more_connections_than_one_page() -> TestResult {
    let fixture = fixture!(generous_rate()?);
    let mut sessions = Vec::new();
    for _ in 0..5 {
        sessions.push(live(&fixture, "s1").await?.1);
    }
    let target = revoke_ana(&fixture).await?;
    let server = fixture.server()?;
    let mut config = fixture.sweep_config(vec![server.clone()])?;
    config.page_limit = ConnzPageLimit::try_from(2)?;
    let executor = fixture.executor(config).await?;
    let first = executor.pass(&target).await?;
    assert_eq!(
        first.coverage.first().map(|server| &server.coverage),
        Some(&Coverage::Covered {
            connections: 5,
            pages: 3
        })
    );
    assert_eq!(first.kicked.len(), 5);
    let report = executor.drive(&target, deadline(DRIVE_DEADLINE)?).await?;
    assert!(report.state.is_complete(), "{:?}", report.state);
    for session in &mut sessions {
        assert!(disconnected_within(session, Duration::from_millis(500)).await);
    }
    Ok(())
}

#[tokio::test]
async fn other_users_in_the_account_do_not_stall_connz_paging() -> TestResult {
    let fixture = fixture!(generous_rate()?);
    let (_, mut session) = live(&fixture, "s1").await?;
    let mut bystanders = Vec::new();
    for sub in ["bob", "carol"] {
        let request = fixture.enroll_request(TENANT_A, sub, "s1", &topics(&["rooms.alpha"])?, DEFAULT_SESSION)?;
        let enrolled = fixture.enroller.enroll(&request).await?;
        let token = fixture.token(&enrolled.identity)?;
        bystanders.push(fixture.connect(&enrolled.identity, &token).await?);
    }
    let target = revoke_ana(&fixture).await?;
    let executor = fixture.executor(fixture.sweep_config(vec![fixture.server()?])?).await?;
    let first = executor.pass(&target).await?;
    assert_eq!(
        first.coverage.first().map(|server| &server.coverage),
        Some(&Coverage::Covered {
            connections: 1,
            pages: 1
        })
    );
    assert_eq!(first.kicked.len(), 1);
    assert!(disconnected_within(&mut session, Duration::from_millis(500)).await);
    for bystander in &mut bystanders {
        bystander.client.flush().await?;
        assert!(still_connected(bystander));
    }
    Ok(())
}

#[tokio::test]
async fn silent_roster_server_keeps_the_sweep_pending() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let (_, mut session) = live(&fixture, "s1").await?;
    let target = revoke_ana(&fixture).await?;
    let silent = silent_server()?;
    let executor = fixture
        .executor(fixture.sweep_config(vec![fixture.server()?, silent.clone()])?)
        .await?;
    let report = executor.drive(&target, deadline(SHORT_DEADLINE)?).await?;
    assert_eq!(
        report.state,
        SweepState::Incomplete(vec![IncompleteReason::MissingServerResponse {
            servers: vec![silent.clone()]
        }])
    );
    assert_eq!(report.kicked.len(), 1);
    assert!(disconnected_within(&mut session, Duration::from_millis(500)).await);
    assert!(!stored_sweep(&fixture, &target).await?.is_complete());
    assert_eq!(executor.status(&target).await?, report.state);
    Ok(())
}

#[tokio::test]
async fn missing_ledger_row_falls_back_to_the_jwt_ceiling() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let (_, mut session) = live(&fixture, "s1").await?;
    let server = fixture.server()?;
    let connection = BrokerConnection {
        server: server.clone(),
        client: BrokerClientId::new(session.client.server_info().client_id),
    };
    let grant = AuthKey::grant(&connection.server, connection.client);
    let callout = fixture.users.callout.connect(&fixture.url).await?;
    let mut headers = HeaderMap::new();
    headers.insert("KV-Operation", "DEL");
    async_nats::jetstream::new(callout)
        .publish_with_headers(format!("$KV.PRESENCE_AUTH_V1.{grant}"), headers, "".into())
        .await?
        .await?;
    let target = revoke_ana(&fixture).await?;
    let created_at = stored_sweep(&fixture, &target).await?.created_at;
    let executor = fixture.executor(fixture.sweep_config(vec![server])?).await?;
    let report = executor.drive(&target, deadline(SHORT_DEADLINE)?).await?;
    let SweepState::Incomplete(reasons) = &report.state else {
        return Err(format!("expected an incomplete sweep, got {:?}", report.state).into());
    };
    let [IncompleteReason::UnattributedConnections { connections, ceiling }] = reasons.as_slice() else {
        return Err(format!("expected only the ceiling fallback, got {reasons:?}").into());
    };
    assert_eq!(connections, &vec![connection]);
    assert!(*ceiling >= created_at.saturating_add(DEFAULT_JWT_CEILING));
    assert!(report.kicked.is_empty());
    assert!(!stored_sweep(&fixture, &target).await?.is_complete());
    assert!(still_connected(&mut session));
    Ok(())
}

#[tokio::test]
async fn restart_resumes_an_incomplete_sweep_from_the_store() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let (_, mut session) = live(&fixture, "s1").await?;
    let target = revoke_ana(&fixture).await?;
    let before = fixture.executor(fixture.sweep_config(vec![silent_server()?])?).await?;
    let reports = before.tick().await?;
    let ours = reports
        .iter()
        .find(|report| report.operation == target.operation)
        .ok_or("the first process did not pick up the sweep")?;
    assert!(matches!(ours.state, SweepState::Incomplete(_)), "{:?}", ours.state);
    drop(before);
    assert!(still_connected(&mut session));

    let after = fixture.executor(fixture.sweep_config(vec![fixture.server()?])?).await?;
    assert_eq!(after.status(&target).await?, SweepState::Pending);
    let until = tokio::time::Instant::now() + DRIVE_DEADLINE;
    let mut kicked = 0;
    loop {
        let reports = after.tick().await?;
        let ours = reports.iter().find(|report| report.operation == target.operation);
        kicked += ours.map_or(0, |report| report.kicked.len());
        if ours.is_none_or(|report| report.state.is_complete()) {
            break;
        }
        if tokio::time::Instant::now() > until {
            return Err("resumed sweep did not complete".into());
        }
        tokio::time::sleep(after.config().interval.get()).await;
    }
    assert_eq!(kicked, 1);
    assert!(disconnected_within(&mut session, Duration::from_millis(500)).await);
    assert!(stored_sweep(&fixture, &target).await?.is_complete());
    assert!(after.tick().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn duplicate_sweep_execution_is_idempotent() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let (_, _session) = live(&fixture, "s1").await?;
    let target = revoke_ana(&fixture).await?;
    let replayed = fixture.commands.revoke_user(&target).await?;
    assert_eq!(replayed.state, CommandState::Replayed);
    let config = fixture.sweep_config(vec![fixture.server()?])?;
    let first = fixture.executor(config.clone()).await?;
    let second = fixture.executor(config).await?;
    let (a, b) = tokio::join!(
        first.drive(&target, deadline(DRIVE_DEADLINE)?),
        second.drive(&target, deadline(DRIVE_DEADLINE)?),
    );
    let (a, b) = (a?, b?);
    assert!(
        a.state.is_complete() && b.state.is_complete(),
        "{:?} {:?}",
        a.state,
        b.state
    );
    assert_eq!(a.state, b.state);
    assert!(a.kicked.len() + b.kicked.len() >= 1);
    let completed = stored_sweep(&fixture, &target).await?.completed_at;
    let again = first.drive(&target, deadline(DRIVE_DEADLINE)?).await?;
    assert!(again.kicked.is_empty() && again.coverage.is_empty());
    assert_eq!(stored_sweep(&fixture, &target).await?.completed_at, completed);
    Ok(())
}

#[tokio::test]
async fn system_user_only_reaches_connz_and_kick() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let server = fixture.server()?;
    let mut system = fixture.users.system.session(&fixture.url).await?;
    let connz = system
        .client
        .request(format!("$SYS.REQ.SERVER.{server}.CONNZ"), "{}".into())
        .await?;
    assert!(String::from_utf8_lossy(&connz.payload).contains("\"connections\""));
    let denied: Vec<String> = [
        format!("$SYS.REQ.SERVER.{server}.VARZ"),
        format!("$SYS.REQ.SERVER.{server}.RELOAD"),
        "$SYS.REQ.SERVER.PING".to_owned(),
        "$SYS.REQ.ACCOUNT.APP_A.CONNZ".to_owned(),
        "$SYS.REQ.USER.INFO".to_owned(),
        "$JS.API.INFO".to_owned(),
        "$KV.PRESENCE_AUTH_V1.policy.x".to_owned(),
        "presence.v1.track.x".to_owned(),
    ]
    .to_vec();
    let violations = system.publish_violations(&denied).await?;
    assert_eq!(violations, denied);
    let subscriptions = vec!["$SYS.ACCOUNT.>".to_owned(), "$SYS.REQ.>".to_owned()];
    assert_eq!(system.subscribe_violations(&subscriptions).await?, subscriptions);
    Ok(())
}

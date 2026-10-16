mod common;

use std::time::Duration;

use common::{topics, Fixture, FixtureOptions, TestResult, DEFAULT_SESSION, TENANT_A};
use trogon_presence_auth::{
    AuthKey, AuthVersion, CalloutContext, CommandError, CommandState, ConnectIdentity, ConnectionRow, EnrollOutcome,
    RefreshRequest, Rejection, SessionCeiling, SessionRow, SessionTarget, SweepScope, SweepStatus, UnixSeconds,
};

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
) -> Result<Result<trogon_presence_auth::Authorized, Rejection>, common::BoxError> {
    let token = fixture.token(identity)?;
    let direct = fixture.direct_request(&token)?;
    let context = CalloutContext {
        server_xkey: &direct.server_xkey,
        max_payload: MAX_PAYLOAD,
        now: UnixSeconds::now(),
    };
    Ok(fixture.callout.authorize(&direct.request, &context).await)
}

async fn enrolled(fixture: &Fixture, sub: &str) -> Result<EnrollOutcome, common::BoxError> {
    let request = fixture.enroll_request(TENANT_A, sub, "s1", &topics(&["rooms.alpha"])?, DEFAULT_SESSION)?;
    Ok(fixture.enroller.enroll(&request).await?)
}

fn refresh_request(
    fixture: &Fixture,
    identity: &ConnectIdentity,
    extend: Duration,
) -> Result<RefreshRequest, common::BoxError> {
    Ok(RefreshRequest {
        session: SessionTarget {
            user: Fixture::user(TENANT_A, identity.sub.as_str())?,
            sid: identity.sid.clone(),
        },
        realm: fixture.realm.clone(),
        cid: identity.cid,
        session_expires_at: UnixSeconds::now().saturating_add(extend),
    })
}

#[tokio::test]
async fn refreshed_token_is_admitted_and_the_old_token_is_rejected() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let before = enrolled(&fixture, "ana").await?.identity;
    let old_token = fixture.token(&before)?;

    let request = refresh_request(&fixture, &before, Duration::from_secs(7200))?;
    let refreshed = fixture.enroller.refresh_session(&request).await?;
    let after = &refreshed.identity;
    assert_eq!(refreshed.outcome.state, CommandState::Committed);
    assert_eq!(after.sid, before.sid);
    assert_eq!(after.cid, before.cid);
    assert_eq!(after.auth_epoch, before.auth_epoch);
    assert_eq!(after.asv, AuthVersion::new(2));
    assert!(after.session_expires_at > before.session_expires_at);
    assert_eq!(
        refreshed.outcome.sweep(),
        &SweepStatus::Pending {
            sweep: request.session.user.operation,
            scope: SweepScope::Connection {
                sid: before.sid.clone(),
                cid: before.cid,
                below: after.asv,
            },
        }
    );

    let direct = fixture.direct_request(&old_token)?;
    let context = CalloutContext {
        server_xkey: &direct.server_xkey,
        max_payload: MAX_PAYLOAD,
        now: UnixSeconds::now(),
    };
    let stale = fixture.callout.authorize(&direct.request, &context).await;
    assert!(matches!(stale, Err(Rejection::VersionMismatch)), "{stale:?}");
    assert!(fixture.connect(&before, &old_token).await.is_err());

    let admitted = authorize(&fixture, after).await??;
    assert_eq!(admitted.claims.asv, after.asv);
    assert!(fixture.connect(after, &fixture.token(after)?).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn replayed_refresh_returns_the_stored_result_without_bumping_again() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let before = enrolled(&fixture, "ana").await?.identity;
    let request = refresh_request(&fixture, &before, Duration::from_secs(7200))?;
    let first = fixture.enroller.refresh_session(&request).await?;
    let second = fixture.enroller.refresh_session(&request).await?;
    assert_eq!(first.outcome.state, CommandState::Committed);
    assert_eq!(second.outcome.state, CommandState::Replayed);
    assert_eq!(first.outcome.receipt, second.outcome.receipt);
    assert_eq!(first.identity, second.identity);

    let key = AuthKey::connection(&before.tenant, &before.sub, &before.sid, &before.cid);
    let (_, connection) = fixture
        .issuer_store
        .read::<ConnectionRow>(&key)
        .await?
        .present()
        .ok_or("connection row is missing")?;
    assert_eq!(connection.version, AuthVersion::new(2));

    let revoke = SessionTarget {
        user: request.session.user.clone(),
        sid: before.sid.clone(),
    };
    assert!(matches!(
        fixture.commands.revoke_session(&revoke).await,
        Err(CommandError::OperationReused(_))
    ));
    Ok(())
}

#[tokio::test]
async fn revoked_session_cannot_be_refreshed() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let before = enrolled(&fixture, "ana").await?.identity;
    let target = SessionTarget {
        user: Fixture::user(TENANT_A, "ana")?,
        sid: before.sid.clone(),
    };
    assert!(fixture.commands.revoke_session(&target).await?.state_committed());
    let request = refresh_request(&fixture, &before, Duration::from_secs(7200))?;
    let refused = fixture.enroller.refresh_session(&request).await;
    assert!(matches!(refused, Err(CommandError::SessionRevoked)), "{refused:?}");
    let receipt = AuthKey::receipt(&before.tenant, &before.sub, &request.session.user.operation);
    assert!(fixture
        .issuer_store
        .read::<trogon_presence_auth::CommandReceipt>(&receipt)
        .await?
        .present()
        .is_none());
    Ok(())
}

#[tokio::test]
async fn refresh_racing_a_revoke_never_revives_the_session() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    for round in 0..6 {
        let sub = format!("racer{round}");
        let before = enrolled(&fixture, &sub).await?.identity;
        let target = SessionTarget {
            user: Fixture::user(TENANT_A, &sub)?,
            sid: before.sid.clone(),
        };
        let request = RefreshRequest {
            session: SessionTarget {
                user: Fixture::user(TENANT_A, &sub)?,
                sid: before.sid.clone(),
            },
            realm: fixture.realm.clone(),
            cid: before.cid,
            session_expires_at: UnixSeconds::now().saturating_add(Duration::from_secs(7200)),
        };
        let (revoked, refreshed) = tokio::join!(
            fixture.commands.revoke_session(&target),
            fixture.enroller.refresh_session(&request)
        );
        assert!(revoked?.state_committed());
        let session_key = AuthKey::session(&before.tenant, &before.sub, &before.sid);
        let (_, session) = fixture
            .issuer_store
            .read::<SessionRow>(&session_key)
            .await?
            .present()
            .ok_or("session row is missing")?;
        assert_eq!(session.status, trogon_presence_auth::RowStatus::Disabled);
        match refreshed {
            Ok(outcome) => {
                let rejection = authorize(&fixture, &outcome.identity).await?;
                assert!(
                    matches!(
                        rejection,
                        Err(Rejection::SessionRevoked) | Err(Rejection::EpochMismatch)
                    ),
                    "{rejection:?}"
                );
            }
            Err(err) => assert!(matches!(err, CommandError::SessionRevoked), "{err:?}"),
        }
    }
    Ok(())
}

#[tokio::test]
async fn refresh_past_the_ceiling_is_clamped() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let before = enrolled(&fixture, "ana").await?.identity;
    let ceiling = Duration::from_secs(7200);
    let commands = fixture
        .enroller
        .clone()
        .with_session_ceiling(SessionCeiling::new(ceiling)?);
    let start = UnixSeconds::now();
    let request = refresh_request(&fixture, &before, Duration::from_secs(30 * 24 * 3600))?;
    let refreshed = commands.refresh_session(&request).await?;
    let end = UnixSeconds::now();
    let expires_at = refreshed.identity.session_expires_at;
    assert!(expires_at >= start.saturating_add(ceiling) && expires_at <= end.saturating_add(ceiling));

    let key = AuthKey::session(&before.tenant, &before.sub, &before.sid);
    let (_, session) = fixture
        .issuer_store
        .read::<SessionRow>(&key)
        .await?
        .present()
        .ok_or("session row is missing")?;
    assert_eq!(session.expires_at, expires_at);
    assert!(authorize(&fixture, &refreshed.identity).await?.is_ok());
    Ok(())
}

#[tokio::test]
async fn unknown_connection_cannot_be_refreshed() -> TestResult {
    let fixture = fixture!(FixtureOptions::default());
    let before = enrolled(&fixture, "ana").await?.identity;
    let mut request = refresh_request(&fixture, &before, Duration::from_secs(7200))?;
    request.cid = trogon_presence::ConnectionId::generate()?;
    let refused = fixture.enroller.refresh_session(&request).await;
    assert!(matches!(refused, Err(CommandError::UnknownConnection)), "{refused:?}");
    Ok(())
}

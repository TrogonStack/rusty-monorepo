//! Auth read throughput: distinct enrolled users connecting through the callout over the plain
//! and websocket listeners at fixed client concurrency, then a reconnect wave of already
//! connected users that all reconnect within one second.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use tokio::time::Instant;
use trogon_presence_auth::ConnectIdentity;

use crate::common::{topics, BoxError, Fixture, FixtureOptions, Session, DEFAULT_SESSION, TENANT_A};
use crate::recorder::{CalloutWindow, Capture};
use crate::stats::{Samples, Table, Tally};

const USERS: usize = 256;
const LEVELS: [usize; 4] = [8, 32, 64, 128];
const WAVE_USERS: usize = 128;
const WAVE_SPREAD: Duration = Duration::from_secs(1);
const WAVE_TIMEOUT: Duration = Duration::from_secs(20);
const SETUP_CONCURRENCY: usize = 16;
const GCRA_REFILL: Duration = Duration::from_millis(1500);
const METRIC_SETTLE: Duration = Duration::from_millis(250);
const TOPIC: &str = "load.auth";
const BUSY: &str = "rate_limited:busy";
const RATE_LIMITED: &str = "rate_limited:rate_limited";
const ALLOWED: &str = "allowed";
const CONNECTED: &str = "connected";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Plain,
    Websocket,
}

impl Transport {
    const ALL: [Self; 2] = [Self::Plain, Self::Websocket];

    fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Websocket => "websocket",
        }
    }

    fn url(self, fixture: &Fixture) -> Result<String, BoxError> {
        match self {
            Self::Plain => Ok(fixture.url.clone()),
            Self::Websocket => Ok(crate::support::ws_url(fixture)?.to_owned()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Concurrency(NonZeroUsize);

impl Concurrency {
    fn of(value: usize) -> Result<Self, BoxError> {
        Ok(Self(
            NonZeroUsize::new(value).ok_or("concurrency must be at least one")?,
        ))
    }

    fn get(self) -> usize {
        self.0.get()
    }
}

type Attempt = (Duration, Result<Session, String>);

struct Run {
    transport: Transport,
    concurrency: Concurrency,
    wall: Duration,
    connect: Samples,
    clients: Tally,
    callout: CalloutWindow,
}

impl Run {
    fn admitted(&self) -> u64 {
        self.clients.get("ok")
    }

    fn per_second(&self) -> f64 {
        crate::stats::round(self.admitted() as f64 / self.wall.as_secs_f64().max(f64::EPSILON))
    }

    fn to_json(&self) -> Value {
        json!({
            "transport": self.transport.as_str(),
            "concurrency": self.concurrency.get(),
            "attempts": self.clients.total(),
            "wall_ms": crate::stats::round(self.wall.as_secs_f64() * 1000.0),
            "admitted_per_s": self.per_second(),
            "client_outcomes": self.clients.to_json(),
            "client_connect": self.connect.summary().to_json(),
            "callout_decisions": self.callout.decisions.to_json(),
            "callout_busy_share": crate::stats::round(self.callout.decisions.share(BUSY)),
            "callout_rate_limited_share": crate::stats::round(self.callout.decisions.share(RATE_LIMITED)),
            "callout_all": self.callout.all_durations().summary().to_json(),
            "callout_allowed": self.callout.durations_of(ALLOWED).summary().to_json(),
        })
    }

    fn row(&self, label: String) -> Vec<String> {
        let callout = self.callout.all_durations().summary();
        let mut row = vec![label, self.transport.as_str().to_owned()];
        row.extend(callout.cells());
        row.push(crate::stats::cell(self.connect.summary().p99));
        row.push(format!("{:.1}", self.per_second()));
        row.push(format!("{:.3}", self.callout.decisions.share(BUSY)));
        row.push(format!("{:.3}", self.callout.decisions.share(RATE_LIMITED)));
        row.push(self.callout.decisions.render());
        row
    }
}

fn options() -> FixtureOptions {
    FixtureOptions {
        websocket: true,
        ..FixtureOptions::default()
    }
}

async fn enroll(fixture: &Fixture, prefix: &str, count: usize) -> Result<Vec<ConnectIdentity>, BoxError> {
    let granted = topics(&[TOPIC])?;
    let enrolled: Vec<Result<ConnectIdentity, BoxError>> = stream::iter(0..count)
        .map(|index| {
            let granted = &granted;
            async move {
                let sub = format!("{prefix}{index:04}");
                let request = fixture.enroll_request(TENANT_A, &sub, "s1", granted, DEFAULT_SESSION)?;
                Ok(fixture.enroller.enroll(&request).await?.identity)
            }
        })
        .buffer_unordered(SETUP_CONCURRENCY)
        .collect()
        .await;
    enrolled.into_iter().collect()
}

async fn connect_all(
    fixture: &Fixture,
    url: &str,
    users: &[ConnectIdentity],
    concurrency: Concurrency,
) -> Result<(Vec<Attempt>, Duration), BoxError> {
    let started = Instant::now();
    let attempts: Vec<Result<Attempt, BoxError>> = stream::iter(users)
        .map(|identity| async move {
            let token = fixture.token(identity)?;
            let begun = Instant::now();
            let outcome = fixture.connect_to(url, identity, &token).await;
            Ok((begun.elapsed(), outcome.map_err(|err| format!("{:?}", err.kind()))))
        })
        .buffer_unordered(concurrency.get())
        .collect()
        .await;
    let wall = started.elapsed();
    Ok((attempts.into_iter().collect::<Result<_, _>>()?, wall))
}

async fn run(
    fixture: &Fixture,
    capture: &Capture,
    transport: Transport,
    users: &[ConnectIdentity],
    concurrency: Concurrency,
) -> Result<Run, BoxError> {
    let url = transport.url(fixture)?;
    capture.take();
    let (attempts, wall) = connect_all(fixture, &url, users, concurrency).await?;
    tokio::time::sleep(METRIC_SETTLE).await;
    let callout = capture.take();
    let mut connect = Samples::default();
    let mut clients = Tally::default();
    let mut sessions = Vec::new();
    for (elapsed, outcome) in attempts {
        connect.push(elapsed);
        match outcome {
            Ok(session) => {
                clients.bump("ok");
                sessions.push(session);
            }
            Err(kind) => clients.bump(kind),
        }
    }
    drop(sessions);
    Ok(Run {
        transport,
        concurrency,
        wall,
        connect,
        clients,
        callout,
    })
}

struct Wave {
    transport: Transport,
    users: usize,
    wall: Duration,
    reconnect: Samples,
    retried: usize,
    outcomes: Tally,
    callout: CalloutWindow,
}

impl Wave {
    fn reconnected(&self) -> u64 {
        self.outcomes.get("reconnected")
    }

    fn per_second(&self) -> f64 {
        crate::stats::round(self.reconnected() as f64 / self.wall.as_secs_f64().max(f64::EPSILON))
    }

    fn to_json(&self) -> Value {
        json!({
            "transport": self.transport.as_str(),
            "users": self.users,
            "spread_ms": WAVE_SPREAD.as_millis(),
            "wall_ms": crate::stats::round(self.wall.as_secs_f64() * 1000.0),
            "reconnected_per_s": self.per_second(),
            "outcomes": self.outcomes.to_json(),
            "clients_with_rejected_attempts": self.retried,
            "reconnect": self.reconnect.summary().to_json(),
            "callout_decisions": self.callout.decisions.to_json(),
            "callout_busy_share": crate::stats::round(self.callout.decisions.share(BUSY)),
            "callout_rate_limited_share": crate::stats::round(self.callout.decisions.share(RATE_LIMITED)),
            "callout_all": self.callout.all_durations().summary().to_json(),
        })
    }

    fn row(&self) -> Vec<String> {
        let mut row = vec![self.transport.as_str().to_owned(), self.users.to_string()];
        row.extend(self.reconnect.summary().cells());
        row.extend(self.callout.all_durations().summary().cells().into_iter().skip(1));
        row.push(format!("{:.1}", self.per_second()));
        row.push(self.retried.to_string());
        row.push(self.callout.decisions.render());
        row
    }
}

enum Reconnect {
    Done(Duration, bool),
    TimedOut(bool),
    Closed,
}

async fn reconnect_one(mut session: Session, at: Instant) -> (Reconnect, Session) {
    tokio::time::sleep_until(at).await;
    while session.events.try_recv().is_ok() {}
    let begun = Instant::now();
    if session.client.force_reconnect().await.is_err() {
        return (Reconnect::Closed, session);
    }
    let mut rejected = false;
    loop {
        match tokio::time::timeout_at(begun + WAVE_TIMEOUT, session.events.recv()).await {
            Ok(Some(event)) if event == CONNECTED => return (Reconnect::Done(begun.elapsed(), rejected), session),
            Ok(Some(event)) => rejected |= event.to_lowercase().contains("authorization"),
            Ok(None) => return (Reconnect::Closed, session),
            Err(_) => return (Reconnect::TimedOut(rejected), session),
        }
    }
}

async fn wave(
    fixture: &Fixture,
    capture: &Capture,
    transport: Transport,
    users: &[ConnectIdentity],
) -> Result<Wave, BoxError> {
    let url = transport.url(fixture)?;
    let (attempts, _) = connect_all(fixture, &url, users, Concurrency::of(SETUP_CONCURRENCY)?).await?;
    let sessions: Vec<Session> = attempts.into_iter().filter_map(|(_, outcome)| outcome.ok()).collect();
    let connected = sessions.len();
    tokio::time::sleep(GCRA_REFILL).await;
    capture.take();
    let started = Instant::now();
    let spacing = WAVE_SPREAD / u32::try_from(connected.max(1))?;
    let tasks: Vec<_> = sessions
        .into_iter()
        .enumerate()
        .map(|(index, session)| {
            let at = started + spacing * u32::try_from(index).unwrap_or(u32::MAX);
            tokio::spawn(reconnect_one(session, at))
        })
        .collect();
    let mut reconnect = Samples::default();
    let mut outcomes = Tally::default();
    let mut retried = 0;
    let mut kept = Vec::new();
    for task in tasks {
        let (outcome, session) = task.await?;
        kept.push(session);
        match outcome {
            Reconnect::Done(elapsed, rejected) => {
                reconnect.push(elapsed);
                outcomes.bump("reconnected");
                retried += usize::from(rejected);
            }
            Reconnect::TimedOut(rejected) => {
                outcomes.bump("timed_out");
                retried += usize::from(rejected);
            }
            Reconnect::Closed => outcomes.bump("closed"),
        }
    }
    let wall = started.elapsed();
    tokio::time::sleep(METRIC_SETTLE).await;
    let callout = capture.take();
    drop(kept);
    if connected < users.len() {
        outcomes.add("initial_connect_failed", u64::try_from(users.len() - connected)?);
    }
    Ok(Wave {
        transport,
        users: users.len(),
        wall,
        reconnect,
        retried,
        outcomes,
        callout,
    })
}

pub struct AuthReport {
    pub json: Value,
    pub tables: Vec<Table>,
}

pub async fn measure(capture: &Arc<Capture>, version: &mut Option<String>) -> Result<Option<AuthReport>, BoxError> {
    let Some(fixture) = Fixture::start(options()).await else {
        return Ok(None);
    };
    *version = Some(fixture.system.server_info().version.clone());
    let users = enroll(&fixture, "u", USERS).await?;
    let wave_users = enroll(&fixture, "w", WAVE_USERS * Transport::ALL.len()).await?;
    let mut runs = Vec::new();
    for transport in Transport::ALL {
        for level in LEVELS {
            tokio::time::sleep(GCRA_REFILL).await;
            runs.push(run(&fixture, capture, transport, &users, Concurrency::of(level)?).await?);
        }
    }
    let mut waves = Vec::new();
    for (transport, users) in Transport::ALL.into_iter().zip(wave_users.chunks(WAVE_USERS)) {
        waves.push(wave(&fixture, capture, transport, users).await?);
    }

    let mut connects = Table::new(
        format!("auth callout admission, {USERS} distinct users per row (latency in ms)"),
        &[
            "concurrency",
            "transport",
            "decisions",
            "callout p50",
            "callout p95",
            "callout p99",
            "callout max",
            "client p99",
            "admitted/s",
            "busy share",
            "rate-limit share",
            "callout decisions",
        ],
    );
    for run in &runs {
        connects.row(run.row(run.concurrency.get().to_string()));
    }
    let mut reconnects = Table::new(
        format!(
            "reconnect wave, every user reconnects within {} ms (latency in ms)",
            WAVE_SPREAD.as_millis()
        ),
        &[
            "transport",
            "users",
            "reconnected",
            "reconnect p50",
            "reconnect p95",
            "reconnect p99",
            "reconnect max",
            "callout p50",
            "callout p95",
            "callout p99",
            "callout max",
            "reconnected/s",
            "clients rejected first",
            "callout decisions",
        ],
    );
    for wave in &waves {
        reconnects.row(wave.row());
    }
    let json = json!({
        "users_per_run": USERS,
        "concurrency_levels": LEVELS,
        "callout_limits": "fixture defaults: GCRA 1/s burst 5 per user, 64 concurrent callouts, 1 s deadline",
        "runs": runs.iter().map(Run::to_json).collect::<Vec<_>>(),
        "reconnect_waves": waves.iter().map(Wave::to_json).collect::<Vec<_>>(),
    });
    Ok(Some(AuthReport {
        json,
        tables: vec![connects, reconnects],
    }))
}

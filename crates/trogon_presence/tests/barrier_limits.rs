mod common;

use std::sync::Arc;
use std::time::Duration;

use common::NatsServer;
use tokio::task::JoinSet;
use trogon_presence::{
    BarrierError, BarrierWaiters, EntryKey, EntryRevision, HolderId, Presence, PresenceConfig, ProvisionOptions,
    ReadBarrier, TopicWatch, UnixMillis,
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), BoxError>;

const PER_TOPIC: usize = 32;
const PER_PROCESS: usize = 128;
const SETTLE: Duration = Duration::from_secs(2);

fn unreachable_barrier(presence: &Presence, watch: &TopicWatch) -> Result<ReadBarrier, BoxError> {
    let entry = EntryKey::new(watch.topic().clone(), "ana".parse()?, HolderId::generate()?);
    let expires = UnixMillis::now().saturating_add(SETTLE * 5);
    Ok(ReadBarrier::new(
        presence.generation(),
        entry,
        EntryRevision::from(1_000_000),
        expires,
    ))
}

fn occupy(presence: &Presence, watch: &Arc<TopicWatch>, waiters: &mut JoinSet<Result<(), BarrierError>>) -> TestResult {
    for _ in 0..PER_TOPIC {
        let barrier = unreachable_barrier(presence, watch)?;
        let watch = Arc::clone(watch);
        waiters.spawn(async move { watch.read(&barrier).await.map(drop) });
    }
    Ok(())
}

async fn waiting_reaches(count: usize) -> TestResult {
    let deadline = tokio::time::Instant::now() + SETTLE;
    while BarrierWaiters::process().waiting() < count {
        if tokio::time::Instant::now() > deadline {
            return Err(format!("only {} waiters registered", BarrierWaiters::process().waiting()).into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[tokio::test]
async fn refuses_the_thirty_third_waiter_per_topic_and_the_hundred_twenty_ninth_per_process() -> TestResult {
    let Some(server) = NatsServer::start().await else {
        return Ok(());
    };
    let presence = Presence::provision(
        server.client().await,
        PresenceConfig::default(),
        ProvisionOptions::default(),
    )
    .await?;
    let mut watches = Vec::new();
    for index in 0..=PER_PROCESS / PER_TOPIC {
        watches.push(Arc::new(presence.watch(format!("room:{index}").parse()?).await?));
    }
    let mut waiters = JoinSet::new();

    occupy(&presence, &watches[0], &mut waiters)?;
    waiting_reaches(PER_TOPIC).await?;
    let refused = watches[0].read(&unreachable_barrier(&presence, &watches[0])?).await;
    assert_eq!(refused, Err(BarrierError::Overloaded));

    for watch in &watches[1..PER_PROCESS / PER_TOPIC] {
        occupy(&presence, watch, &mut waiters)?;
    }
    waiting_reaches(PER_PROCESS).await?;
    let last = &watches[PER_PROCESS / PER_TOPIC];
    let refused = last.read(&unreachable_barrier(&presence, last)?).await;
    assert_eq!(refused, Err(BarrierError::Overloaded));

    while let Some(result) = waiters.join_next().await {
        assert!(matches!(result?, Err(BarrierError::Expired { .. })));
    }
    assert_eq!(BarrierWaiters::process().waiting(), 0);
    Ok(())
}

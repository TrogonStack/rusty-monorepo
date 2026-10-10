#[allow(dead_code)]
#[path = "../../trogon_presence_service/tests/common/cluster.rs"]
mod r3;

use std::time::Duration;

use async_nats::jetstream::stream::StorageType;
use serde_json::{json, Value};
use trogon_presence_auth::{
    AuthBucket, AuthKey, AuthProvisionOptions, AuthStore, Row, Subject, TenantId, Write, WriteOutcome,
    AUTH_ROW_SCHEMA_V1, DEFAULT_AUTH_BUCKET,
};

use r3::{Cluster, ClusterError, Peer, Wait};

type TestResult = Result<(), ClusterError>;

const ROUNDS: u64 = 8;
const KILL_ROUND: u64 = 4;
const RESTART_ROUND: u64 = 6;
const COMMITTED: Wait = Wait::new("an auth write to commit", Duration::from_secs(60));
const READABLE: Wait = Wait::new("an auth read to succeed", Duration::from_secs(60));
const NEW_LEADER: Wait = Wait::new("a new auth stream leader", Duration::from_secs(60));
const REPLICAS_CURRENT: Wait = Wait::new("every auth replica to be current", Duration::from_secs(60));

macro_rules! cluster_or_skip {
    () => {
        match Cluster::start().await {
            Some(cluster) => cluster,
            None => return Ok(()),
        }
    };
}

fn version_of(row: Row<Value>) -> Option<u64> {
    row.present().and_then(|(_, value)| value["version"].as_u64())
}

async fn read_version(store: &AuthStore, key: &AuthKey) -> Result<Option<u64>, ClusterError> {
    READABLE
        .until(|| async { store.read::<Value>(key).await.ok().map(version_of) })
        .await
}

async fn commit_version(store: &AuthStore, key: &AuthKey, version: u64) -> TestResult {
    COMMITTED
        .until(|| async {
            let row = store.read::<Value>(key).await.ok()?;
            let expected = row.expected();
            if version_of(row) == Some(version) {
                return Some(());
            }
            let write = Write::put(
                key.clone(),
                &json!({ "v": AUTH_ROW_SCHEMA_V1, "version": version }),
                expected,
            )
            .ok()?;
            match store.commit(vec![write]).await.ok()? {
                WriteOutcome::Committed => Some(()),
                WriteOutcome::Conflict | WriteOutcome::Unknown | WriteOutcome::Rejected(_) => None,
            }
        })
        .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Health {
    AllNodes,
    ThirdNodeDown,
}

async fn move_leader(
    cluster: &Cluster,
    via: &async_nats::Client,
    stream: &str,
    round: u64,
    health: Health,
) -> TestResult {
    let previous = match health {
        Health::AllNodes => Cluster::wait_for_replicas(via, stream, REPLICAS_CURRENT).await?,
        Health::ThirdNodeDown => Cluster::wait_for_quorum(via, stream, REPLICAS_CURRENT).await?,
    };
    Cluster::step_down(via, stream).await?;
    let next = Cluster::wait_for_leader(via, stream, Some(previous), NEW_LEADER).await?;
    eprintln!(
        "round {round}: auth leader moved {previous} to {next} ({})",
        cluster.url(next)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "three-node cluster fault test, run through `mise run presence:cluster`"]
async fn a_follower_read_after_a_leader_change_never_returns_an_older_version() -> TestResult {
    let mut cluster = cluster_or_skip!();
    let writer_client = cluster.client(Peer::N1).await?;
    let reader_client = cluster.client(Peer::N2).await?;
    let bucket: AuthBucket = DEFAULT_AUTH_BUCKET.parse()?;
    let options = AuthProvisionOptions {
        replicas: 3,
        storage: StorageType::File,
    };
    let writer = AuthStore::provision(writer_client.clone(), bucket.clone(), &options).await?;
    let reader = AuthStore::open(reader_client.clone(), bucket.clone()).await?;
    let stream = bucket.stream_name();
    let config = writer.stream().cached_info().config.clone();
    assert!(!config.allow_direct, "auth reads must not be served by followers");
    assert!(!config.mirror_direct, "auth reads must not be served by mirrors");
    let key = AuthKey::policy(
        &TenantId::try_from("alpha".to_owned())?,
        &Subject::try_from("ana".to_owned())?,
    );

    for round in 1..=ROUNDS {
        commit_version(&writer, &key, round).await?;
        if round == KILL_ROUND {
            Wait::new("the auth leader to sit on the third node", Duration::from_secs(60))
                .until(|| async {
                    let leader = Cluster::wait_for_replicas(&reader_client, &stream, REPLICAS_CURRENT)
                        .await
                        .ok()?;
                    if leader == Peer::N3 {
                        return Some(());
                    }
                    let _ = Cluster::step_down(&reader_client, &stream).await;
                    None
                })
                .await?;
            cluster.stop(Peer::N3);
            let next = Cluster::wait_for_leader(&reader_client, &stream, Some(Peer::N3), NEW_LEADER).await?;
            eprintln!("round {round}: killed auth leader n3, {next} took over");
        } else if round == RESTART_ROUND {
            cluster.restart(Peer::N3).await?;
            move_leader(&cluster, &reader_client, &stream, round, Health::AllNodes).await?;
        } else if round > KILL_ROUND && round < RESTART_ROUND {
            move_leader(&cluster, &reader_client, &stream, round, Health::ThirdNodeDown).await?;
        } else {
            move_leader(&cluster, &reader_client, &stream, round, Health::AllNodes).await?;
        }
        assert_eq!(
            read_version(&reader, &key).await?,
            Some(round),
            "a read through a follower node returned an older version in round {round}"
        );
        assert_eq!(read_version(&writer, &key).await?, Some(round));
    }
    Ok(())
}

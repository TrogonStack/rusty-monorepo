//! Passive capture of the public frames one browser connection receives, written in the
//! conformance golden format so `replay.mjs` can drive the vendored Phoenix helper with it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use async_nats::{HeaderMap, Message};
use futures_util::stream::select_all;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use trogon_presence::{ConnectionId, PresenceKey, Presences, RequestId, ShardCount, Topic, ViewShard};
use trogon_presence_service::reply::{
    HEADER_CODE, HEADER_GENERATION, HEADER_KIND, HEADER_OWNER_ID, HEADER_OWNER_REV, HEADER_PART, HEADER_PARTS,
    HEADER_PREV, HEADER_REQUEST_ID, HEADER_SEQ, HEADER_SNAPSHOT_ID,
};
use trogon_presence_service::snapshot::{Assembly, AssemblyProgress, SnapshotFrame, SnapshotManifest};
use trogon_presence_service::subjects::{diff_subject, epoch_subject, snapshot_subject, SnapshotReplySubject};
use trogon_presence_service::{ReplyInbox, SnapshotLimits, CALLER_INBOX_PREFIX};

use crate::common::BoxError;

const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const SERVER_BINARY_ENV: &str = "TROGON_PRESENCE_NATS_SERVER";

fn header<'a>(headers: Option<&'a HeaderMap>, name: &str) -> Option<&'a str> {
    headers
        .and_then(|headers| headers.get(name))
        .map(|value| value.as_str())
}

fn number(message: &Message, name: &str) -> Result<u64, BoxError> {
    Ok(header(message.headers.as_ref(), name)
        .ok_or_else(|| format!("missing {name} on {}", message.subject))?
        .parse()?)
}

fn text(message: &Message, name: &str) -> Result<String, BoxError> {
    Ok(header(message.headers.as_ref(), name)
        .ok_or_else(|| format!("missing {name} on {}", message.subject))?
        .to_owned())
}

fn positioned(message: &Message, mut record: Value) -> Result<Value, BoxError> {
    record["generation"] = json!(text(message, HEADER_GENERATION)?);
    record["owner_rev"] = json!(number(message, HEADER_OWNER_REV)?);
    record["owner_id"] = json!(text(message, HEADER_OWNER_ID)?);
    Ok(record)
}

pub fn epoch_key(record: &Value) -> Value {
    json!([record["generation"], record["owner_rev"], record["owner_id"]])
}

enum Frame {
    Epoch(Value),
    Manifest { snapshot: String, record: Value },
    Part { snapshot: String, record: Value },
    Stream(Value),
    Refused,
}

struct Own {
    request: RequestId,
    assembly: Option<(Value, Assembly)>,
    stash: Vec<Message>,
}

/// Records what the reader's own connection receives: diffs, keepalives, epoch hints, and the
/// manifests and parts of the reader's snapshots. Frames arrive on several subscriptions, so a
/// part that overtakes its manifest is held until the manifest is recorded. A pump drains every
/// subscription as frames land so the capture keeps their arrival order.
pub struct Trace {
    client: async_nats::Client,
    shards: ShardCount,
    topic: Topic,
    key: PresenceKey,
    connection: ConnectionId,
    frames: mpsc::UnboundedReceiver<Message>,
    pump: JoinHandle<()>,
    records: Vec<Value>,
    manifests: HashSet<String>,
    held: HashMap<String, Vec<Value>>,
    own: Option<Own>,
}

impl Drop for Trace {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

impl Trace {
    pub async fn attach(
        client: async_nats::Client,
        key: PresenceKey,
        connection: ConnectionId,
        topic: Topic,
        shards: ShardCount,
    ) -> Result<Self, BoxError> {
        let diffs = client.subscribe(diff_subject(&topic)).await?;
        let epochs = client
            .subscribe(epoch_subject(shards, ViewShard::of(&topic, shards)))
            .await?;
        let parts = client
            .subscribe(SnapshotReplySubject::filter(&key, &connection))
            .await?;
        let replies = client
            .subscribe(ReplyInbox::connection_filter(&key, &connection))
            .await?;
        client.flush().await?;
        let (forward, frames) = mpsc::unbounded_channel();
        let mut merged = select_all([diffs, epochs, parts, replies]);
        let pump = tokio::spawn(async move {
            while let Some(message) = merged.next().await {
                if forward.send(message).is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            client,
            shards,
            topic,
            key,
            connection,
            frames,
            pump,
            records: Vec::new(),
            manifests: HashSet::new(),
            held: HashMap::new(),
            own: None,
        })
    }

    pub fn records(&self) -> &[Value] {
        &self.records
    }

    fn classify(message: &Message) -> Result<Frame, BoxError> {
        let headers = message.headers.as_ref();
        let subject = message.subject.as_str();
        if subject.starts_with("presence.v1.epoch.") {
            let record = json!({ "kind": "epoch", "payload": serde_json::from_slice::<Value>(&message.payload)? });
            return Ok(Frame::Epoch(positioned(message, record)?));
        }
        if subject.starts_with(CALLER_INBOX_PREFIX) {
            if header(headers, HEADER_CODE) != Some("ok") || header(headers, HEADER_KIND) != Some("manifest") {
                return Ok(Frame::Refused);
            }
            let payload: Value = serde_json::from_slice(&message.payload)?;
            let snapshot = payload["snapshot_id"]
                .as_str()
                .ok_or("manifest without snapshot_id")?
                .to_owned();
            let record = json!({ "kind": "manifest", "seq": number(message, HEADER_SEQ)?, "payload": payload });
            return Ok(Frame::Manifest {
                snapshot,
                record: positioned(message, record)?,
            });
        }
        let kind = header(headers, HEADER_KIND).ok_or("frame without Presence-Kind")?;
        if subject.starts_with("presence.v1.snapshot-reply.") {
            let snapshot = text(message, HEADER_SNAPSHOT_ID)?;
            let mut record = json!({
                "kind": kind,
                "seq": number(message, HEADER_SEQ)?,
                "snapshot_id": snapshot,
                "request_id": text(message, HEADER_REQUEST_ID)?,
            });
            if header(headers, HEADER_PART).is_some() {
                record["part"] = json!(number(message, HEADER_PART)?);
                record["payload"] = json!(message.payload.to_vec());
            }
            if header(headers, HEADER_PARTS).is_some() {
                record["parts"] = json!(number(message, HEADER_PARTS)?);
            }
            return Ok(Frame::Part {
                snapshot,
                record: positioned(message, record)?,
            });
        }
        let prev = match header(headers, HEADER_PREV) {
            Some(_) => Some(number(message, HEADER_PREV)?),
            None => None,
        };
        let record = json!({
            "kind": kind,
            "seq": number(message, HEADER_SEQ)?,
            "prev": prev,
            "payload": serde_json::from_slice::<Value>(&message.payload)?,
        });
        Ok(Frame::Stream(positioned(message, record)?))
    }

    fn requested_by_us(&self, message: &Message) -> bool {
        let Some(own) = &self.own else {
            return false;
        };
        let request = own.request.to_string();
        if message.subject.as_str().ends_with(&format!(".{request}")) {
            return true;
        }
        header(message.headers.as_ref(), HEADER_REQUEST_ID) == Some(request.as_str())
    }

    fn ingest(&mut self, message: Message) -> Result<Option<(Value, Presences)>, BoxError> {
        if self.requested_by_us(&message) {
            return self.feed_own(message);
        }
        match Self::classify(&message)? {
            Frame::Epoch(record) | Frame::Stream(record) => self.records.push(record),
            Frame::Manifest { snapshot, record } => {
                self.records.push(record);
                self.records.extend(self.held.remove(&snapshot).unwrap_or_default());
                self.manifests.insert(snapshot);
            }
            Frame::Part { snapshot, record } => {
                if self.manifests.contains(&snapshot) {
                    self.records.push(record);
                } else {
                    self.held.entry(snapshot).or_default().push(record);
                }
            }
            Frame::Refused => {}
        }
        Ok(None)
    }

    fn feed_own(&mut self, message: Message) -> Result<Option<(Value, Presences)>, BoxError> {
        let Some(own) = self.own.as_mut() else {
            return Ok(None);
        };
        if message.subject.as_str().starts_with(CALLER_INBOX_PREFIX) {
            let code = header(message.headers.as_ref(), HEADER_CODE);
            if code != Some("ok") {
                let body = String::from_utf8_lossy(&message.payload);
                return Err(format!("final snapshot replied {code:?}: {body}").into());
            }
            let manifest: SnapshotManifest = serde_json::from_slice(&message.payload)?;
            let record = match Self::classify(&message)? {
                Frame::Manifest { record, .. } => record,
                _ => return Err("final snapshot reply is not a manifest".into()),
            };
            own.assembly = Some((record, Assembly::begin(manifest, SnapshotLimits::default())?));
            for stashed in std::mem::take(&mut own.stash) {
                if let Some(done) = Self::accept(own, &stashed)? {
                    return Ok(Some(done));
                }
            }
            return Ok(None);
        }
        if own.assembly.is_none() {
            own.stash.push(message);
            return Ok(None);
        }
        Self::accept(own, &message)
    }

    fn accept(own: &mut Own, message: &Message) -> Result<Option<(Value, Presences)>, BoxError> {
        let Some((record, assembly)) = own.assembly.as_mut() else {
            return Ok(None);
        };
        match assembly.accept(SnapshotFrame::try_from(message)?)? {
            AssemblyProgress::Pending => Ok(None),
            AssemblyProgress::Complete(state) => Ok(Some((record.clone(), state))),
        }
    }

    async fn next(&mut self, timeout: Duration) -> Result<Option<Message>, BoxError> {
        match tokio::time::timeout(timeout, self.frames.recv()).await {
            Ok(Some(message)) => Ok(Some(message)),
            Ok(None) => Err("trace subscriptions ended".into()),
            Err(_) => Ok(None),
        }
    }

    /// Records every frame that arrives within the window.
    pub async fn settle(&mut self, window: Duration) -> Result<(), BoxError> {
        let deadline = tokio::time::Instant::now() + window;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            if let Some(message) = self.next(left).await? {
                self.ingest(message)?;
            }
        }
    }

    /// Asks the service for one more snapshot on the same connection, kept out of the capture,
    /// and returns its manifest record with the assembled state.
    pub async fn final_snapshot(&mut self) -> Result<(Value, Presences), BoxError> {
        let request = RequestId::generate()?;
        self.own = Some(Own {
            request,
            assembly: None,
            stash: Vec::new(),
        });
        let inbox = format!(
            "{CALLER_INBOX_PREFIX}.{}.{}.{request}",
            self.key.token(),
            self.connection
        );
        self.client
            .publish_with_reply(
                snapshot_subject(self.shards, &self.key, &self.connection, &self.topic),
                inbox,
                serde_json::to_vec(&json!({ "request_id": request }))?.into(),
            )
            .await?;
        self.client.flush().await?;
        loop {
            let message = self
                .next(FRAME_TIMEOUT)
                .await?
                .ok_or("final snapshot did not complete")?;
            if let Some(done) = self.ingest(message)? {
                self.own = None;
                return Ok(done);
            }
        }
    }

    /// Writes the capture with its scenario header and an expected record that carries the
    /// final snapshot's position and the Rust reader's state.
    pub fn write(
        mut self,
        scenario: &str,
        resyncs: usize,
        manifest: &Value,
        reader: &Presences,
        dir: &Path,
    ) -> Result<PathBuf, BoxError> {
        let mut leftovers: Vec<(String, Vec<Value>)> = self.held.drain().collect();
        leftovers.sort_by(|left, right| left.0.cmp(&right.0));
        self.records
            .extend(leftovers.into_iter().flat_map(|(_, records)| records));
        let mut expected = manifest.clone();
        expected["kind"] = json!("expected");
        expected["payload"] = serde_json::to_value(reader)?;
        let mut out = String::new();
        let header = json!({ "kind": "scenario", "name": scenario, "resyncs": resyncs });
        for record in std::iter::once(&header)
            .chain(&self.records)
            .chain(std::iter::once(&expected))
        {
            out.push_str(&serde_json::to_string(record)?);
            out.push('\n');
        }
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{scenario}.jsonl"));
        std::fs::write(&path, out)?;
        Ok(path)
    }
}

pub fn conformance_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("trogon_presence_service")
        .join("conformance")
}

fn node() -> Option<PathBuf> {
    let on_path = Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if on_path {
        return Some(PathBuf::from("node"));
    }
    let output = Command::new("mise").args(["which", "node"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(PathBuf::from(String::from_utf8(output.stdout).ok()?.trim()))
}

/// Replays one capture through `replay.mjs`. Without node the replay is skipped, except under
/// the pinned-server matrix, where a missing node is a failure.
pub fn replay(capture: &Path) -> Result<Option<String>, BoxError> {
    let Some(node) = node() else {
        if std::env::var_os(SERVER_BINARY_ENV).is_some() {
            return Err("node is required to replay the capture under the pinned-server matrix".into());
        }
        eprintln!("skipping the Phoenix replay: node is neither on PATH nor resolvable through mise");
        return Ok(None);
    };
    let dir = conformance_dir();
    let output = Command::new(node)
        .arg(dir.join("replay.mjs"))
        .arg(capture)
        .current_dir(&dir)
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("replay failed:\n{stdout}\n{stderr}").into());
    }
    Ok(Some(stdout))
}

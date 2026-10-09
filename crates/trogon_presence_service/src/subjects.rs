use std::fmt;

use trogon_presence::{
    ConnectionId, KeyError, PresenceKey, ShardCount, ShardError, SnapshotId, Topic, TopicError, ViewShard, WriterShard,
};

use crate::config::NodeId;
use crate::reply::ReplyCode;

pub const DOMAIN: &str = "presence.v1";
pub const QUEUE_GROUP: &str = "trogon-presence";
pub const ANY_SHARD: &str = "_";
pub const SNAPSHOT_REPLY_OP: &str = "snapshot-reply";
pub const INTERNAL_WRITE: &str = "internal.write";
pub const INTERNAL_REPLY: &str = "internal.reply";
pub const INTERNAL: &str = "internal";
pub const HEARTBEAT_MANY_OP: &str = "heartbeat-many";
pub const DIFF_OP: &str = "diff";
pub const EPOCH_OP: &str = "epoch";
pub const ANY_TOKEN: &str = "*";
pub const ANY_TAIL: &str = ">";
const SUBJECT_MAX_BYTES: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteOp {
    Track,
    Update,
    Untrack,
}

impl WriteOp {
    pub const ALL: [Self; 3] = [Self::Track, Self::Update, Self::Untrack];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Update => "update",
            Self::Untrack => "untrack",
        }
    }

    pub fn filter(self) -> String {
        format!("{DOMAIN}.{}.>", self.as_str())
    }

    pub fn subject(self, key: &PresenceKey, topic: &Topic) -> String {
        format!("{DOMAIN}.{}.{}.{}", self.as_str(), key.token(), topic.tokens())
    }

    pub fn prefix_grant(self, key: &PresenceKey, prefix: &Topic) -> String {
        format!("{}.{ANY_TAIL}", self.subject(key, prefix))
    }

    pub fn parse(self, subject: &str) -> Result<WriteTarget, SubjectError> {
        let rest = strip_op(subject, self.as_str())?;
        let (key_token, topic_tokens) = rest.split_once('.').ok_or(SubjectError::Malformed)?;
        let key = PresenceKey::from_token(key_token)?;
        let topic = Topic::from_tokens(topic_tokens)?;
        if key.token() != key_token || topic.tokens() != topic_tokens {
            return Err(SubjectError::NotCanonical);
        }
        Ok(WriteTarget { key, topic })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HolderOp {
    Heartbeat,
    Release,
}

impl HolderOp {
    pub const ALL: [Self; 2] = [Self::Heartbeat, Self::Release];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heartbeat => "heartbeat",
            Self::Release => "release",
        }
    }

    pub fn filter(self) -> String {
        format!("{DOMAIN}.{}.*", self.as_str())
    }

    pub fn subject(self, caller: &PresenceKey) -> String {
        format!("{DOMAIN}.{}.{}", self.as_str(), caller.token())
    }

    pub fn parse(self, subject: &str) -> Result<PresenceKey, SubjectError> {
        let token = strip_op(subject, self.as_str())?;
        let key = PresenceKey::from_token(token)?;
        if key.token() != token {
            return Err(SubjectError::NotCanonical);
        }
        Ok(key)
    }
}

pub fn internal_write_filter(shards: ShardCount, shard: WriterShard) -> String {
    format!("{DOMAIN}.{INTERNAL_WRITE}.{}.>", shards.token(shard))
}

pub fn internal_write_subject(shards: ShardCount, key: &PresenceKey, topic: Option<&Topic>) -> String {
    let shard = shards.token(WriterShard::of(key, shards));
    match topic {
        Some(topic) => format!("{DOMAIN}.{INTERNAL_WRITE}.{shard}.{}.{}", key.token(), topic.tokens()),
        None => format!("{DOMAIN}.{INTERNAL_WRITE}.{shard}.{}", key.token()),
    }
}

pub fn heartbeat_many_filter() -> String {
    format!("{DOMAIN}.{HEARTBEAT_MANY_OP}.*")
}

pub fn heartbeat_many_subject(node: &NodeId) -> String {
    format!("{DOMAIN}.{HEARTBEAT_MANY_OP}.{node}")
}

pub fn heartbeat_many_node(subject: &async_nats::Subject) -> Result<NodeId, SubjectError> {
    let node = strip_op(subject.as_str(), HEARTBEAT_MANY_OP)?;
    node.parse().map_err(|_| SubjectError::Malformed)
}

pub(crate) fn caller_token(subject: &str) -> Option<&str> {
    let rest = subject.strip_prefix(DOMAIN)?.strip_prefix('.')?;
    rest.split('.').nth(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadOp {
    List,
    Get,
}

impl ReadOp {
    pub const ALL: [Self; 2] = [Self::List, Self::Get];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Get => "get",
        }
    }

    pub fn shard_filter(self, shards: ShardCount, shard: ViewShard) -> String {
        format!("{DOMAIN}.{}.{}.>", self.as_str(), shards.token(shard))
    }

    pub fn any_shard_filter(self) -> String {
        format!("{DOMAIN}.{}.{ANY_SHARD}.>", self.as_str())
    }

    pub fn grant(self, caller: &PresenceKey, topic: &Topic) -> String {
        format!(
            "{DOMAIN}.{}.{ANY_TOKEN}.{}.{}",
            self.as_str(),
            caller.token(),
            topic.tokens()
        )
    }

    pub fn prefix_grant(self, caller: &PresenceKey, prefix: &Topic) -> String {
        format!("{}.{ANY_TAIL}", self.grant(caller, prefix))
    }

    pub fn subject(self, shards: ShardCount, caller: &PresenceKey, topic: &Topic) -> String {
        let shard = shards.token(ViewShard::of(topic, shards));
        format!(
            "{DOMAIN}.{}.{shard}.{}.{}",
            self.as_str(),
            caller.token(),
            topic.tokens()
        )
    }

    pub fn parse(self, subject: &str, shards: ShardCount) -> Result<ReadTarget, SubjectError> {
        let rest = strip_op(subject, self.as_str())?;
        let mut parts = rest.splitn(3, '.');
        let (Some(shard), Some(caller), Some(topic)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(SubjectError::Malformed);
        };
        let addressed = if shard == ANY_SHARD {
            None
        } else {
            Some(ViewShard::from(shards.parse_token(shard)?))
        };
        Ok(ReadTarget {
            addressed,
            caller: PresenceKey::from_token(caller)?,
            topic: Topic::from_tokens(topic)?,
        })
    }
}

fn strip_op<'a>(subject: &'a str, op: &str) -> Result<&'a str, SubjectError> {
    if subject.len() > SUBJECT_MAX_BYTES {
        return Err(SubjectError::TooLong);
    }
    subject
        .strip_prefix(DOMAIN)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_prefix(op))
        .and_then(|rest| rest.strip_prefix('.'))
        .ok_or(SubjectError::Malformed)
}

pub fn diff_subject(topic: &Topic) -> String {
    format!("{DOMAIN}.{DIFF_OP}.{}", topic.tokens())
}

pub fn diff_prefix_filter(prefix: &Topic) -> String {
    format!("{}.{ANY_TAIL}", diff_subject(prefix))
}

pub fn diff_any_filter() -> String {
    format!("{DOMAIN}.{DIFF_OP}.{ANY_TAIL}")
}

pub fn epoch_subject(shards: ShardCount, shard: ViewShard) -> String {
    format!("{DOMAIN}.{EPOCH_OP}.{}", shards.token(shard))
}

pub fn epoch_any_filter() -> String {
    format!("{DOMAIN}.{EPOCH_OP}.{ANY_TOKEN}")
}

/// The reply subject of one request the service sends to itself. It lives under the service's own
/// namespace so a runtime user granted only `presence.v1.>` can answer it, unlike the client's
/// default `_INBOX` mux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalReplyInbox(String);

impl InternalReplyInbox {
    pub fn generate() -> Result<Self, getrandom::Error> {
        let high = getrandom::u64()?;
        let low = getrandom::u64()?;
        Ok(Self(format!("{DOMAIN}.{INTERNAL_REPLY}.{high:016x}{low:016x}")))
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for InternalReplyInbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn internal_any_filter() -> String {
    format!("{DOMAIN}.{INTERNAL}.{ANY_TAIL}")
}

pub fn snapshot_reply_any_filter() -> String {
    format!("{DOMAIN}.{SNAPSHOT_REPLY_OP}.{ANY_TAIL}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteTarget {
    key: PresenceKey,
    topic: Topic,
}

impl WriteTarget {
    pub fn new(key: PresenceKey, topic: Topic) -> Self {
        Self { key, topic }
    }

    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn topic(&self) -> &Topic {
        &self.topic
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadTarget {
    addressed: Option<ViewShard>,
    caller: PresenceKey,
    topic: Topic,
}

impl ReadTarget {
    pub fn addressed(&self) -> Option<ViewShard> {
        self.addressed
    }

    pub fn caller(&self) -> &PresenceKey {
        &self.caller
    }

    pub fn topic(&self) -> &Topic {
        &self.topic
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubjectError {
    #[error("subject does not match the operation layout")]
    Malformed,
    #[error("subject exceeds {SUBJECT_MAX_BYTES} bytes")]
    TooLong,
    #[error("subject tokens are not in canonical form")]
    NotCanonical,
    #[error(transparent)]
    Shard(#[from] ShardError),
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error(transparent)]
    Topic(#[from] TopicError),
}

impl SubjectError {
    pub fn code(&self) -> ReplyCode {
        match self {
            Self::Malformed | Self::NotCanonical | Self::Shard(_) => ReplyCode::InvalidRequest,
            Self::TooLong => ReplyCode::TopicTooLong,
            Self::Key(err) => err.code().into(),
            Self::Topic(err) => err.code().into(),
        }
    }
}

pub const SNAPSHOT_OP: &str = "snapshot";

pub fn snapshot_shard_filter(shards: ShardCount, shard: ViewShard) -> String {
    format!("{DOMAIN}.{SNAPSHOT_OP}.{}.>", shards.token(shard))
}

pub fn snapshot_any_shard_filter() -> String {
    format!("{DOMAIN}.{SNAPSHOT_OP}.{ANY_SHARD}.>")
}

pub fn snapshot_subject(shards: ShardCount, caller: &PresenceKey, connection: &ConnectionId, topic: &Topic) -> String {
    let shard = shards.token(ViewShard::of(topic, shards));
    format!(
        "{DOMAIN}.{SNAPSHOT_OP}.{shard}.{}.{connection}.{}",
        caller.token(),
        topic.tokens()
    )
}

pub fn snapshot_grant(caller: &PresenceKey, connection: &ConnectionId, topic: &Topic) -> String {
    format!(
        "{DOMAIN}.{SNAPSHOT_OP}.{ANY_TOKEN}.{}.{connection}.{}",
        caller.token(),
        topic.tokens()
    )
}

pub fn snapshot_prefix_grant(caller: &PresenceKey, connection: &ConnectionId, prefix: &Topic) -> String {
    format!("{}.{ANY_TAIL}", snapshot_grant(caller, connection, prefix))
}

pub(crate) fn snapshot_scope(subject: &str) -> Option<(&str, &str)> {
    let rest = subject
        .strip_prefix(DOMAIN)?
        .strip_prefix('.')?
        .strip_prefix(SNAPSHOT_OP)?
        .strip_prefix('.')?;
    let mut parts = rest.split('.').skip(1);
    Some((parts.next()?, parts.next()?))
}

pub fn parse_snapshot(subject: &str, shards: ShardCount) -> Result<SnapshotTarget, SubjectError> {
    let rest = strip_op(subject, SNAPSHOT_OP)?;
    let mut parts = rest.splitn(4, '.');
    let (Some(shard), Some(caller), Some(connection), Some(topic)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(SubjectError::Malformed);
    };
    let addressed = if shard == ANY_SHARD {
        None
    } else {
        Some(ViewShard::from(shards.parse_token(shard)?))
    };
    let connection: ConnectionId = connection.parse().map_err(|_| SubjectError::Malformed)?;
    Ok(SnapshotTarget {
        read: ReadTarget {
            addressed,
            caller: PresenceKey::from_token(caller)?,
            topic: Topic::from_tokens(topic)?,
        },
        connection,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotTarget {
    read: ReadTarget,
    connection: ConnectionId,
}

impl SnapshotTarget {
    pub fn read(&self) -> &ReadTarget {
        &self.read
    }

    pub fn connection(&self) -> &ConnectionId {
        &self.connection
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SnapshotReplySubject(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("snapshot reply subject {0:?} must be {DOMAIN}.{SNAPSHOT_REPLY_OP}.<key>.<connection>.<snapshot>")]
pub struct SnapshotReplySubjectError(String);

impl SnapshotReplySubject {
    pub fn new(caller: &PresenceKey, connection: &ConnectionId, snapshot: &SnapshotId) -> Self {
        Self(format!(
            "{DOMAIN}.{SNAPSHOT_REPLY_OP}.{}.{connection}.{snapshot}",
            caller.token()
        ))
    }

    pub fn filter(caller: &PresenceKey, connection: &ConnectionId) -> String {
        format!("{DOMAIN}.{SNAPSHOT_REPLY_OP}.{}.{connection}.*", caller.token())
    }

    pub fn snapshot(&self) -> Result<SnapshotId, SnapshotReplySubjectError> {
        self.0
            .rsplit_once('.')
            .and_then(|(_, token)| token.parse().ok())
            .ok_or_else(|| SnapshotReplySubjectError(self.0.clone()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&async_nats::Subject> for SnapshotReplySubject {
    type Error = SnapshotReplySubjectError;

    fn try_from(subject: &async_nats::Subject) -> Result<Self, Self::Error> {
        let raw = subject.as_str();
        let invalid = || SnapshotReplySubjectError(raw.to_owned());
        let rest = strip_op(raw, SNAPSHOT_REPLY_OP).map_err(|_| invalid())?;
        let mut parts = rest.split('.');
        let (Some(caller), Some(connection), Some(snapshot), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid());
        };
        let caller = PresenceKey::from_token(caller).map_err(|_| invalid())?;
        let connection: ConnectionId = connection.parse().map_err(|_| invalid())?;
        let snapshot: SnapshotId = snapshot.parse().map_err(|_| invalid())?;
        let canonical = Self::new(&caller, &connection, &snapshot);
        if canonical.0 == raw {
            Ok(canonical)
        } else {
            Err(invalid())
        }
    }
}

impl fmt::Display for SnapshotReplySubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn internal_reply_inboxes_are_distinct_and_inside_the_internal_namespace() -> Result<(), getrandom::Error> {
        let first = InternalReplyInbox::generate()?.into_string();
        let second = InternalReplyInbox::generate()?.into_string();
        assert_ne!(first, second);
        let namespace = internal_any_filter();
        let prefix = namespace.trim_end_matches('>');
        for inbox in [&first, &second] {
            assert!(inbox.starts_with(&format!("{DOMAIN}.{INTERNAL_REPLY}.")), "{inbox}");
            assert!(inbox.starts_with(prefix), "{inbox} is outside {namespace}");
            assert_eq!(inbox.split('.').count(), DOMAIN.split('.').count() + 3, "{inbox}");
        }
        Ok(())
    }

    #[test]
    fn builds_and_parses_read_subjects() -> TestResult {
        let shards = ShardCount::DEFAULT;
        let topic: Topic = "room:lobby".parse()?;
        let caller: PresenceKey = "ana@x.io".parse()?;
        let subject = ReadOp::List.subject(shards, &caller, &topic);
        assert_eq!(subject, "presence.v1.list.s56.ana=40x=2Eio.room.lobby");
        let target = ReadOp::List.parse(&subject, shards)?;
        assert_eq!(target.addressed(), Some(ViewShard::from(shards.shard(56)?)));
        assert_eq!(target.caller(), &caller);
        assert_eq!(target.topic(), &topic);
        let any = ReadOp::Get.parse("presence.v1.get._.ana.room.lobby", shards)?;
        assert_eq!(any.addressed(), None);
        assert_eq!(
            ReadOp::Get.shard_filter(shards, ViewShard::from(shards.shard(3)?)),
            "presence.v1.get.s03.>"
        );
        Ok(())
    }

    #[test]
    fn maps_bad_read_subjects_to_codes() {
        let shards = ShardCount::DEFAULT;
        let code = |subject: &str| ReadOp::List.parse(subject, shards).map_err(|err| err.code());
        assert_eq!(code("presence.v1.list.s56.ana"), Err(ReplyCode::InvalidRequest));
        assert_eq!(code("presence.v1.list.s99.ana.room"), Err(ReplyCode::InvalidRequest));
        assert_eq!(code("presence.v1.list.s56.ana.room..x"), Err(ReplyCode::InvalidTopic));
        let long_key = "a".repeat(800);
        assert_eq!(
            code(&format!("presence.v1.list.s56.{long_key}.room")),
            Err(ReplyCode::KeyTooLong)
        );
        assert_eq!(code("presence.v1.get.s56.ana.room"), Err(ReplyCode::InvalidRequest));
    }

    #[test]
    fn builds_and_parses_write_subjects() -> TestResult {
        let topic: Topic = "room:lobby".parse()?;
        let key: PresenceKey = "carol".parse()?;
        let subject = WriteOp::Track.subject(&key, &topic);
        assert_eq!(subject, "presence.v1.track.carol.room.lobby");
        let target = WriteOp::Track.parse(&subject)?;
        assert_eq!(target.key(), &key);
        assert_eq!(target.topic(), &topic);
        assert!(WriteOp::Update.parse(&subject).is_err());
        assert_eq!(diff_subject(&topic), "presence.v1.diff.room.lobby");
        Ok(())
    }

    #[test]
    fn builds_and_parses_holder_subjects() -> TestResult {
        let caller: PresenceKey = "ana@x.io".parse()?;
        let subject = HolderOp::Heartbeat.subject(&caller);
        assert_eq!(subject, "presence.v1.heartbeat.ana=40x=2Eio");
        assert_eq!(HolderOp::Heartbeat.parse(&subject)?, caller);
        assert_eq!(HolderOp::Release.filter(), "presence.v1.release.*");
        assert!(HolderOp::Release.parse(&subject).is_err());
        assert_eq!(caller_token(&subject), Some("ana=40x=2Eio"));
        assert_eq!(caller_token("presence.v1.track.carol.room.lobby"), Some("carol"));
        Ok(())
    }

    #[test]
    fn builds_internal_write_subjects() -> TestResult {
        let shards = ShardCount::DEFAULT;
        let key: PresenceKey = "carol".parse()?;
        let topic: Topic = "room:lobby".parse()?;
        let shard = shards.token(WriterShard::of(&key, shards));
        assert_eq!(
            internal_write_subject(shards, &key, Some(&topic)),
            format!("presence.v1.internal.write.{shard}.carol.room.lobby")
        );
        assert_eq!(
            internal_write_subject(shards, &key, None),
            format!("presence.v1.internal.write.{shard}.carol")
        );
        assert_eq!(
            internal_write_filter(shards, WriterShard::of(&key, shards)),
            format!("presence.v1.internal.write.{shard}.>")
        );
        let node: NodeId = "edge-1".parse()?;
        let subject = async_nats::Subject::from(heartbeat_many_subject(&node));
        assert_eq!(subject.as_str(), "presence.v1.heartbeat-many.edge-1");
        assert_eq!(heartbeat_many_node(&subject)?, node);
        Ok(())
    }

    #[test]
    fn rejects_non_canonical_write_subjects() {
        assert_eq!(
            WriteOp::Track.parse("presence.v1.track.carol.room.lobby").map(|_| ()),
            Ok(())
        );
        assert!(WriteOp::Track.parse("presence.v1.track.=63arol.room.lobby").is_err());
    }

    #[test]
    fn builds_and_parses_snapshot_subjects() -> TestResult {
        let shards = ShardCount::DEFAULT;
        let topic: Topic = "room:lobby".parse()?;
        let caller: PresenceKey = "ana@x.io".parse()?;
        let connection = ConnectionId::from([3; 16]);
        let subject = snapshot_subject(shards, &caller, &connection, &topic);
        assert_eq!(
            subject,
            format!("presence.v1.snapshot.s56.ana=40x=2Eio.{connection}.room.lobby")
        );
        let target = parse_snapshot(&subject, shards)?;
        assert_eq!(target.connection(), &connection);
        assert_eq!(target.read().caller(), &caller);
        assert_eq!(target.read().topic(), &topic);
        assert!(parse_snapshot("presence.v1.snapshot.s56.ana.notanid.room", shards).is_err());
        assert_eq!(snapshot_any_shard_filter(), "presence.v1.snapshot._.>");
        Ok(())
    }

    #[test]
    fn validates_snapshot_reply_subjects() -> TestResult {
        let caller: PresenceKey = "ana@x.io".parse()?;
        let connection = ConnectionId::from([3; 16]);
        let snapshot = SnapshotId::from([4; 16]);
        let reply = SnapshotReplySubject::new(&caller, &connection, &snapshot);
        let parsed = SnapshotReplySubject::try_from(&async_nats::Subject::from(reply.as_str()))?;
        assert_eq!(parsed, reply);
        assert_eq!(parsed.snapshot()?, snapshot);
        for bad in [
            format!("presence.v1.snapshot-reply.ana=40x=2Eio.{connection}"),
            format!("presence.v1.snapshot-reply.ana=40x=2Eio.{connection}.{snapshot}.x"),
            format!("presence.v1.snapshot-reply.ana=40x=2Eio.edge-1.{snapshot}"),
            "presence.v1.snapshot-reply.>".to_owned(),
            "presence.v1.diff.room.lobby".to_owned(),
        ] {
            assert!(
                SnapshotReplySubject::try_from(&async_nats::Subject::from(bad.as_str())).is_err(),
                "{bad}"
            );
        }
        Ok(())
    }
}

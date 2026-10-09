use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::Serialize;
use trogon_presence::{ConnectionId, ShardCount, Topic, TopicError, ViewShard};
use trogon_presence_service::subjects::{
    diff_any_filter, diff_prefix_filter, diff_subject, epoch_any_filter, epoch_subject, heartbeat_many_filter,
    internal_write_any_filter, snapshot_grant, snapshot_prefix_grant, snapshot_reply_any_filter, HolderOp, ReadOp,
    SnapshotReplySubject, WriteOp,
};
use trogon_presence_service::ReplyInbox;

use crate::claims::Subject;

pub const DEFAULT_MAX_PRIVATE_TOPICS: usize = 128;
pub const DEFAULT_RESPONSE_BUDGET_BYTES: usize = 900 * 1024;
pub const RESPONSE_OVERHEAD_BYTES: usize = 512;

const API_DENY: [&str; 3] = ["$JS.>", "$KV.>", "$SYS.>"];
const PLAIN_INBOX_DENY: &str = "_INBOX.>";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PublicPrefix(Topic);

impl PublicPrefix {
    pub fn topic(&self) -> &Topic {
        &self.0
    }

    pub fn covers(&self, topic: &Topic) -> bool {
        topic
            .tokens()
            .strip_prefix(self.0.tokens())
            .is_some_and(|rest| rest.starts_with('.'))
    }
}

impl FromStr for PublicPrefix {
    type Err = TopicError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse()?))
    }
}

impl fmt::Display for PublicPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivateTopicCap(usize);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("private topic cap must be 1 to {DEFAULT_MAX_PRIVATE_TOPICS}")]
pub struct PrivateTopicCapError;

impl PrivateTopicCap {
    pub fn new(cap: usize) -> Result<Self, PrivateTopicCapError> {
        if cap == 0 || cap > DEFAULT_MAX_PRIVATE_TOPICS {
            return Err(PrivateTopicCapError);
        }
        Ok(Self(cap))
    }

    pub fn get(self) -> usize {
        self.0
    }
}

impl Default for PrivateTopicCap {
    fn default() -> Self {
        Self(DEFAULT_MAX_PRIVATE_TOPICS)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponseBudget(usize);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("response budget must be 1 to {DEFAULT_RESPONSE_BUDGET_BYTES} bytes")]
pub struct ResponseBudgetError;

impl ResponseBudget {
    pub fn bytes(bytes: usize) -> Result<Self, ResponseBudgetError> {
        if bytes == 0 || bytes > DEFAULT_RESPONSE_BUDGET_BYTES {
            return Err(ResponseBudgetError);
        }
        Ok(Self(bytes))
    }

    pub fn get(self) -> usize {
        self.0
    }

    pub fn effective(self, max_payload: usize) -> usize {
        self.0.min(max_payload.saturating_sub(RESPONSE_OVERHEAD_BYTES))
    }
}

impl Default for ResponseBudget {
    fn default() -> Self {
        Self(DEFAULT_RESPONSE_BUDGET_BYTES)
    }
}

#[derive(Debug, Clone, Default)]
pub struct GrantPolicy {
    pub public_prefixes: Vec<PublicPrefix>,
    pub max_private_topics: PrivateTopicCap,
    pub response_budget: ResponseBudget,
    pub shards: ShardCount,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantError {
    #[error("{count} private topics exceed the cap of {cap}")]
    TooManyTopics { count: usize, cap: usize },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SubjectRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Permissions {
    #[serde(rename = "pub")]
    pub publish: SubjectRules,
    #[serde(rename = "sub")]
    pub subscribe: SubjectRules,
}

impl Permissions {
    pub fn subject_count(&self) -> usize {
        self.publish.allow.len() + self.publish.deny.len() + self.subscribe.allow.len() + self.subscribe.deny.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantPlan {
    pub granted: Vec<Topic>,
    pub public: Vec<Topic>,
    pub permissions: Permissions,
}

pub struct GrantRequest<'a> {
    pub sub: &'a Subject,
    pub connection: &'a ConnectionId,
    pub topics: &'a [Topic],
}

impl GrantPolicy {
    pub fn plan(&self, request: &GrantRequest<'_>) -> Result<GrantPlan, GrantError> {
        let key = request.sub.key();
        let connection = request.connection;
        let mut seen = BTreeSet::new();
        let mut granted = Vec::new();
        let mut public = Vec::new();
        for topic in request.topics {
            if !seen.insert(topic.tokens()) {
                continue;
            }
            if self.public_prefixes.iter().any(|prefix| prefix.covers(topic)) {
                public.push(topic.clone());
            } else {
                granted.push(topic.clone());
            }
        }
        if granted.len() > self.max_private_topics.get() {
            return Err(GrantError::TooManyTopics {
                count: granted.len(),
                cap: self.max_private_topics.get(),
            });
        }

        let mut permissions = Permissions::default();
        let publish = &mut permissions.publish.allow;
        let subscribe = &mut permissions.subscribe.allow;
        publish.extend(HolderOp::ALL.iter().map(|op| op.subject(key)));
        subscribe.push(ReplyInbox::connection_filter(key, connection));
        subscribe.push(SnapshotReplySubject::filter(key, connection));
        if self.public_prefixes.is_empty() {
            let shards: BTreeSet<String> = granted
                .iter()
                .map(|topic| epoch_subject(self.shards, ViewShard::of(topic, self.shards)))
                .collect();
            subscribe.extend(shards);
        } else {
            subscribe.push(epoch_any_filter());
        }
        for topic in &granted {
            publish.extend(WriteOp::ALL.iter().map(|op| op.subject(key, topic)));
            publish.extend(ReadOp::ALL.iter().map(|op| op.grant(key, topic)));
            publish.push(snapshot_grant(key, connection, topic));
            subscribe.push(diff_subject(topic));
        }
        for prefix in &self.public_prefixes {
            let topic = prefix.topic();
            publish.extend(WriteOp::ALL.iter().map(|op| op.prefix_grant(key, topic)));
            publish.extend(ReadOp::ALL.iter().map(|op| op.prefix_grant(key, topic)));
            publish.push(snapshot_prefix_grant(key, connection, topic));
            subscribe.push(diff_prefix_filter(topic));
        }
        permissions.publish.deny = API_DENY
            .iter()
            .map(|subject| (*subject).to_owned())
            .chain([
                diff_any_filter(),
                epoch_any_filter(),
                snapshot_reply_any_filter(),
                internal_write_any_filter(),
                heartbeat_many_filter(),
                PLAIN_INBOX_DENY.to_owned(),
            ])
            .collect();
        permissions.subscribe.deny = API_DENY
            .iter()
            .map(|subject| (*subject).to_owned())
            .chain([
                internal_write_any_filter(),
                heartbeat_many_filter(),
                PLAIN_INBOX_DENY.to_owned(),
            ])
            .collect();
        Ok(GrantPlan {
            granted,
            public,
            permissions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn sub(raw: &str) -> Result<Subject, Box<dyn std::error::Error>> {
        Ok(Subject::try_from(raw.to_owned())?)
    }

    fn topics(raw: &[&str]) -> Result<Vec<Topic>, TopicError> {
        raw.iter().map(|t| t.parse()).collect()
    }

    fn plan(
        policy: &GrantPolicy,
        sub: &Subject,
        topics: &[Topic],
    ) -> Result<(ConnectionId, GrantPlan), Box<dyn std::error::Error>> {
        let connection = ConnectionId::generate()?;
        let plan = policy.plan(&GrantRequest {
            sub,
            connection: &connection,
            topics,
        })?;
        Ok((connection, plan))
    }

    #[test]
    fn emits_exactly_the_private_topic_table() -> TestResult {
        let ana = sub("ana@x.io")?;
        let (c, plan) = plan(&GrantPolicy::default(), &ana, &topics(&["room:jos\u{e9}"])?)?;
        let k = "ana=40x=2Eio";
        let t = "room.jos=C3=A9";
        assert_eq!(
            plan.permissions.publish.allow,
            [
                format!("presence.v1.heartbeat.{k}"),
                format!("presence.v1.release.{k}"),
                format!("presence.v1.track.{k}.{t}"),
                format!("presence.v1.update.{k}.{t}"),
                format!("presence.v1.untrack.{k}.{t}"),
                format!("presence.v1.list.*.{k}.{t}"),
                format!("presence.v1.get.*.{k}.{t}"),
                format!("presence.v1.snapshot.*.{k}.{c}.{t}"),
            ]
        );
        let epoch = epoch_subject(
            ShardCount::default(),
            ViewShard::of(&"room:jos\u{e9}".parse()?, ShardCount::default()),
        );
        assert_eq!(
            plan.permissions.subscribe.allow,
            [
                format!("_INBOX_U.{k}.{c}.>"),
                format!("presence.v1.snapshot-reply.{k}.{c}.*"),
                epoch,
                format!("presence.v1.diff.{t}"),
            ]
        );
        let deny = &plan.permissions.publish.deny;
        for subject in [
            "presence.v1.diff.>",
            "presence.v1.epoch.*",
            "presence.v1.snapshot-reply.>",
            "presence.v1.internal.write.>",
            "presence.v1.heartbeat-many.*",
            "$JS.>",
        ] {
            assert!(deny.contains(&subject.to_owned()), "{subject}");
        }
        assert!(!deny.iter().any(|s| s.starts_with("presence.v1.snapshot.")));
        Ok(())
    }

    #[test]
    fn public_prefixes_cover_children_but_not_the_bare_prefix() -> TestResult {
        let policy = GrantPolicy {
            public_prefixes: vec!["room".parse()?],
            ..GrantPolicy::default()
        };
        let ana = sub("ana")?;
        let (c, plan) = plan(&policy, &ana, &topics(&["room:lobby", "room", "dm:1"])?)?;
        assert_eq!(plan.public, topics(&["room:lobby"])?);
        assert_eq!(plan.granted, topics(&["room", "dm:1"])?);
        let allow = &plan.permissions.publish.allow;
        assert!(allow.contains(&"presence.v1.track.ana.room.>".to_owned()));
        assert!(allow.contains(&"presence.v1.list.*.ana.room.>".to_owned()));
        assert!(allow.contains(&format!("presence.v1.snapshot.*.ana.{c}.room.>")));
        assert!(allow.contains(&"presence.v1.track.ana.room".to_owned()));
        assert!(plan
            .permissions
            .subscribe
            .allow
            .contains(&"presence.v1.diff.room.>".to_owned()));
        assert!(GrantPolicy::default().public_prefixes.is_empty());
        Ok(())
    }

    #[test]
    fn refuses_more_private_topics_than_the_cap() -> TestResult {
        let many: Vec<Topic> = (0..129).map(|i| format!("dm:{i}").parse()).collect::<Result<_, _>>()?;
        let connection = ConnectionId::generate()?;
        let ana = sub("ana")?;
        let request = GrantRequest {
            sub: &ana,
            connection: &connection,
            topics: &many,
        };
        assert_eq!(
            GrantPolicy::default().plan(&request),
            Err(GrantError::TooManyTopics { count: 129, cap: 128 })
        );
        let fits = GrantRequest {
            topics: &many[..128],
            ..request
        };
        assert_eq!(GrantPolicy::default().plan(&fits)?.granted.len(), 128);
        assert!(PrivateTopicCap::new(129).is_err());
        Ok(())
    }

    #[test]
    fn budget_is_bounded_by_max_payload() -> TestResult {
        let budget = ResponseBudget::default();
        assert_eq!(budget.effective(1024 * 1024), DEFAULT_RESPONSE_BUDGET_BYTES);
        assert_eq!(budget.effective(4096), 4096 - RESPONSE_OVERHEAD_BYTES);
        assert!(ResponseBudget::bytes(DEFAULT_RESPONSE_BUDGET_BYTES + 1).is_err());
        Ok(())
    }

    #[test]
    fn deduplicates_topics() -> TestResult {
        let (_, plan) = plan(&GrantPolicy::default(), &sub("ana")?, &topics(&["dm:1", "dm:1"])?)?;
        assert_eq!(plan.granted.len(), 1);
        Ok(())
    }
}

use std::fmt;

use async_nats::Subject;
use trogon_presence::{ConnectionId, PresenceKey};

pub const CALLER_INBOX_PREFIX: &str = "_INBOX_U";
const INBOX_MIN_TAIL_TOKENS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyInbox(Subject);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("reply inbox {0:?} is not under {CALLER_INBOX_PREFIX}.<key>.<client>.>")]
pub struct ReplyInboxError(String);

impl ReplyInbox {
    pub fn scoped(key: &PresenceKey, subject: Subject) -> Result<Self, ReplyInboxError> {
        Self::under_token(key.token(), subject)
    }

    pub fn for_connection(
        key: &PresenceKey,
        connection: &ConnectionId,
        subject: Subject,
    ) -> Result<Self, ReplyInboxError> {
        Self::under_connection_token(key.token(), &connection.to_string(), subject)
    }

    pub(crate) fn under_connection_token(
        key_token: &str,
        connection_token: &str,
        subject: Subject,
    ) -> Result<Self, ReplyInboxError> {
        let valid = !connection_token.is_empty()
            && tail_after(subject.as_str(), key_token).is_some_and(|tail| {
                tail.split_once('.')
                    .is_some_and(|(token, rest)| token == connection_token && valid_tokens(rest, 1))
            });
        if valid {
            Ok(Self(subject))
        } else {
            Err(ReplyInboxError(subject.to_string()))
        }
    }

    pub(crate) fn under_token(key_token: &str, subject: Subject) -> Result<Self, ReplyInboxError> {
        let valid =
            tail_after(subject.as_str(), key_token).is_some_and(|tail| valid_tokens(tail, INBOX_MIN_TAIL_TOKENS));
        if valid {
            Ok(Self(subject))
        } else {
            Err(ReplyInboxError(subject.to_string()))
        }
    }

    pub fn connection_filter(key: &PresenceKey, connection: &ConnectionId) -> String {
        format!("{CALLER_INBOX_PREFIX}.{}.{connection}.>", key.token())
    }

    pub fn subject(&self) -> &Subject {
        &self.0
    }

    pub fn into_subject(self) -> Subject {
        self.0
    }
}

fn tail_after<'a>(subject: &'a str, key_token: &str) -> Option<&'a str> {
    if key_token.is_empty() {
        return None;
    }
    subject
        .strip_prefix(CALLER_INBOX_PREFIX)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_prefix(key_token))
        .and_then(|rest| rest.strip_prefix('.'))
}

fn valid_tokens(tail: &str, minimum: usize) -> bool {
    let tokens: Vec<&str> = tail.split('.').collect();
    tokens.len() >= minimum
        && tokens
            .iter()
            .all(|token| !token.is_empty() && !token.contains(['*', '>', ' ', '\t', '\r', '\n']))
}

impl fmt::Display for ReplyInbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn accepts_only_the_caller_scoped_inbox() -> TestResult {
        let key: PresenceKey = "ana@x.io".parse()?;
        assert!(ReplyInbox::scoped(&key, Subject::from("_INBOX_U.ana=40x=2Eio.c1.n1")).is_ok());
        assert!(ReplyInbox::scoped(&key, Subject::from("_INBOX_U.ana=40x=2Eio.c1.n1.n2")).is_ok());
        for bad in [
            "_INBOX_U.ana=40x=2Eio.c1",
            "_INBOX_U.bob.c1.n1",
            "_INBOX.ana=40x=2Eio.c1.n1",
            "_INBOX_U.ana=40x=2Eio..n1",
            "_INBOX_U.ana=40x=2Eio.c1.*",
            "_INBOX_U.ana=40x=2Eio.c1.>",
            "_INBOX_U.ana=40x=2Eiox.c1.n1",
            "presence.v1.diff.room",
        ] {
            assert!(ReplyInbox::scoped(&key, Subject::from(bad)).is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn accepts_only_the_connection_scoped_inbox() -> TestResult {
        let key: PresenceKey = "ana@x.io".parse()?;
        let connection = ConnectionId::from([7; 16]);
        let other = ConnectionId::from([8; 16]);
        let good = format!("_INBOX_U.ana=40x=2Eio.{connection}.n1");
        assert!(ReplyInbox::for_connection(&key, &connection, Subject::from(good.as_str())).is_ok());
        for bad in [
            format!("_INBOX_U.ana=40x=2Eio.{connection}"),
            format!("_INBOX_U.ana=40x=2Eio.{other}.n1"),
            format!("_INBOX_U.bob.{connection}.n1"),
            format!("_INBOX_U.ana=40x=2Eio.{connection}.*"),
            format!("_INBOX_U.ana=40x=2Eio.{connection}x.n1"),
        ] {
            assert!(
                ReplyInbox::for_connection(&key, &connection, Subject::from(bad.as_str())).is_err(),
                "{bad}"
            );
        }
        Ok(())
    }
}

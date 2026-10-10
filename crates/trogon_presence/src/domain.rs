use std::fmt;
use std::str::FromStr;

use async_nats::jetstream::{self, Context};
use async_nats::Subject;

use crate::constants::{JS_API_TOKEN, JS_SUBJECT_ROOT, TOKEN_SEPARATOR};

/// A JetStream domain name, one subject token that may not hold wildcards or whitespace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct JetStreamDomain(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("jetstream domain {0:?} must be one non-empty subject token without dots, whitespace or wildcards")]
pub struct JetStreamDomainError(String);

impl JetStreamDomain {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for JetStreamDomain {
    type Error = JetStreamDomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let valid = !value.is_empty()
            && value
                .chars()
                .all(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '.' | '*' | '>'));
        if valid {
            Ok(Self(value))
        } else {
            Err(JetStreamDomainError(value))
        }
    }
}

impl FromStr for JetStreamDomain {
    type Err = JetStreamDomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl fmt::Display for JetStreamDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where JetStream API calls and bucket writes go: the domain of the server the client is connected to,
/// or a named domain reached through its API prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct JetStreamRoute(Option<JetStreamDomain>);

impl JetStreamRoute {
    pub fn local() -> Self {
        Self(None)
    }

    pub fn through(domain: JetStreamDomain) -> Self {
        Self(Some(domain))
    }

    pub fn domain(&self) -> Option<&JetStreamDomain> {
        self.0.as_ref()
    }

    pub fn api_prefix(&self) -> String {
        match &self.0 {
            Some(domain) => format!("{JS_SUBJECT_ROOT}{TOKEN_SEPARATOR}{domain}{TOKEN_SEPARATOR}{JS_API_TOKEN}"),
            None => format!("{JS_SUBJECT_ROOT}{TOKEN_SEPARATOR}{JS_API_TOKEN}"),
        }
    }

    pub fn context(&self, client: async_nats::Client) -> Context {
        jetstream::with_prefix(client, &self.api_prefix())
    }

    /// Maps a stored bucket subject to the subject a write is published on. A leafnode between JetStream
    /// domains does not carry stored bucket subjects, so a write for another domain goes through that
    /// domain's API prefix, which the owning server maps back onto the stored subject.
    pub fn publish_subject(&self, stored: &Subject) -> Subject {
        match &self.0 {
            Some(_) => Subject::from(format!("{}{TOKEN_SEPARATOR}{stored}", self.api_prefix())),
            None => stored.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BucketName;
    use crate::holder::HolderId;
    use crate::kv_key::{EntryKey, KvKey};
    use crate::shard::ShardCount;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn hub() -> Result<JetStreamDomain, JetStreamDomainError> {
        "hub".parse()
    }

    #[test]
    fn a_domain_is_one_plain_token() -> TestResult {
        assert_eq!(hub()?.as_str(), "hub");
        assert_eq!("edge-1_a".parse::<JetStreamDomain>()?.to_string(), "edge-1_a");
        for invalid in ["", "a.b", "a b", "a\tb", "*", "a>", "hub.>", " "] {
            assert_eq!(
                invalid.parse::<JetStreamDomain>(),
                Err(JetStreamDomainError(invalid.to_owned())),
                "{invalid:?} should be refused"
            );
        }
        Ok(())
    }

    #[test]
    fn a_local_route_uses_the_default_api_prefix() {
        assert_eq!(JetStreamRoute::local().api_prefix(), "$JS.API");
        assert_eq!(JetStreamRoute::default(), JetStreamRoute::local());
        assert_eq!(JetStreamRoute::local().domain(), None);
    }

    #[test]
    fn a_domain_route_uses_the_domain_api_prefix() -> TestResult {
        let route = JetStreamRoute::through(hub()?);
        assert_eq!(route.api_prefix(), "$JS.hub.API");
        assert_eq!(route.domain(), Some(&hub()?));
        Ok(())
    }

    #[test]
    fn a_local_route_publishes_on_the_stored_subject() {
        let stored = Subject::from("$KV.PRESENCE_V1.s1.k");
        assert_eq!(JetStreamRoute::local().publish_subject(&stored), stored);
    }

    #[test]
    fn a_domain_route_publishes_through_the_domain_api() -> TestResult {
        let stored = Subject::from("$KV.PRESENCE_V1.s1.k");
        assert_eq!(
            JetStreamRoute::through(hub()?).publish_subject(&stored).as_str(),
            "$JS.hub.API.$KV.PRESENCE_V1.s1.k"
        );
        Ok(())
    }

    #[test]
    fn keys_with_dots_and_wildcard_characters_keep_their_stored_encoding() -> TestResult {
        let route = JetStreamRoute::through(hub()?);
        let bucket = BucketName::default();
        let topic = "room:lobby.a*b>c".parse()?;
        let key = "ana@x.io".parse()?;
        let kv: KvKey = EntryKey::new(topic, key, HolderId::generate()?).encode(ShardCount::DEFAULT)?;
        let stored = Subject::from(bucket.subject_for(&kv));
        let routed = route.publish_subject(&stored);
        assert_eq!(routed.as_str(), format!("$JS.hub.API.{stored}"));
        assert_eq!(routed.as_str().strip_prefix("$JS.hub.API."), Some(stored.as_str()));
        Ok(())
    }
}

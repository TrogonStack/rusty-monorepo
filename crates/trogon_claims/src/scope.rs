use std::borrow::Cow;

use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("scope token must contain only the RFC 6749 scope-token characters")]
pub struct ScopeNameError;

/// An OAuth scope token, distinct from a role or permission decision.
/// See [RFC 6749 §3.3](https://www.rfc-editor.org/rfc/rfc6749.html#section-3.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeName(Cow<'static, str>);

impl ScopeName {
    pub fn try_new(value: impl Into<Cow<'static, str>>) -> Result<Self, ScopeNameError> {
        let value = value.into();
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| matches!(byte, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
        {
            return Err(ScopeNameError);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Scope membership with the source token order and repetitions preserved.
/// Parsing performs no sorting, deduplication or whitespace normalization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeSet(Vec<ScopeName>);

impl ScopeSet {
    /// Parses the non-empty, single-space-delimited OAuth scope syntax.
    pub fn try_from_space_delimited(value: &str) -> Result<Self, ScopeNameError> {
        let scopes = value
            .split(' ')
            .map(|value| ScopeName::try_new(value.to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(scopes))
    }

    pub fn scopes(&self) -> &[ScopeName] {
        &self.0
    }

    pub fn contains(&self, scope: &ScopeName) -> bool {
        self.0.contains(scope)
    }
}

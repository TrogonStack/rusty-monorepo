use std::borrow::Cow;

use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("group name must not be empty")]
pub struct GroupNameError;

/// A group-membership assertion, distinct from a caller role.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupName(Cow<'static, str>);

impl GroupName {
    pub fn try_new(value: impl Into<Cow<'static, str>>) -> Result<Self, GroupNameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(GroupNameError);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

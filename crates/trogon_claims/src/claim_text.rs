use std::borrow::Cow;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClaimText(Cow<'static, str>);

impl ClaimText {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0.into_owned()
    }
}

impl From<String> for ClaimText {
    fn from(value: String) -> Self {
        Self(Cow::Owned(value))
    }
}

impl From<&'static str> for ClaimText {
    fn from(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }
}

impl From<Cow<'static, str>> for ClaimText {
    fn from(value: Cow<'static, str>) -> Self {
        Self(value)
    }
}

impl fmt::Display for ClaimText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

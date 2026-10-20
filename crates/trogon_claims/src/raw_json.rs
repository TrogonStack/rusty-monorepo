use crate::raw_scan::array_elements;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawJson(Box<str>);

impl RawJson {
    pub(crate) fn from_validated(text: &str) -> Self {
        Self(text.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn kind(&self) -> RawKind {
        match self.0.as_bytes().first() {
            Some(b'"') => RawKind::String,
            Some(b'{') => RawKind::Object,
            Some(b'[') => RawKind::Array,
            Some(b't' | b'f') => RawKind::Bool,
            Some(b'n') => RawKind::Null,
            _ => RawKind::Number,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match &*self.0 {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<String> {
        if self.kind() != RawKind::String {
            return None;
        }
        if !self.0.contains('\\') {
            return self.0.get(1..self.0.len() - 1).map(str::to_owned);
        }
        serde_json::from_str(&self.0).ok()
    }

    pub fn number_text(&self) -> Option<&str> {
        (self.kind() == RawKind::Number).then_some(&*self.0)
    }

    pub fn as_array(&self) -> Option<Vec<RawJson>> {
        if self.kind() != RawKind::Array {
            return None;
        }
        array_elements(&self.0).map(|elements| elements.into_iter().map(Self::from_validated).collect())
    }
}

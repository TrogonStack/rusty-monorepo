use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

use crate::constants::{CANONICAL_JSON_MAX_DEPTH, OPERATION_DOMAIN_TAG};

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalJsonV1(Value);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalJsonError {
    #[error("json nests deeper than {CANONICAL_JSON_MAX_DEPTH} containers")]
    TooDeep,
    #[error("json object repeats member {0:?}")]
    DuplicateMember(String),
    #[error("json could not be parsed: {0}")]
    Syntax(String),
}

impl CanonicalJsonV1 {
    pub fn from_value(value: Value) -> Result<Self, CanonicalJsonError> {
        if depth_of(&value) > CANONICAL_JSON_MAX_DEPTH {
            return Err(CanonicalJsonError::TooDeep);
        }
        Ok(Self(value))
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, CanonicalJsonError> {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let value = Node { depth: 0 }
            .deserialize(&mut deserializer)
            .map_err(|err| classify(&err))?;
        deserializer.end().map_err(|err| classify(&err))?;
        Ok(Self(value))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn into_value(self) -> Value {
        self.0
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_canonical(&self.0, &mut out);
        out
    }
}

pub(crate) fn canonical_bytes(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(value, &mut out);
    out
}

pub(crate) fn depth_of(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(depth_of).max().unwrap_or(0),
        Value::Object(members) => 1 + members.values().map(depth_of).max().unwrap_or(0),
        _ => 0,
    }
}

fn classify(err: &serde_json::Error) -> CanonicalJsonError {
    let message = err.to_string();
    if let Some(member) = message
        .strip_prefix(DUPLICATE_PREFIX)
        .and_then(|rest| rest.split(DUPLICATE_SUFFIX).next())
    {
        return CanonicalJsonError::DuplicateMember(member.to_owned());
    }
    if message.starts_with(TOO_DEEP_MESSAGE) {
        return CanonicalJsonError::TooDeep;
    }
    CanonicalJsonError::Syntax(message)
}

const DUPLICATE_PREFIX: &str = "duplicate member ";
const DUPLICATE_SUFFIX: &str = "\u{0}";
const TOO_DEEP_MESSAGE: &str = "json nests too deep";

fn write_canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        Value::Object(members) => {
            let mut sorted: Vec<(&String, &Value)> = members.iter().collect();
            sorted.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            out.push(b'{');
            for (index, (key, member)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                out.extend_from_slice(&scalar_bytes(&Value::String(key.clone())));
                out.push(b':');
                write_canonical(member, out);
            }
            out.push(b'}');
        }
        scalar => out.extend_from_slice(&scalar_bytes(scalar)),
    }
}

fn scalar_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

struct Node {
    depth: usize,
}

impl Node {
    fn enter<E: de::Error>(&self) -> Result<Self, E> {
        let depth = self.depth + 1;
        if depth > CANONICAL_JSON_MAX_DEPTH {
            return Err(E::custom(TOO_DEEP_MESSAGE));
        }
        Ok(Self { depth })
    }
}

impl<'de> DeserializeSeed<'de> for Node {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Node {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a json value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("json numbers must be finite"))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let inner = self.enter()?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(Node { depth: inner.depth })? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let inner = self.enter()?;
        let mut members = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if members.contains_key(&key) {
                return Err(de::Error::custom(format_args!(
                    "{DUPLICATE_PREFIX}{key}{DUPLICATE_SUFFIX}"
                )));
            }
            let member = map.next_value_seed(Node { depth: inner.depth })?;
            members.insert(key, member);
        }
        Ok(Value::Object(members))
    }
}

impl<'de> Deserialize<'de> for CanonicalJsonV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Node { depth: 0 }.deserialize(deserializer).map(Self)
    }
}

impl Serialize for CanonicalJsonV1 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationFingerprint([u8; 32]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("operation fingerprint must be 32 bytes of unpadded base64url")]
pub struct FingerprintError;

pub(crate) struct FingerprintBuilder(Sha256);

impl FingerprintBuilder {
    pub(crate) fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(OPERATION_DOMAIN_TAG.as_bytes());
        Self(hasher)
    }

    pub(crate) fn field(mut self, bytes: &[u8]) -> Self {
        self.0.update((bytes.len() as u64).to_be_bytes());
        self.0.update(bytes);
        self
    }

    pub(crate) fn optional(self, bytes: Option<&[u8]>) -> Self {
        match bytes {
            Some(bytes) => self.field(&[1]).field(bytes),
            None => self.field(&[0]),
        }
    }

    pub(crate) fn finish(self) -> OperationFingerprint {
        OperationFingerprint(self.0.finalize().into())
    }
}

impl OperationFingerprint {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for OperationFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for OperationFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OperationFingerprint({self})")
    }
}

impl std::str::FromStr for OperationFingerprint {
    type Err = FingerprintError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = URL_SAFE_NO_PAD.decode(s).map_err(|_| FingerprintError)?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| FingerprintError)?;
        if URL_SAFE_NO_PAD.encode(bytes) != s {
            return Err(FingerprintError);
        }
        Ok(Self(bytes))
    }
}

impl Serialize for OperationFingerprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for OperationFingerprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn canonical(input: &str) -> Result<String, Box<dyn std::error::Error>> {
        Ok(String::from_utf8(CanonicalJsonV1::parse(input.as_bytes())?.to_bytes())?)
    }

    #[test]
    fn golden_vectors() -> TestResult {
        let vectors = [
            (r#"{"b":1,"a":2}"#, r#"{"a":2,"b":1}"#),
            (
                r#"{"int":-9007199254740993,"big":18446744073709551615,"zero":0}"#,
                r#"{"big":18446744073709551615,"int":-9007199254740993,"zero":0}"#,
            ),
            (r#"[1.5, 1e2, 0.1, 2.50, 1E-7]"#, r#"[1.5,100.0,0.1,2.5,1e-7]"#),
            (r#"{"n":-0, "f":-0.0}"#, r#"{"f":-0.0,"n":-0.0}"#),
            (
                r#"{"s":"tab\tquote\"slash\\ctl\u0001solidus\/"}"#,
                r#"{"s":"tab\tquote\"slash\\ctl\u0001solidus/"}"#,
            ),
            (r#"{"é":"é","a":"日本","z":"😀"}"#, r#"{"a":"日本","z":"😀","é":"é"}"#),
            (
                r#"{"outer":{"z":[{"y":1,"x":2}],"a":null},"A":true}"#,
                r#"{"A":true,"outer":{"a":null,"z":[{"x":2,"y":1}]}}"#,
            ),
        ];
        for (input, expected) in vectors {
            assert_eq!(canonical(input)?, expected, "{input}");
        }
        Ok(())
    }

    #[test]
    fn sorts_by_utf8_bytes_not_utf16_units() -> TestResult {
        assert_eq!(canonical(r#"{"｡":1,"😀":2}"#)?, r#"{"｡":1,"😀":2}"#);
        Ok(())
    }

    #[test]
    fn rejects_duplicate_members_at_any_depth() {
        assert_eq!(
            CanonicalJsonV1::parse(br#"{"a":1,"a":2}"#),
            Err(CanonicalJsonError::DuplicateMember("a".to_owned()))
        );
        assert_eq!(
            CanonicalJsonV1::parse(br#"{"x":[{"k":1,"k":1}]}"#),
            Err(CanonicalJsonError::DuplicateMember("k".to_owned()))
        );
    }

    #[test]
    fn rejects_nesting_beyond_32_containers() -> TestResult {
        let nested = |depth: usize| format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(CanonicalJsonV1::parse(nested(32).as_bytes()).is_ok());
        assert_eq!(
            CanonicalJsonV1::parse(nested(33).as_bytes()),
            Err(CanonicalJsonError::TooDeep)
        );
        let deep: Value = serde_json::from_str(&nested(33))?;
        assert_eq!(CanonicalJsonV1::from_value(deep), Err(CanonicalJsonError::TooDeep));
        Ok(())
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(matches!(
            CanonicalJsonV1::parse(b"{} {}"),
            Err(CanonicalJsonError::Syntax(_))
        ));
    }

    #[test]
    fn fingerprint_round_trips_and_separates_fields() -> TestResult {
        let joined = FingerprintBuilder::new().field(b"ab").field(b"c").finish();
        let split = FingerprintBuilder::new().field(b"a").field(b"bc").finish();
        assert_ne!(joined, split);
        assert_eq!(joined.to_string().parse::<OperationFingerprint>()?, joined);
        Ok(())
    }
}

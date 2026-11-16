use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;

use crate::claim_property_key::ClaimPropertyKey;
use crate::claim_text::ClaimText;
use crate::constants::{
    VALUE_TYPE_BASE64_BINARY, VALUE_TYPE_BOOLEAN, VALUE_TYPE_DATE_TIME, VALUE_TYPE_DAYTIME_DURATION, VALUE_TYPE_DOUBLE,
    VALUE_TYPE_INTEGER64, VALUE_TYPE_STRING, VALUE_TYPE_U_INTEGER64,
};
use crate::timestamp::Timestamp;
use crate::unsigned63::Unsigned63;

#[derive(Clone, Debug, PartialEq)]
pub enum ClaimValue {
    Text(ClaimText),
    Integer(i64),
    Unsigned(Unsigned63),
    Double(f64),
    Boolean(bool),
    Timestamp(Timestamp),
    Duration(Duration),
    Bytes(Vec<u8>),
    IpAddr(IpAddr),
    List(Vec<ClaimValue>),
    Map(BTreeMap<ClaimPropertyKey, ClaimValue>),
}

impl ClaimValue {
    pub const fn value_type(&self) -> Option<&'static str> {
        match self {
            Self::Text(_) | Self::IpAddr(_) => Some(VALUE_TYPE_STRING),
            Self::Integer(_) => Some(VALUE_TYPE_INTEGER64),
            Self::Unsigned(_) => Some(VALUE_TYPE_U_INTEGER64),
            Self::Double(_) => Some(VALUE_TYPE_DOUBLE),
            Self::Boolean(_) => Some(VALUE_TYPE_BOOLEAN),
            Self::Timestamp(_) => Some(VALUE_TYPE_DATE_TIME),
            Self::Duration(_) => Some(VALUE_TYPE_DAYTIME_DURATION),
            Self::Bytes(_) => Some(VALUE_TYPE_BASE64_BINARY),
            Self::List(_) | Self::Map(_) => None,
        }
    }
}

impl From<ClaimText> for ClaimValue {
    fn from(value: ClaimText) -> Self {
        Self::Text(value)
    }
}

impl From<i64> for ClaimValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<Unsigned63> for ClaimValue {
    fn from(value: Unsigned63) -> Self {
        Self::Unsigned(value)
    }
}

impl From<bool> for ClaimValue {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<Timestamp> for ClaimValue {
    fn from(value: Timestamp) -> Self {
        Self::Timestamp(value)
    }
}

impl From<Duration> for ClaimValue {
    fn from(value: Duration) -> Self {
        Self::Duration(value)
    }
}

impl From<IpAddr> for ClaimValue {
    fn from(value: IpAddr) -> Self {
        Self::IpAddr(value)
    }
}

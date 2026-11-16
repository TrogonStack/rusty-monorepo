use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::time::Duration;

use prost::Message;
use prost_types::value::Kind;
use prost_types::{ListValue, Struct, Value};

use crate::base64_standard::encode_padded;
use crate::caveat_context_error::CaveatContextError;
use crate::claim_property_key::ClaimPropertyKey;
use crate::claim_value::ClaimValue;
use crate::constants::{DEFAULT_CAVEAT_CONTEXT_BUDGET, MAX_EXACT_NESTED_INTEGER, NANO_DIGITS};
use crate::param_name::ParamName;
use crate::sealed::Sealed;

pub trait ContextPhase: Sealed {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckTime {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteTime {}

impl Sealed for CheckTime {}
impl Sealed for WriteTime {}
impl ContextPhase for CheckTime {}
impl ContextPhase for WriteTime {}

pub type CheckContext = CaveatContext<CheckTime>;
pub type WriteContext = CaveatContext<WriteTime>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamDeclaration {
    names: BTreeSet<ParamName>,
}

impl ParamDeclaration {
    pub fn new(names: impl IntoIterator<Item = ParamName>) -> Self {
        Self {
            names: names.into_iter().collect(),
        }
    }

    pub fn contains(&self, name: &ParamName) -> bool {
        self.names.contains(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &ParamName> {
        self.names.iter()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaveatContext<P: ContextPhase> {
    entries: BTreeMap<ParamName, ClaimValue>,
    budget: usize,
    phase: PhantomData<fn() -> P>,
}

impl<P: ContextPhase> CaveatContext<P> {
    pub fn builder(declaration: &ParamDeclaration) -> CaveatContextBuilder<'_, P> {
        CaveatContextBuilder {
            declaration,
            entries: BTreeMap::new(),
            budget: DEFAULT_CAVEAT_CONTEXT_BUDGET,
            phase: PhantomData,
        }
    }

    pub fn get(&self, name: &ParamName) -> Option<&ClaimValue> {
        self.entries.get(name)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn budget(&self) -> usize {
        self.budget
    }
}

#[derive(Debug)]
pub struct CaveatContextBuilder<'d, P: ContextPhase> {
    declaration: &'d ParamDeclaration,
    entries: BTreeMap<ParamName, ClaimValue>,
    budget: usize,
    phase: PhantomData<fn() -> P>,
}

impl<P: ContextPhase> CaveatContextBuilder<'_, P> {
    pub fn with_budget(mut self, budget: usize) -> Self {
        self.budget = budget;
        self
    }

    pub fn insert(&mut self, name: ParamName, value: impl Into<ClaimValue>) -> Result<&mut Self, CaveatContextError> {
        if !self.declaration.contains(&name) {
            return Err(CaveatContextError::Undeclared { parameter: name });
        }
        if self.entries.contains_key(&name) {
            return Err(CaveatContextError::Duplicate { parameter: name });
        }
        self.entries.insert(name, value.into());
        Ok(self)
    }

    pub fn build(self) -> CaveatContext<P> {
        CaveatContext {
            entries: self.entries,
            budget: self.budget,
            phase: PhantomData,
        }
    }
}

enum Unrepresentable {
    NestedInteger,
    NonFinite,
}

fn string(text: String) -> Value {
    Value {
        kind: Some(Kind::StringValue(text)),
    }
}

fn number(value: f64) -> Result<Value, Unrepresentable> {
    if value.is_finite() {
        Ok(Value {
            kind: Some(Kind::NumberValue(value)),
        })
    } else {
        Err(Unrepresentable::NonFinite)
    }
}

fn duration_text(duration: Duration) -> String {
    let nanos = duration.subsec_nanos();
    if nanos == 0 {
        return format!("{}s", duration.as_secs());
    }
    let fraction = format!("{nanos:0width$}", width = NANO_DIGITS);
    format!("{}.{}s", duration.as_secs(), fraction.trim_end_matches('0'))
}

fn nested_integer(magnitude: u64, value: f64) -> Result<Value, Unrepresentable> {
    if magnitude > MAX_EXACT_NESTED_INTEGER.unsigned_abs() {
        return Err(Unrepresentable::NestedInteger);
    }
    number(value)
}

fn nested_struct(map: &BTreeMap<ClaimPropertyKey, ClaimValue>) -> Result<Value, Unrepresentable> {
    let mut fields = BTreeMap::new();
    for (key, value) in map {
        fields.insert(key.as_str().to_owned(), nested(value)?);
    }
    Ok(Value {
        kind: Some(Kind::StructValue(Struct { fields })),
    })
}

fn nested_list(items: &[ClaimValue]) -> Result<Value, Unrepresentable> {
    let values = items.iter().map(nested).collect::<Result<Vec<_>, _>>()?;
    Ok(Value {
        kind: Some(Kind::ListValue(ListValue { values })),
    })
}

fn nested(value: &ClaimValue) -> Result<Value, Unrepresentable> {
    match value {
        ClaimValue::Integer(integer) => nested_integer(integer.unsigned_abs(), *integer as f64),
        ClaimValue::Unsigned(unsigned) => nested_integer(unsigned.get(), unsigned.get() as f64),
        other => scalar_or_container(other),
    }
}

fn top_level(value: &ClaimValue) -> Result<Value, Unrepresentable> {
    match value {
        ClaimValue::Integer(integer) => Ok(string(integer.to_string())),
        ClaimValue::Unsigned(unsigned) => Ok(string(unsigned.get().to_string())),
        other => scalar_or_container(other),
    }
}

fn scalar_or_container(value: &ClaimValue) -> Result<Value, Unrepresentable> {
    match value {
        ClaimValue::Text(text) => Ok(string(text.as_str().to_owned())),
        ClaimValue::Double(double) => number(*double),
        ClaimValue::Boolean(boolean) => Ok(Value {
            kind: Some(Kind::BoolValue(*boolean)),
        }),
        ClaimValue::Timestamp(timestamp) => Ok(string(timestamp.to_string())),
        ClaimValue::Duration(duration) => Ok(string(duration_text(*duration))),
        ClaimValue::Bytes(bytes) => Ok(string(encode_padded(bytes))),
        ClaimValue::IpAddr(address) => Ok(string(address.to_string())),
        ClaimValue::List(items) => nested_list(items),
        ClaimValue::Map(map) => nested_struct(map),
        ClaimValue::Integer(_) | ClaimValue::Unsigned(_) => nested(value),
    }
}

impl<P: ContextPhase> TryFrom<&CaveatContext<P>> for Struct {
    type Error = CaveatContextError;

    fn try_from(context: &CaveatContext<P>) -> Result<Self, Self::Error> {
        let mut fields = BTreeMap::new();
        for (name, value) in &context.entries {
            let converted = top_level(value).map_err(|reason| match reason {
                Unrepresentable::NestedInteger => CaveatContextError::NestedIntegerPrecision {
                    parameter: name.clone(),
                },
                Unrepresentable::NonFinite => CaveatContextError::NonFiniteDouble {
                    parameter: name.clone(),
                },
            })?;
            fields.insert(name.as_str().to_owned(), converted);
        }
        let projected = Struct { fields };
        let encoded = projected.encoded_len();
        if encoded > context.budget {
            return Err(CaveatContextError::OverBudget {
                encoded,
                budget: context.budget,
            });
        }
        Ok(projected)
    }
}

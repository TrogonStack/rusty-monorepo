use std::collections::BTreeMap;
use std::sync::Arc;

use crate::claim_property_key::ClaimPropertyKey;
use crate::claim_text::ClaimText;
use crate::claim_type::ClaimType;
use crate::claim_value::ClaimValue;
use crate::issuer::Issuer;

#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    claim_type: ClaimType,
    value: ClaimValue,
    issuer: Arc<Issuer>,
    original_issuer: Option<Arc<Issuer>>,
    properties: BTreeMap<ClaimPropertyKey, ClaimText>,
}

impl Claim {
    pub fn builder(claim_type: ClaimType, value: ClaimValue) -> ClaimBuilder {
        ClaimBuilder::new(claim_type, value)
    }

    pub fn claim_type(&self) -> &ClaimType {
        &self.claim_type
    }

    pub fn value(&self) -> &ClaimValue {
        &self.value
    }

    pub fn value_type(&self) -> Option<&'static str> {
        self.value.value_type()
    }

    pub fn issuer(&self) -> &Issuer {
        &self.issuer
    }

    pub fn original_issuer(&self) -> &Issuer {
        self.original_issuer.as_deref().unwrap_or(&self.issuer)
    }

    pub fn properties(&self) -> &BTreeMap<ClaimPropertyKey, ClaimText> {
        &self.properties
    }
}

#[derive(Clone, Debug)]
pub struct ClaimBuilder {
    claim_type: ClaimType,
    value: ClaimValue,
    issuer: Option<Arc<Issuer>>,
    original_issuer: Option<Arc<Issuer>>,
    properties: BTreeMap<ClaimPropertyKey, ClaimText>,
}

impl ClaimBuilder {
    pub fn new(claim_type: ClaimType, value: ClaimValue) -> Self {
        Self {
            claim_type,
            value,
            issuer: None,
            original_issuer: None,
            properties: BTreeMap::new(),
        }
    }

    pub fn issuer(mut self, issuer: Issuer) -> Self {
        self.issuer = Some(Arc::new(issuer));
        self
    }

    pub fn original_issuer(mut self, issuer: Issuer) -> Self {
        self.original_issuer = Some(Arc::new(issuer));
        self
    }

    pub fn property(mut self, key: ClaimPropertyKey, value: ClaimText) -> Self {
        self.properties.insert(key, value);
        self
    }

    #[cfg(feature = "json")]
    pub(crate) fn shared_issuer(mut self, issuer: Arc<Issuer>) -> Self {
        self.issuer = Some(issuer);
        self
    }

    pub fn build(self) -> Claim {
        Claim {
            claim_type: self.claim_type,
            value: self.value,
            issuer: self.issuer.unwrap_or_else(|| Arc::new(Issuer::local_authority())),
            original_issuer: self.original_issuer,
            properties: self.properties,
        }
    }
}

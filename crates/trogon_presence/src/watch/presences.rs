use std::collections::BTreeMap;

use serde::de::Error as _;
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::key::PresenceKey;
use crate::meta::Meta;
use crate::phx_ref::ViewRef;

const METAS_FIELD: &str = "metas";
const PHX_REF_FIELD: &str = "phx_ref";
const PHX_REF_PREV_FIELD: &str = "phx_ref_prev";

#[derive(Debug, Clone, PartialEq)]
pub struct MetaEntry {
    phx_ref: ViewRef,
    phx_ref_prev: Option<ViewRef>,
    meta: Meta,
}

impl MetaEntry {
    pub fn new(phx_ref: ViewRef, phx_ref_prev: Option<ViewRef>, meta: Meta) -> Self {
        Self {
            phx_ref,
            phx_ref_prev,
            meta,
        }
    }

    pub fn phx_ref(&self) -> &ViewRef {
        &self.phx_ref
    }

    pub fn phx_ref_prev(&self) -> Option<&ViewRef> {
        self.phx_ref_prev.as_ref()
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn with_meta(self, meta: Meta) -> Self {
        Self { meta, ..self }
    }
}

impl Serialize for MetaEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let fields = self.meta.as_map().len() + 1 + usize::from(self.phx_ref_prev.is_some());
        let mut map = serializer.serialize_map(Some(fields))?;
        for (name, value) in self.meta.as_map() {
            map.serialize_entry(name, value)?;
        }
        map.serialize_entry(PHX_REF_FIELD, &self.phx_ref)?;
        if let Some(prev) = &self.phx_ref_prev {
            map.serialize_entry(PHX_REF_PREV_FIELD, prev)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for MetaEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = Map::<String, Value>::deserialize(deserializer)?;
        let phx_ref = fields
            .remove(PHX_REF_FIELD)
            .ok_or_else(|| D::Error::missing_field(PHX_REF_FIELD))
            .and_then(|value| ViewRef::deserialize(value).map_err(D::Error::custom))?;
        let phx_ref_prev = fields
            .remove(PHX_REF_PREV_FIELD)
            .map(|value| ViewRef::deserialize(value).map_err(D::Error::custom))
            .transpose()?;
        let meta = Meta::try_from(fields).map_err(D::Error::custom)?;
        Ok(Self::new(phx_ref, phx_ref_prev, meta))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Presences(BTreeMap<PresenceKey, Vec<MetaEntry>>);

impl Presences {
    pub fn get(&self, key: &PresenceKey) -> Option<&[MetaEntry]> {
        self.0.get(key).map(Vec::as_slice)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PresenceKey, &[MetaEntry])> {
        self.0.iter().map(|(key, metas)| (key, metas.as_slice()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_map(self) -> BTreeMap<PresenceKey, Vec<MetaEntry>> {
        self.0
    }

    pub(crate) fn push(&mut self, key: PresenceKey, entry: MetaEntry) {
        self.0.entry(key).or_default().push(entry);
    }
}

impl From<BTreeMap<PresenceKey, Vec<MetaEntry>>> for Presences {
    fn from(map: BTreeMap<PresenceKey, Vec<MetaEntry>>) -> Self {
        Self(map.into_iter().filter(|(_, metas)| !metas.is_empty()).collect())
    }
}

impl FromIterator<(PresenceKey, MetaEntry)> for Presences {
    fn from_iter<I: IntoIterator<Item = (PresenceKey, MetaEntry)>>(iter: I) -> Self {
        let mut presences = Self::default();
        for (key, entry) in iter {
            presences.push(key, entry);
        }
        presences
    }
}

impl IntoIterator for Presences {
    type Item = (PresenceKey, Vec<MetaEntry>);
    type IntoIter = std::collections::btree_map::IntoIter<PresenceKey, Vec<MetaEntry>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

struct Metas<'a>(&'a [MetaEntry]);

impl Serialize for Metas<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(METAS_FIELD, self.0)?;
        map.end()
    }
}

impl Serialize for Presences {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, metas) in &self.0 {
            map.serialize_entry(key.as_str(), &Metas(metas))?;
        }
        map.end()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetasWire {
    metas: Vec<MetaEntry>,
}

impl<'de> Deserialize<'de> for Presences {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, MetasWire>::deserialize(deserializer)?;
        let mut map = BTreeMap::new();
        for (key, wire) in raw {
            let key: PresenceKey = key.parse().map_err(D::Error::custom)?;
            map.insert(key, wire.metas);
        }
        Ok(Self::from(map))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diff {
    joins: Presences,
    leaves: Presences,
}

impl Diff {
    pub fn new(joins: Presences, leaves: Presences) -> Self {
        Self { joins, leaves }
    }

    pub fn joins(&self) -> &Presences {
        &self.joins
    }

    pub fn leaves(&self) -> &Presences {
        &self.leaves
    }

    pub fn is_empty(&self) -> bool {
        self.joins.is_empty() && self.leaves.is_empty()
    }

    pub fn into_parts(self) -> (Presences, Presences) {
        (self.joins, self.leaves)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn serializes_phoenix_diff_shape() -> TestResult {
        let meta: Meta = serde_json::from_str(r#"{"status":"away"}"#)?;
        let joined = MetaEntry::new("Fq1".parse()?, Some("Fp9".parse()?), meta.clone());
        let left = MetaEntry::new("Fp9".parse()?, None, meta);
        let key: PresenceKey = "ana@x.io".parse()?;
        let diff = Diff::new(
            [(key.clone(), joined)].into_iter().collect(),
            [(key, left)].into_iter().collect(),
        );
        assert_eq!(
            serde_json::to_string(&diff)?,
            r#"{"joins":{"ana@x.io":{"metas":[{"status":"away","phx_ref":"Fq1","phx_ref_prev":"Fp9"}]}},"leaves":{"ana@x.io":{"metas":[{"status":"away","phx_ref":"Fp9"}]}}}"#
        );
        assert_eq!(serde_json::to_string(&Diff::default())?, r#"{"joins":{},"leaves":{}}"#);
        let decoded: Diff = serde_json::from_str(&serde_json::to_string(&diff)?)?;
        assert_eq!(decoded, diff);
        Ok(())
    }
}

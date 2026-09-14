//! What a run was asked to do, paired with what its harness actually enforced.
//!
//! `--permission` names the grant an operator asks for, but not every harness draws a
//! boundary that fine: cursor-agent's only non-interactive mode runs unrestricted, so a
//! run asked for `workspace_write` under it still executes with wider authority. A report
//! that records only the request lets a reader believe a run was bounded when it was not,
//! which is worse than recording nothing, because it reads as an answer.

use schemars::JsonSchema;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use super::report::PermissionGrant;

/// A run's requested grant, paired with the grant its harness actually enforced.
///
/// `effective` is never narrower than `requested`: a harness can fail to draw as fine a
/// boundary as the one asked for, but nothing here models a harness that enforces less
/// authority than it was granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(schema_with = "permission_outcome_schema")]
pub struct PermissionOutcome {
    requested: PermissionGrant,
    effective: PermissionGrant,
}

impl PermissionOutcome {
    /// Refuses a pair where the harness enforced less than what was requested: no runner
    /// in this codebase ever narrows a grant, so a value like that could only come from a
    /// hand-edited or foreign `report.json`.
    pub fn new(requested: PermissionGrant, effective: PermissionGrant) -> Result<Self, String> {
        if effective < requested {
            return Err(format!(
                "a run cannot execute under less authority than it was granted: requested {} but effective {}",
                requested.as_str(),
                effective.as_str()
            ));
        }
        Ok(Self { requested, effective })
    }

    pub fn requested(self) -> PermissionGrant {
        self.requested
    }

    pub fn effective(self) -> PermissionGrant {
        self.effective
    }

    /// Whether the harness enforced more than the operator asked for.
    ///
    /// True exactly when a reader comparing `requested` to `effective` would otherwise
    /// have to notice the mismatch themselves.
    pub fn was_widened(self) -> bool {
        self.effective > self.requested
    }
}

impl Default for PermissionOutcome {
    fn default() -> Self {
        PermissionGrant::default().into()
    }
}

impl From<PermissionGrant> for PermissionOutcome {
    /// A grant with nothing yet to compare it against, so it has not been widened.
    fn from(grant: PermissionGrant) -> Self {
        Self {
            requested: grant,
            effective: grant,
        }
    }
}

/// Written by hand rather than derived, and kept as strict as `Deserialize`, because a
/// schema looser than its own parser lets `eval verify --mode strict` call a report
/// conformant that `grade`, `benchmark` and `compare` then refuse to read. Enumerated by
/// value rather than expressed as a general object shape because `PermissionGrant` has
/// exactly two variants, so every value either side accepts costs nothing to list.
fn permission_outcome_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "description": "What a run was asked to do, paired with what its harness actually enforced. A bare grant is the shape written before this distinction existed, and reads as a run whose harness enforced exactly what was requested.",
        "enum": [
            "workspace_write",
            "unrestricted",
            { "requested": "workspace_write", "effective": "workspace_write" },
            { "requested": "workspace_write", "effective": "unrestricted" },
            { "requested": "unrestricted", "effective": "unrestricted" }
        ]
    })
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum RawPermissionOutcome {
    /// The shape a `report.json` held before this type existed: a bare grant, with
    /// nothing yet said about what the harness actually enforced.
    Requested(PermissionGrant),
    Explicit {
        requested: PermissionGrant,
        effective: PermissionGrant,
    },
}

impl<'de> Deserialize<'de> for PermissionOutcome {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawPermissionOutcome::deserialize(deserializer)?;
        match raw {
            RawPermissionOutcome::Requested(grant) => Ok(grant.into()),
            RawPermissionOutcome::Explicit { requested, effective } => {
                Self::new(requested, effective).map_err(de::Error::custom)
            }
        }
    }
}

#[cfg(test)]
mod schema_agrees_with_the_parser {
    use super::*;

    fn validator() -> jsonschema::Validator {
        let schema = serde_json::to_value(schemars::schema_for!(PermissionOutcome)).unwrap();
        jsonschema::validator_for(&schema).unwrap()
    }

    /// The schema and `Deserialize` have to refuse the same documents. When the schema is
    /// the looser of the two, `eval verify --mode strict` calls a report conformant and
    /// the next command to read those same bytes fails on them.
    #[test]
    fn the_schema_refuses_every_value_deserialize_refuses() {
        let validator = validator();
        for value in [
            serde_json::json!("acceptEdits"),
            serde_json::json!({}),
            serde_json::json!({ "requested": "unrestricted", "effective": "workspace_write" }),
            serde_json::json!({ "requested": "workspace_write" }),
            serde_json::json!({ "effective": "workspace_write" }),
            serde_json::json!({ "requested": "workspace_write", "effective": "workspace_write", "extra": true }),
        ] {
            assert!(
                serde_json::from_value::<PermissionOutcome>(value.clone()).is_err(),
                "expected Deserialize to refuse {value}"
            );
            assert!(!validator.is_valid(&value), "schema admitted {value}");
        }
    }

    #[test]
    fn the_schema_admits_every_outcome_deserialize_admits() {
        let validator = validator();
        for value in [
            serde_json::json!("workspace_write"),
            serde_json::json!("unrestricted"),
            serde_json::json!({ "requested": "workspace_write", "effective": "workspace_write" }),
            serde_json::json!({ "requested": "workspace_write", "effective": "unrestricted" }),
            serde_json::json!({ "requested": "unrestricted", "effective": "unrestricted" }),
        ] {
            assert!(serde_json::from_value::<PermissionOutcome>(value.clone()).is_ok());
            assert!(validator.is_valid(&value), "schema refused {value}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_with_nothing_to_compare_it_against_has_not_been_widened() {
        let outcome: PermissionOutcome = PermissionGrant::WorkspaceWrite.into();
        assert_eq!(outcome.requested(), PermissionGrant::WorkspaceWrite);
        assert_eq!(outcome.effective(), PermissionGrant::WorkspaceWrite);
        assert!(!outcome.was_widened());
    }

    #[test]
    fn a_harness_that_enforces_more_than_it_was_asked_reports_as_widened() {
        let outcome = PermissionOutcome::new(PermissionGrant::WorkspaceWrite, PermissionGrant::Unrestricted).unwrap();
        assert!(outcome.was_widened());
    }

    #[test]
    fn a_harness_cannot_be_recorded_as_enforcing_less_than_was_requested() {
        assert!(PermissionOutcome::new(PermissionGrant::Unrestricted, PermissionGrant::WorkspaceWrite).is_err());
    }

    #[test]
    fn a_bare_grant_deserializes_as_not_yet_widened() {
        let outcome: PermissionOutcome = serde_json::from_value(serde_json::json!("workspace_write")).unwrap();
        assert_eq!(outcome, PermissionGrant::WorkspaceWrite.into());
    }

    #[test]
    fn default_matches_the_default_grant_with_nothing_to_compare_it_against() {
        assert_eq!(PermissionOutcome::default(), PermissionGrant::default().into());
    }
}

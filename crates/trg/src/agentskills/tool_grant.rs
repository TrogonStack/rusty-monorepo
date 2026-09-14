//! What a run's harness may reach for, by tool name.

use std::collections::BTreeSet;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

/// A named, finite set of tools a run's harness may use.
///
/// Never parsed as empty: naming zero tools is not how an operator or a case denies
/// every gated tool, since a grant with nothing in it reads in a report exactly like a
/// grant nobody declared. [`ToolGrant::narrowed_by`] is the one place an empty set is
/// legitimate, because it answers "what do both sides admit", not "what was declared".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ToolGrant(BTreeSet<String>);

impl ToolGrant {
    pub fn parse(tools: impl IntoIterator<Item = impl Into<String>>) -> Result<Self, String> {
        let mut set = BTreeSet::new();
        for tool in tools {
            let tool = tool.into();
            let trimmed = tool.trim();
            if trimmed.is_empty() {
                return Err("a tool grant must not name an empty tool".to_string());
            }
            set.insert(trimmed.to_string());
        }
        if set.is_empty() {
            return Err(
                "a tool grant naming no tools would deny every gated tool; leave it unset instead of declaring an empty list"
                    .to_string(),
            );
        }
        Ok(Self(set))
    }

    pub fn tools(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    /// Whether this grant names no tools.
    ///
    /// Only ever true of a value [`ToolGrant::narrowed_by`] produced: `parse` refuses an
    /// empty list outright. A caller that reaches an invocation with an empty grant in
    /// hand has to decide what an empty allowlist means to that harness rather than pass
    /// it through, since [`Display`](fmt::Display) and [`Serialize`] both still render it.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The tools both `self` and `other` admit.
    ///
    /// This is how a case's declaration can only narrow an operator's grant, and never
    /// the reverse: whichever side is applied second, the result is bound by both. The
    /// intersection may be empty when the two sides share nothing, which is a legitimate
    /// effective grant (every gated tool denied) rather than a parse error.
    pub fn narrowed_by(&self, other: &ToolGrant) -> ToolGrant {
        ToolGrant(self.0.intersection(&other.0).cloned().collect())
    }
}

impl fmt::Display for ToolGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        write!(f, "{}", self.tools().collect::<Vec<_>>().join(","))
    }
}

impl<'de> Deserialize<'de> for ToolGrant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let tools: Vec<String> = Vec::deserialize(deserializer)?;
        Self::parse(tools).map_err(serde::de::Error::custom)
    }
}

/// The grant a run is actually given once the operator's ceiling and the case's own
/// declaration are combined.
///
/// `None` on both sides means unrestricted: nobody asked for a tool allowlist, so no
/// control is exercised and no harness needs to support one. Naming a grant on either
/// side alone makes that side's set the effective one; naming a grant on both narrows to
/// their intersection, since a case can only cut into the operator's ceiling, never past
/// it.
pub fn effective_tool_grant(operator: Option<&ToolGrant>, case: Option<&ToolGrant>) -> Option<ToolGrant> {
    match (operator, case) {
        (None, None) => None,
        (Some(grant), None) | (None, Some(grant)) => Some(grant.clone()),
        (Some(operator), Some(case)) => Some(operator.narrowed_by(case)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_grant_is_refused() {
        assert!(ToolGrant::parse(Vec::<String>::new()).is_err());
    }

    #[test]
    fn a_blank_tool_name_is_refused() {
        assert!(ToolGrant::parse(["Bash", "  "]).is_err());
    }

    #[test]
    fn duplicate_names_collapse_to_one() {
        let grant = ToolGrant::parse(["Bash", "Bash", "Read"]).unwrap();
        assert_eq!(grant.tools().collect::<Vec<_>>(), vec!["Bash", "Read"]);
    }

    #[test]
    fn narrowing_keeps_only_what_both_sides_admit() {
        let operator = ToolGrant::parse(["Bash", "Read", "Write"]).unwrap();
        let case = ToolGrant::parse(["Read", "Write", "Edit"]).unwrap();
        let effective = operator.narrowed_by(&case);
        assert_eq!(effective.tools().collect::<Vec<_>>(), vec!["Read", "Write"]);
    }

    #[test]
    fn a_case_cannot_widen_the_operators_grant() {
        let operator = ToolGrant::parse(["Read"]).unwrap();
        let case = ToolGrant::parse(["Read", "Bash", "Write"]).unwrap();
        let effective = operator.narrowed_by(&case);
        assert_eq!(effective.tools().collect::<Vec<_>>(), vec!["Read"]);
    }

    #[test]
    fn disjoint_grants_narrow_to_nothing() {
        let operator = ToolGrant::parse(["Bash"]).unwrap();
        let case = ToolGrant::parse(["Read"]).unwrap();
        let effective = operator.narrowed_by(&case);
        assert_eq!(effective.tools().count(), 0);
    }

    #[test]
    fn neither_side_declaring_a_grant_is_unrestricted() {
        assert_eq!(effective_tool_grant(None, None), None);
    }

    #[test]
    fn a_lone_case_declaration_applies_whole_when_the_operator_named_no_ceiling() {
        let case = ToolGrant::parse(["Read", "Grep"]).unwrap();
        let effective = effective_tool_grant(None, Some(&case));
        assert_eq!(effective, Some(case));
    }

    #[test]
    fn a_lone_operator_grant_applies_whole_when_the_case_declares_nothing() {
        let operator = ToolGrant::parse(["Read", "Grep"]).unwrap();
        let effective = effective_tool_grant(Some(&operator), None);
        assert_eq!(effective, Some(operator));
    }

    #[test]
    fn both_sides_declaring_a_grant_narrows_to_their_intersection() {
        let operator = ToolGrant::parse(["Read", "Bash"]).unwrap();
        let case = ToolGrant::parse(["Read"]).unwrap();
        let effective = effective_tool_grant(Some(&operator), Some(&case));
        assert_eq!(effective, Some(ToolGrant::parse(["Read"]).unwrap()));
    }

    #[test]
    fn round_trips_through_json_sorted() {
        let grant = ToolGrant::parse(["Write", "Bash", "Read"]).unwrap();
        let json = serde_json::to_string(&grant).unwrap();
        assert_eq!(json, r#"["Bash","Read","Write"]"#);
        let back: ToolGrant = serde_json::from_str(&json).unwrap();
        assert_eq!(back, grant);
    }

    #[test]
    fn displays_as_a_comma_joined_sorted_list() {
        let grant = ToolGrant::parse(["Write", "Bash"]).unwrap();
        assert_eq!(grant.to_string(), "Bash,Write");
    }
}

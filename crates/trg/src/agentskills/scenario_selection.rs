//! Which scenarios a pass runs when the operator did not say.
//!
//! An eval suite exists to answer whether a skill helped, and that question has no
//! answer from a single arm: a `with_skill` pass alone reports whether the case was
//! covered, not whether the skill changed anything, because there is no baseline in the
//! same report to read it against. So the vector behind `--scenario` is left empty by
//! clap rather than defaulted to `with_skill`, and empty is read here as "the operator
//! expressed no preference" rather than papered over as if it meant `with_skill` by
//! itself. Resolving that fact into an actual set of scenarios to run is the whole job of
//! this type, so the rule lives in one place instead of as a scattered empty-vec check.

use std::collections::HashSet;

use crate::agentskills::report::ScenarioKind;

/// The scenarios one pass has resolved to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioSelection(Vec<ScenarioKind>);

impl ScenarioSelection {
    /// Resolve what a pass runs from what the operator typed.
    ///
    /// Named scenarios are used exactly as given, never widened with an arm the operator
    /// did not ask for. Naming none defaults to both `with_skill` and `without_skill`,
    /// because a pass that never asked for a baseline is exactly the pass that needs one.
    /// The one exception is `--reuse-completed`: reuse serves a prior completed run for
    /// the same case and scenario, so it is inherently single-arm (see the error below),
    /// and defaulting to both arms under it would turn a command that used to work into
    /// one that always fails, for no benefit to the operator who typed nothing extra.
    /// Naming more than one scenario together with `--reuse-completed` stays an error:
    /// reuse would serve one arm's completed run to the other, which compares a run
    /// against itself instead of against a baseline.
    pub fn resolve(requested: &[ScenarioKind], reuse_completed: bool) -> Result<Self, String> {
        if reuse_completed {
            let distinct: HashSet<ScenarioKind> = requested.iter().copied().collect();
            if distinct.len() > 1 {
                return Err(
                    "--reuse-completed cannot be combined with more than one --scenario: a completed run for one scenario is served to the others, so the scenario delta would compare a run against itself. Run one scenario per invocation, or drop --reuse-completed."
                        .to_string(),
                );
            }
        }

        if !requested.is_empty() {
            return Ok(Self(requested.to_vec()));
        }

        if reuse_completed {
            return Ok(Self(vec![ScenarioKind::WithSkill]));
        }

        Ok(Self(vec![ScenarioKind::WithSkill, ScenarioKind::WithoutSkill]))
    }

    pub fn scenarios(&self) -> &[ScenarioKind] {
        &self.0
    }

    pub fn contains(&self, scenario: ScenarioKind) -> bool {
        self.0.contains(&scenario)
    }
}

#[cfg(test)]
#[path = "scenario_selection_tests.rs"]
mod tests;

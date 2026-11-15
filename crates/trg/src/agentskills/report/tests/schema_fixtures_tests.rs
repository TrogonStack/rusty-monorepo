use crate::agentskills::schemas::{validate_artifact, REPORT_SCHEMA};

const FIXTURE_V1: &str = include_str!("../../testdata/reports/v1.json");
const FIXTURE_V1_MINIMAL: &str = include_str!("../../testdata/reports/v1-minimal.json");

fn load_fixture_json(name: &str, content: &str) -> serde_json::Value {
    serde_json::from_str(content).unwrap_or_else(|error| panic!("{name} is valid JSON: {error}"))
}

#[test]
fn report_fixtures_validate_against_schema() {
    for (name, content) in [("v1.json", FIXTURE_V1), ("v1-minimal.json", FIXTURE_V1_MINIMAL)] {
        let value = load_fixture_json(name, content);
        validate_artifact(REPORT_SCHEMA, &value)
            .unwrap_or_else(|error| panic!("{name} validates against REPORT_SCHEMA: {error}"));
    }
}

use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
};
use crate::fs::testutil::MemFS;
use std::path::{Path, PathBuf};

pub fn sample_report_dir(temp: &tempfile::TempDir) -> PathBuf {
    let fs = MemFS::new();
    let skill_path = Path::new("demo-skill");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(
        skill_path.join("evals/evals.json"),
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "prompt a",
                        "expected_output": "output a",
                        "graders": [{ "type": "contains", "text": "assert a" }]
                    }
                ]
            }"#,
    );

    let bundle = build_report_bundle(
        &fs,
        skill_path,
        Path::new("demo-skill"),
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("report-test".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap()
}

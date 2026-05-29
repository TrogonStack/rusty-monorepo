use std::path::{Path, PathBuf};

use schemars::generate::SchemaSettings;
use schemars::transform::{RemoveRefSiblings, ReplaceBoolSchemas};
use schemars::JsonSchema;
use serde_json::{json, Map, Value};

use trg::agentskills::benchmark::BenchmarkDocument;
use trg::agentskills::compare::ComparisonRecord;
use trg::agentskills::evals::EvalSuite;
use trg::agentskills::feedback::FeedbackDocument;
use trg::agentskills::grading::GradingFile;
use trg::agentskills::improvement_bundle::ImprovementBundleDocument;
use trg::agentskills::iteration_summary::IterationSummaryDocument;
use trg::agentskills::report::ReportDocument;
use trg::agentskills::runner::TimingFile;

fn main() -> std::io::Result<()> {
    let schemas_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas");

    write_schema::<BenchmarkDocument>(&schemas_dir, "benchmark.json.schema.json", "trg skills eval benchmark")?;
    write_schema::<ComparisonRecord>(
        &schemas_dir,
        "comparison.json.schema.json",
        "trg skills eval comparison",
    )?;
    write_schema::<EvalSuite>(&schemas_dir, "evals.json.schema.json", "trg skills eval suite")?;
    write_schema::<FeedbackDocument>(&schemas_dir, "feedback.json.schema.json", "trg skills eval feedback")?;
    write_schema::<GradingFile>(&schemas_dir, "grading.json.schema.json", "trg skills eval grading")?;
    write_schema::<ImprovementBundleDocument>(
        &schemas_dir,
        "improvement-bundle.json.schema.json",
        "trg skills eval improvement bundle",
    )?;
    write_schema::<IterationSummaryDocument>(
        &schemas_dir,
        "iteration-summary.json.schema.json",
        "trg skills eval iteration summary",
    )?;
    write_schema::<ReportDocument>(&schemas_dir, "report.json.schema.json", "trg skills eval report")?;
    write_schema::<TimingFile>(&schemas_dir, "timing.json.schema.json", "trg skills eval timing")?;

    Ok(())
}

fn write_schema<T: JsonSchema>(dir: &Path, file_name: &str, title: &str) -> std::io::Result<()> {
    let mut replace_bools = ReplaceBoolSchemas::default();
    replace_bools.skip_additional_properties = true;
    let settings = SchemaSettings::draft2020_12()
        .with_transform(RemoveRefSiblings::default())
        .with_transform(replace_bools);
    let generator = settings.into_generator();
    let mut schema = generator.into_root_schema_for::<T>().to_value();

    if let Value::Object(map) = &mut schema {
        let mut reordered = Map::new();
        if let Some(v) = map.remove("$schema") {
            reordered.insert("$schema".to_string(), v);
        }
        reordered.insert(
            "$id".to_string(),
            json!(format!("https://trogonstack.dev/schemas/trg/{file_name}")),
        );
        map.remove("title");
        reordered.insert("title".to_string(), json!(title));
        for (k, v) in map.iter() {
            reordered.insert(k.clone(), v.clone());
        }
        schema = Value::Object(reordered);
    }

    let path = dir.join(file_name);
    let mut content = serde_json::to_string_pretty(&schema)?;
    content.push('\n');
    std::fs::write(&path, content)?;
    println!("wrote {}", path.display());
    Ok(())
}

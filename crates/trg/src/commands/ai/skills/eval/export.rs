use std::path::PathBuf;

use clap::Args;
use opentelemetry::trace::TraceContextExt;
use serde::Serialize;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::agentskills::eval_suite_drift::load_report_document;
use crate::agentskills::exit_code::ExitCode;
use crate::agentskills::replay::{replay, Replay, ReplayOptions, TracedRuns};
use crate::agentskills::span_reference::SpanReference;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use crate::telemetry::ContentCapture;

use super::print_json;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 trg ai skills eval export ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval export ./report --include-traced --output-format json
")]
pub struct ExportArgs {
    #[arg(help = "Path to a report bundle directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        help = "Also replay runs the report says were already traced live, linking each to its live span"
    )]
    pub include_traced: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

#[derive(Serialize)]
struct ExportJsonOutput<'a> {
    report_dir: String,
    #[serde(flatten)]
    replay: &'a Replay,
}

impl ExportArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        let document = match load_report_document(&self.report_dir) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("Failed to read report bundle: {error}");
                return ExitCode::InfrastructureFailure;
            }
        };
        let options = ReplayOptions {
            traced_runs: if self.include_traced {
                TracedRuns::Replay
            } else {
                TracedRuns::Skip
            },
            content: ContentCapture::from_env(),
            exported_from: SpanReference::of_context(tracing::Span::current().context().span().span_context()),
        };
        let replayed = match replay(&opentelemetry::global::tracer("trg"), &document, &options) {
            Ok(replayed) => replayed,
            Err(error) => {
                eprintln!("Failed to replay report bundle: {error}");
                return ExitCode::InfrastructureFailure;
            }
        };
        if self.output_format.is_json() {
            return print_json(
                &ExportJsonOutput {
                    report_dir: self.report_dir.display().to_string(),
                    replay: &replayed,
                },
                ExitCode::Success,
            );
        }
        print_summary(&replayed);
        ExitCode::Success
    }
}

fn print_summary(replayed: &Replay) {
    match replayed {
        Replay::Exported {
            suite,
            runs,
            skipped_runs,
            live_run_grades,
            skipped_grades,
        } => {
            if let Some(suite) = suite {
                println!("replayed trace: {}", suite.trace_id());
            }
            println!("runs replayed: {}", runs.len());
            if !live_run_grades.is_empty() {
                println!("grades replayed into their live run's trace: {}", live_run_grades.len());
            }
            if !skipped_runs.is_empty() {
                println!(
                    "runs skipped as already traced live: {} (pass --include-traced to replay them)",
                    skipped_runs.len()
                );
            }
            if !skipped_grades.is_empty() {
                println!(
                    "grades skipped as already traced live: {} (pass --include-traced to replay them)",
                    skipped_grades.len()
                );
            }
        }
        Replay::NothingToReplay { skipped_runs, .. } if skipped_runs.is_empty() => {
            eprintln!("Nothing exported: the report has no runs.");
        }
        Replay::NothingToReplay { .. } => {
            eprintln!(
                "Nothing exported: every run and every grade already has a live trace. Pass --include-traced to replay them anyway."
            );
        }
        Replay::TracingOff => {
            eprintln!("Nothing exported: no trace exporter is configured. Set OTEL_EXPORTER_OTLP_ENDPOINT to export.");
        }
    }
}

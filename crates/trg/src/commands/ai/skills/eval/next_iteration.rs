use std::path::PathBuf;

use crate::agentskills::improvement_bundle::{write_improvement_bundle, NextIterationOptions};
use crate::fs::FileSystem;
use clap::Args;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval next-iteration ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval next-iteration --from ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval next-iteration ./report --allow-eval-suite-drift
")]
pub struct NextIterationArgs {
    #[arg(
        value_name = "DIR",
        help = "Path to the previous iteration report directory containing report.json"
    )]
    pub report_dir: Option<PathBuf>,

    #[arg(
        long = "from",
        value_name = "DIR",
        help = "Path to the previous iteration report directory (alternative to positional DIR)"
    )]
    pub from: Option<PathBuf>,

    #[arg(
        long,
        value_name = "DIR",
        help = "Skill directory used to detect eval suite drift (defaults to skill_path from report.json)"
    )]
    pub skill_dir: Option<PathBuf>,

    #[arg(
        long,
        help = "Suppress the warning when the current evals/evals.json hash differs from the prior iteration"
    )]
    pub allow_eval_suite_drift: bool,
}

impl NextIterationArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> i32 {
        let from_dir = match self.from.or(self.report_dir) {
            Some(path) => path,
            None => {
                eprintln!("Missing report directory: pass DIR or --from <DIR>");
                return 1;
            }
        };

        let options = NextIterationOptions {
            allow_eval_suite_drift: self.allow_eval_suite_drift,
            skill_dir: self.skill_dir,
            ..NextIterationOptions::default()
        };

        let output = match write_improvement_bundle(&from_dir, options) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("Failed to build improvement bundle: {error}");
                return 1;
            }
        };

        println!("Improvement bundle written to {}", output.output_dir.display());
        println!("  {}", output.markdown_path.display());
        println!("  {}", output.json_path.display());
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::improvement_bundle::testutil::sample_prior_iteration_fixture;
    use crate::agentskills::improvement_bundle::{BUNDLE_JSON_NAME, BUNDLE_MD_NAME, NEXT_ITERATION_DIR};

    #[test]
    fn next_iteration_command_writes_bundle_from_fixture() {
        let temp = tempfile::tempdir().unwrap();
        let skill_root = temp.path().join("current-skill");
        let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

        let status = NextIterationArgs {
            report_dir: Some(report_dir.clone()),
            from: None,
            skill_dir: Some(skill_root),
            allow_eval_suite_drift: false,
        }
        .handle(&crate::fs::testutil::MemFS::new());
        assert_eq!(status, 0);

        let bundle_dir = report_dir.parent().unwrap().join(NEXT_ITERATION_DIR);
        assert!(bundle_dir.join(BUNDLE_MD_NAME).is_file());
        assert!(bundle_dir.join(BUNDLE_JSON_NAME).is_file());
    }
}

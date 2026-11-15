use super::*;

#[test]
fn help_includes_examples() {
    let help = ScalingArgs::augment_args(clap::Command::new("scaling").about("scaling"))
        .render_long_help()
        .to_string();
    assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
    assert!(help.contains("--fail-on-regression"));
    assert!(help.contains("--failed-runs"));
}

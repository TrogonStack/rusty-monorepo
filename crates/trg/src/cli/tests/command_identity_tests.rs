use clap::CommandFactory;

use super::super::Cli;

fn identity_of(args: &[&str]) -> String {
    let matches = Cli::command()
        .try_get_matches_from(args)
        .unwrap_or_else(|e| panic!("{args:?} should parse: {e}"));
    let (_, identity) = Cli::from_matches(matches).expect("matches build a Cli");
    identity.as_str().to_string()
}

#[test]
fn names_the_full_subcommand_path() {
    assert_eq!(
        identity_of(&["trg", "mcp", "proxy", "--server", "example"]),
        "mcp proxy"
    );
    assert_eq!(identity_of(&["trg", "doctor"]), "doctor");
}

#[test]
fn excludes_positional_arguments_and_flags() {
    assert_eq!(
        identity_of(&["trg", "exec", "list", "--output-format", "json"]),
        "exec list"
    );
}

use super::*;

#[test]
fn quote_for_shell_leaves_bare_names_alone() {
    assert_eq!(quote_for_shell("exa"), "exa");
    assert_eq!(quote_for_shell("my-server_2.0"), "my-server_2.0");
    assert_eq!(quote_for_shell("mcp/memorizer"), "mcp/memorizer");
}

#[test]
fn quote_for_shell_wraps_names_needing_it() {
    assert_eq!(quote_for_shell(""), "''");
    assert_eq!(quote_for_shell("my server"), "'my server'");
    assert_eq!(quote_for_shell("a;rm -rf /"), "'a;rm -rf /'");
    assert_eq!(quote_for_shell("it's"), r"'it'\''s'");
}

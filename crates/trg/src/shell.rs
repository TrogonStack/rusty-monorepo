//! Rendering values into commands a person is meant to copy and run.

/// Quote a value so pasting the command that contains it runs with that value
/// intact.
///
/// Every one of these comes out of the user's own config, so this is about a
/// path with a space in it producing a command that works rather than about an
/// attacker. A command offered as the fix has to be a command.
pub fn quote_for_shell(name: &str) -> String {
    let is_bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '@' | '+' | '=' | ','));

    if is_bare {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
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
}

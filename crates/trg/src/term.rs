//! Whether output may carry ANSI styling, and the little that this crate uses.
//!
//! Gated on stdout rather than on the terminal check the OAuth flow already
//! performs, because that one looks at stdin and stderr: a login whose stdout is
//! redirected still runs, and escape codes in that file are noise every reader
//! has to strip back out.
//!
//! `trg mcp proxy` is the command that must never colour anything, since its
//! stdout is JSON-RPC. It gets that for free here: a proxy is spawned by an
//! editor with stdout on a pipe, so styling is already off.

use std::ffi::OsStr;
use std::io::{stdout, IsTerminal};

/// Green, or the text unchanged where styling would not be read as styling.
pub fn green(text: &str) -> String {
    paint(text, stdout_is_styled())
}

/// Split from the decision so the escape sequence itself can be asserted
/// without a terminal, and the decision without a subprocess.
fn paint(text: &str, styled: bool) -> String {
    if styled {
        return format!("\x1b[32m{text}\x1b[0m");
    }
    text.to_string()
}

fn stdout_is_styled() -> bool {
    if suppresses_color(
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("TERM").as_deref(),
    ) {
        return false;
    }
    stdout().is_terminal()
}

/// `NO_COLOR` suppresses colour when set to anything but the empty string, per
/// <https://no-color.org>. Treating an empty value as set would make `NO_COLOR=`
/// mean the opposite of what someone clearing it intends.
fn suppresses_color(no_color: Option<&OsStr>, term: Option<&OsStr>) -> bool {
    no_color.is_some_and(|value| !value.is_empty()) || term == Some(OsStr::new("dumb"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styling_wraps_and_closes() {
        assert_eq!(paint("hi", true), "\x1b[32mhi\x1b[0m");
    }

    /// The point of the gate: a redirected stdout gets bytes a reader can use.
    #[test]
    fn unstyled_is_the_text_itself() {
        assert_eq!(paint("hi", false), "hi");
    }

    #[test]
    fn no_color_set_to_anything_suppresses() {
        assert!(suppresses_color(Some(OsStr::new("1")), None));
        assert!(suppresses_color(Some(OsStr::new("0")), None));
    }

    /// `NO_COLOR=` is how someone unsets it inline, so it must not suppress.
    #[test]
    fn no_color_set_to_nothing_does_not_suppress() {
        assert!(!suppresses_color(Some(OsStr::new("")), None));
    }

    #[test]
    fn a_dumb_terminal_suppresses() {
        assert!(suppresses_color(None, Some(OsStr::new("dumb"))));
        assert!(!suppresses_color(None, Some(OsStr::new("xterm-256color"))));
    }

    #[test]
    fn neither_set_does_not_suppress() {
        assert!(!suppresses_color(None, None));
    }
}

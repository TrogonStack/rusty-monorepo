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

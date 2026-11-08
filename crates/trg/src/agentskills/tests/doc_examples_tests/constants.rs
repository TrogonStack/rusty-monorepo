use super::Mode;

/// How many blocks of each kind the docs held when this harness was written.
///
/// A harness that silently stops recognising examples passes for the wrong reason, and
/// would keep passing as they rotted. Counting per kind rather than in total is what
/// catches the shape of a suite, a case or a grader changing underneath the inference:
/// every block would still be valid JSON, and none of them would still be checked.
pub const FLOORS: [(&str, Mode, usize); 3] = [
    ("eval suites", Mode::Suite, 1),
    ("eval cases", Mode::Case, 7),
    ("graders", Mode::Grader, 12),
];

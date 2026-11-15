pub const HTML_REPORT_FILENAME: &str = "report.html";

/// How much of a run's final text the report shows inline.
///
/// Matches the excerpt bound the LLM grader already uses for the same file, so a report
/// reader and a judge are looking at a comparably sized slice rather than an unbounded one
/// that could bloat a single-file report.
pub const FINAL_TEXT_EXCERPT_BYTES: usize = 8_000;

pub const STYLE: &str = "<style>\n\
:root { color-scheme: light dark; }\n\
body { font-family: -apple-system, BlinkMacSystemFont, \"Segoe UI\", sans-serif; margin: 0; padding: 1.5rem; line-height: 1.5; background: Canvas; color: CanvasText; }\n\
h1, h2, h3, h4 { line-height: 1.25; }\n\
section { margin-bottom: 2rem; }\n\
table { border-collapse: collapse; width: 100%; }\n\
table.kv th { text-align: left; width: 14rem; }\n\
th, td { padding: 0.25rem 0.75rem 0.25rem 0; vertical-align: top; }\n\
.muted { opacity: 0.7; font-size: 0.9em; }\n\
.warn { color: #b45309; }\n\
.case { border-top: 1px solid ButtonBorder; padding-top: 1rem; margin-top: 1rem; }\n\
.arm { margin-left: 1rem; margin-bottom: 1rem; }\n\
.run { margin: 0.5rem 0 0.5rem 1rem; border: 1px solid ButtonBorder; border-radius: 4px; padding: 0.5rem 0.75rem; }\n\
.run summary { cursor: pointer; font-weight: 600; }\n\
.status-ok { color: #2e7d32; }\n\
.status-bad { color: #c62828; }\n\
.status-skip { color: #8a6d00; }\n\
.status-unknown { color: #5c5c5c; }\n\
ul.assertions { list-style: none; padding-left: 0; }\n\
ul.assertions li { border-left: 3px solid ButtonBorder; padding: 0.25rem 0 0.25rem 0.6rem; margin-bottom: 0.5rem; }\n\
.outcome-passed { border-left-color: #2e7d32; }\n\
.outcome-failed { border-left-color: #c62828; }\n\
.outcome-unsupported { border-left-color: #8a6d00; }\n\
.outcome-excluded { border-left-color: #5c5c5c; }\n\
.evidence { white-space: pre-wrap; }\n\
pre { white-space: pre-wrap; word-break: break-word; background: color-mix(in srgb, CanvasText 6%, Canvas); padding: 0.5rem; border-radius: 4px; }\n\
ul.artifacts { padding-left: 1.2rem; }\n\
</style>\n";

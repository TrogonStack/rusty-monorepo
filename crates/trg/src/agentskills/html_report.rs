//! A local-only, single-file HTML report over a report bundle that already exists.
//!
//! This is a rendering step: it reads `report.json` and each run's `grading.json` and
//! `outputs/final.md`, and writes `report.html` beside them. It computes no new metric and
//! stores nothing that was not already on disk.
//!
//! Every string it interpolates originates from an agent under evaluation and must be
//! treated as adversarial: a skill under test can deliberately emit `<script>` or
//! `onerror=` in its final text, an assertion's evidence, a judge's rationale, or a file
//! name it created. [`Escaped`] is the single chokepoint every such value passes through
//! before it reaches the page; nothing here builds markup by concatenating a raw string.
//!
//! The page never references the network: no remote stylesheet, script, font, or image,
//! and no `<script>` at all, so there is no way for an adversarial value to run as code
//! even if escaping were somehow bypassed. Interactivity (collapsing a run's detail) uses
//! plain `<details>`/`<summary>`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::eval_suite_drift::load_report_document;
use super::evals::Result;
use super::grading::{AssertionGradeResult, GraderInfo, GraderKind, GradingFile, GradingSummary};
use super::outputs::{FINAL_MD, OUTPUTS_DIR};
use super::report::{EvalCaseDimension, ReportDocument, RunRecord, ScenarioKind, ScenarioSummary};

pub const HTML_REPORT_FILENAME: &str = "report.html";

/// How much of a run's final text the report shows inline.
///
/// Matches the excerpt bound the LLM grader already uses for the same file, so a report
/// reader and a judge are looking at a comparably sized slice rather than an unbounded one
/// that could bloat a single-file report.
const FINAL_TEXT_EXCERPT_BYTES: usize = 8_000;

/// The single place every untrusted string passes through before reaching the page.
///
/// `Escaped::new` is the only constructor. A caller holding an `Escaped` can push it
/// straight into the page through its `Display` impl; there is no second, forgetful way to
/// get a raw string into the markup, so a field added later cannot silently skip escaping
/// by being formatted directly.
pub struct Escaped(String);

impl Escaped {
    pub fn new(raw: &str) -> Self {
        let mut out = String::with_capacity(raw.len());
        for ch in raw.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&#39;"),
                other => out.push(other),
            }
        }
        Self(out)
    }
}

impl std::fmt::Display for Escaped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn esc(raw: &str) -> Escaped {
    Escaped::new(raw)
}

/// A bundle-relative path on its way into an `href`.
///
/// Escaping and encoding answer different questions, and a link needs the second one.
/// `Escaped` leaves `#`, `?` and `%` untouched because they are harmless in text, but a
/// browser reads them in a URL as a fragment, a query and an escape. Artifact filenames
/// are chosen by the agent under evaluation, so all three are reachable, and a link
/// carrying one silently opens the wrong target or nothing at all.
///
/// Every byte outside the unreserved set is percent-encoded and only `/` survives as a
/// separator, which also leaves nothing that could close the attribute it sits in.
pub struct Href(String);

impl Href {
    pub fn new(relative_path: &str) -> Self {
        let mut out = String::with_capacity(relative_path.len());
        for byte in relative_path.as_bytes() {
            match byte {
                b'/' => out.push('/'),
                b if b.is_ascii_alphanumeric() => out.push(char::from(*b)),
                b'-' | b'.' | b'_' | b'~' => out.push(char::from(*byte)),
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        Self(out)
    }
}

impl std::fmt::Display for Href {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reads the bundle at `report_dir` and writes `report.html` beside `report.json`.
///
/// Overwrites any existing `report.html` unconditionally, the same way sibling commands
/// like `eval benchmark` rewrite their own derived artifact on every run.
pub fn write_html_report(report_dir: &Path) -> Result<PathBuf> {
    let document = load_report_document(report_dir)?;
    let html = render_report_html(&document, report_dir);
    let out_path = report_dir.join(HTML_REPORT_FILENAME);
    fs::write(&out_path, html)?;
    Ok(out_path)
}

pub fn render_report_html(document: &ReportDocument, report_dir: &Path) -> String {
    let gradings: HashMap<&str, Option<GradingFile>> = document
        .runs
        .iter()
        .map(|run| (run.id.as_str(), load_run_grading(report_dir, run)))
        .collect();

    let mut totals = ReportTotals::default();
    for grading in gradings.values().flatten() {
        totals.add(&grading.summary);
    }

    let mut out = String::new();
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str(&format!(
        "<title>trg eval report: {}</title>\n",
        esc(&document.suite.skill_name)
    ));
    out.push_str(STYLE);
    out.push_str("</head>\n<body>\n");
    out.push_str(&render_header(document));
    out.push_str(&render_provenance(document));
    out.push_str(&render_summary(document, totals));
    out.push_str(&render_cases(document, report_dir, &gradings));
    out.push_str("</body>\n</html>\n");
    out
}

/// Totals over every scored and non-scoring assertion in the bundle.
///
/// Built from each run's already-computed [`GradingSummary`] rather than re-tallying
/// `assertion_results`, so the report cannot drift from the counts `eval grade` wrote.
#[derive(Debug, Clone, Copy, Default)]
struct ReportTotals {
    passed: usize,
    failed: usize,
    unsupported: usize,
    excluded: usize,
}

impl ReportTotals {
    fn add(&mut self, summary: &GradingSummary) {
        self.passed += summary.passed;
        self.failed += summary.failed;
        self.unsupported += summary.unsupported;
        self.excluded += summary.excluded;
    }

    fn scored(self) -> usize {
        self.passed + self.failed
    }

    fn total(self) -> usize {
        self.scored() + self.unsupported + self.excluded
    }

    fn pass_rate(self) -> Option<f64> {
        let scored = self.scored();
        if scored == 0 {
            None
        } else {
            Some(self.passed as f64 / scored as f64)
        }
    }
}

fn render_header(document: &ReportDocument) -> String {
    format!(
        "<header>\n<h1>{skill}</h1>\n<p class=\"muted\">report {id} &middot; iteration {iteration} &middot; generated {generated_at}</p>\n</header>\n",
        skill = esc(&document.suite.skill_name),
        id = esc(&document.report.id),
        iteration = document.report.iteration,
        generated_at = esc(&document.report.generated_at),
    )
}

fn render_provenance(document: &ReportDocument) -> String {
    let mut rows = Vec::new();
    rows.push(kv_row(
        "producer",
        &format!("{} {}", document.report.producer.name, document.report.producer.version),
    ));
    if let Some(runner) = &document.report.runner {
        rows.push(kv_row("runner", runner));
    }
    if let Some(binary) = &document.report.runner_binary {
        rows.push(kv_row("runner binary", binary));
    }
    if let Some(version) = &document.report.runner_version {
        rows.push(kv_row("runner version", version));
    }
    rows.push(kv_row("environment", document.report.environment.as_str()));
    rows.push(kv_row("permission", document.report.permission.as_str()));
    if let Some(ci) = &document.report.ci {
        rows.push(kv_row("ci provider", &ci.provider));
        if let Some(run_id) = &ci.run_id {
            rows.push(kv_row("ci run", run_id));
        }
        if let Some(workflow) = &ci.workflow {
            rows.push(kv_row("ci workflow", workflow));
        }
        if let Some(commit) = &ci.commit {
            rows.push(kv_row("ci commit", commit));
        }
    }

    rows.push(kv_row("skill path", &document.suite.skill_path));
    rows.push(kv_row("skill hash", &document.suite.skill_hash));
    rows.push(kv_row("evals path", &document.suite.evals_path));
    rows.push(kv_row("evals hash", &document.suite.evals_hash));
    if let Some(old_path) = &document.suite.old_skill_path {
        rows.push(kv_row("old skill path", old_path));
    }
    if let Some(old_hash) = &document.suite.old_skill_hash {
        rows.push(kv_row("old skill hash", old_hash));
    }
    if let Some(selection) = &document.suite.case_selection {
        rows.push(kv_row(
            "case coverage",
            &format!("{} of {} declared cases", selection.covered, selection.declared.len()),
        ));
        if !selection.cases.is_empty() {
            rows.push(kv_row("cases requested", &selection.cases.join(", ")));
        }
        if !selection.tags.is_empty() {
            rows.push(kv_row("tags requested", &selection.tags.join(", ")));
        }
    }

    format!(
        "<section>\n<h2>Provenance</h2>\n<table class=\"kv\">\n{}\n</table>\n</section>\n",
        rows.join("\n")
    )
}

fn kv_row(label: &str, value: &str) -> String {
    format!("<tr><th>{}</th><td>{}</td></tr>", esc(label), esc(value))
}

fn render_summary(document: &ReportDocument, totals: ReportTotals) -> String {
    let mut out = String::new();
    out.push_str("<section>\n<h2>Summary</h2>\n");

    out.push_str(&render_totals_bar(totals));

    out.push_str("<table>\n<thead><tr><th>scenario</th><th>runs</th><th>passed</th><th>skipped</th><th>failed</th></tr></thead>\n<tbody>\n");
    for scenario in &document.summaries.by_scenario {
        out.push_str(&render_scenario_summary_row(scenario));
    }
    out.push_str("</tbody>\n</table>\n");

    if let Some(budget) = &document.budget {
        let spent = budget
            .spent
            .usd()
            .map(|usd| format!("${usd:.2}"))
            .unwrap_or_else(|| "unpriced".to_string());
        let ceiling = budget
            .ceiling_usd
            .map(|usd| format!("${usd:.2}"))
            .unwrap_or_else(|| "none".to_string());
        out.push_str(&format!(
            "<p class=\"muted\">budget: spent {spent} of {ceiling}{exhausted}, {skipped} run(s) skipped for budget</p>\n",
            exhausted = if budget.exhausted { ", ceiling reached" } else { "" },
            skipped = budget.runs_skipped,
        ));
    }

    out.push_str("</section>\n");
    out
}

fn render_scenario_summary_row(scenario: &ScenarioSummary) -> String {
    format!(
        "<tr><td>{scenario}</td><td>{total}</td><td>{passed}</td><td>{skipped}</td><td>{failed}</td></tr>\n",
        scenario = esc(scenario.scenario_id.as_str()),
        total = scenario.total_runs,
        passed = scenario.passed_runs,
        skipped = scenario.skipped_runs,
        failed = scenario.failed_runs,
    )
}

/// A small inline SVG bar splitting every scored and non-scoring assertion in the bundle.
///
/// Drawn from plain numbers computed in this module, not from a charting library and not
/// from any value an agent under evaluation controls, so there is nothing here to escape.
fn render_totals_bar(totals: ReportTotals) -> String {
    let total = totals.total();
    if total == 0 {
        return "<p class=\"muted\">no assertions have been graded yet</p>\n".to_string();
    }

    let width = 600.0_f64;
    let segments = [
        (totals.passed, "#2e7d32"),
        (totals.failed, "#c62828"),
        (totals.unsupported, "#8a6d00"),
        (totals.excluded, "#5c5c5c"),
    ];

    let mut x = 0.0_f64;
    let mut rects = String::new();
    for (count, color) in segments {
        if count == 0 {
            continue;
        }
        let segment_width = width * (count as f64 / total as f64);
        rects.push_str(&format!(
            "<rect x=\"{x:.2}\" y=\"0\" width=\"{segment_width:.2}\" height=\"24\" fill=\"{color}\"></rect>"
        ));
        x += segment_width;
    }

    let pass_rate = totals
        .pass_rate()
        .map(|rate| format!("{:.1}%", rate * 100.0))
        .unwrap_or_else(|| "not scored".to_string());

    format!(
        "<p><svg width=\"{width}\" height=\"24\" viewBox=\"0 0 {width} 24\" role=\"img\" aria-label=\"assertion outcomes\">{rects}</svg></p>\n\
         <p class=\"muted\">pass rate over scored assertions: {pass_rate} ({scored} scored of {total} total) &middot; \
         passed {passed}, failed {failed}, unsupported {unsupported}, excluded {excluded}</p>\n",
        width = width as u32,
        scored = totals.scored(),
        passed = totals.passed,
        failed = totals.failed,
        unsupported = totals.unsupported,
        excluded = totals.excluded,
    )
}

/// One eval case's runs, grouped by scenario ("arm").
struct CaseGroup<'a> {
    case_id: &'a str,
    dimension: Option<&'a EvalCaseDimension>,
    arms: Vec<ArmGroup<'a>>,
}

/// One scenario's runs for a single case, in attempt order.
struct ArmGroup<'a> {
    scenario: ScenarioKind,
    runs: Vec<&'a RunRecord>,
}

fn group_runs_by_case_and_arm(document: &ReportDocument) -> Vec<CaseGroup<'_>> {
    let dimension_by_id: HashMap<&str, &EvalCaseDimension> = document
        .dimensions
        .eval_cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect();

    let mut case_order: Vec<&str> = document
        .dimensions
        .eval_cases
        .iter()
        .map(|case| case.id.as_str())
        .collect();
    for run in &document.runs {
        if !case_order.contains(&run.eval_case_id.as_str()) {
            case_order.push(&run.eval_case_id);
        }
    }

    case_order
        .into_iter()
        .map(|case_id| {
            let mut runs_for_case: Vec<&RunRecord> =
                document.runs.iter().filter(|run| run.eval_case_id == case_id).collect();
            runs_for_case.sort_by_key(|run| (run.scenario_id, run.attempt));

            let mut arms: Vec<ArmGroup<'_>> = Vec::new();
            for run in runs_for_case {
                match arms.last_mut() {
                    Some(arm) if arm.scenario == run.scenario_id => arm.runs.push(run),
                    _ => arms.push(ArmGroup {
                        scenario: run.scenario_id,
                        runs: vec![run],
                    }),
                }
            }

            CaseGroup {
                case_id,
                dimension: dimension_by_id.get(case_id).copied(),
                arms,
            }
        })
        .collect()
}

fn render_cases(document: &ReportDocument, report_dir: &Path, gradings: &HashMap<&str, Option<GradingFile>>) -> String {
    let mut out = String::new();
    out.push_str("<section>\n<h2>Cases</h2>\n");

    for group in group_runs_by_case_and_arm(document) {
        out.push_str(&render_case_group(&group, report_dir, gradings));
    }

    out.push_str("</section>\n");
    out
}

fn render_case_group(
    group: &CaseGroup<'_>,
    report_dir: &Path,
    gradings: &HashMap<&str, Option<GradingFile>>,
) -> String {
    let heading = match group.dimension.and_then(|dim| dim.name.as_deref()) {
        Some(name) => format!("{} <span class=\"muted\">({})</span>", esc(name), esc(group.case_id)),
        None => esc(group.case_id).to_string(),
    };

    let mut out = format!("<article class=\"case\">\n<h3>{heading}</h3>\n");

    if let Some(dimension) = group.dimension {
        if let Some(description) = &dimension.description {
            out.push_str(&format!("<p class=\"muted\">{}</p>\n", esc(description)));
        }
        if !dimension.files.is_empty() {
            let files = dimension
                .files
                .iter()
                .map(|file| esc(file).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("<p class=\"muted\">fixture files: {files}</p>\n"));
        }
    }

    for arm in &group.arms {
        out.push_str(&render_arm_group(arm, report_dir, gradings));
    }

    out.push_str("</article>\n");
    out
}

fn render_arm_group(arm: &ArmGroup<'_>, report_dir: &Path, gradings: &HashMap<&str, Option<GradingFile>>) -> String {
    let mut out = format!("<div class=\"arm\">\n<h4>{}</h4>\n", esc(arm.scenario.as_str()));
    for run in &arm.runs {
        let grading = gradings.get(run.id.as_str()).and_then(|found| found.as_ref());
        out.push_str(&render_run(run, report_dir, grading));
    }
    out.push_str("</div>\n");
    out
}

fn run_status_class(status: &str) -> &'static str {
    match status {
        "completed" => "status-ok",
        "failed" | "timeout" => "status-bad",
        "skipped" => "status-skip",
        _ => "status-unknown",
    }
}

fn render_run(run: &RunRecord, report_dir: &Path, grading: Option<&GradingFile>) -> String {
    let mut out = format!(
        "<details class=\"run\">\n<summary><span class=\"{status_class}\">{status}</span> attempt {attempt} &middot; {run_id}</summary>\n",
        status_class = run_status_class(&run.status),
        status = esc(&run.status),
        attempt = run.attempt,
        run_id = esc(&run.id),
    );

    if let Some(failure_kind) = &run.failure_kind {
        out.push_str(&format!("<p class=\"muted\">failure kind: {}</p>\n", esc(failure_kind)));
    }

    out.push_str(&render_run_metrics(run));

    if let Some(cache) = &run.cache {
        out.push_str(&format!(
            "<p class=\"muted\">cache: {hit} (source run {source}, key {key})</p>\n",
            hit = if cache.hit { "hit" } else { "miss" },
            source = esc(&cache.source_run_id),
            key = esc(&cache.key),
        ));
    }

    if let Some(integrity) = &run.skill_integrity {
        if integrity.tampered {
            let files = if integrity.tampered_files.is_empty() {
                "unverifiable (directory could not be re-read)".to_string()
            } else {
                integrity
                    .tampered_files
                    .iter()
                    .map(|file| esc(file).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            out.push_str(&format!(
                "<p class=\"warn\">skill changed since this run: {files}</p>\n"
            ));
        }
    }

    if !run.warnings.is_empty() {
        out.push_str("<ul class=\"warn\">\n");
        for warning in &run.warnings {
            out.push_str(&format!("<li>{}</li>\n", esc(warning)));
        }
        out.push_str("</ul>\n");
    }

    out.push_str(&render_run_artifacts(run));

    if let Some(final_text) = load_run_final_text(report_dir, run) {
        out.push_str(&format!(
            "<details class=\"final-text\"><summary>final text</summary>\n<pre>{}</pre>\n</details>\n",
            esc(&final_text)
        ));
    }

    match grading {
        Some(grading) => out.push_str(&render_grading(grading)),
        None => out.push_str("<p class=\"muted\">not graded</p>\n"),
    }

    out.push_str("</details>\n");
    out
}

fn render_run_metrics(run: &RunRecord) -> String {
    let mut parts = Vec::new();
    if let Some(duration_ms) = run.metrics.duration_ms {
        parts.push(format!("{duration_ms} ms"));
    }
    if let Some(total_tokens) = run.metrics.total_tokens {
        parts.push(format!("{total_tokens} tokens"));
    }
    if let Some(cost_usd) = run.metrics.cost_usd {
        parts.push(format!("${cost_usd:.4}"));
    }
    if let Some(exit_code) = run.metrics.exit_code {
        parts.push(format!("exit {exit_code}"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("<p class=\"muted\">{}</p>\n", esc(&parts.join(" \u{b7} ")))
}

fn render_run_artifacts(run: &RunRecord) -> String {
    let entries: Vec<(&str, &str)> = run.artifacts.iter().filter_map(artifact_kind_and_path).collect();
    if entries.is_empty() {
        return String::new();
    }

    let mut out = String::from("<ul class=\"artifacts\">\n");
    for (kind, path) in entries {
        out.push_str(&format!(
            "<li>{kind}: <a href=\"./{href}\">{path}</a></li>\n",
            kind = esc(kind),
            href = Href::new(path),
            path = esc(path),
        ));
    }
    out.push_str("</ul>\n");
    out
}

fn artifact_kind_and_path(artifact: &Value) -> Option<(&str, &str)> {
    let path = artifact.get("path")?.as_str()?;
    let kind = artifact.get("kind").and_then(Value::as_str).unwrap_or("artifact");
    Some((kind, path))
}

fn render_grading(grading: &GradingFile) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<p class=\"muted\">{passed}/{scored} scored assertions passed{unsupported}{excluded}</p>\n",
        passed = grading.summary.passed,
        scored = grading.summary.passed + grading.summary.failed,
        unsupported = if grading.summary.unsupported > 0 {
            format!(", {} unsupported", grading.summary.unsupported)
        } else {
            String::new()
        },
        excluded = if grading.summary.excluded > 0 {
            format!(", {} excluded", grading.summary.excluded)
        } else {
            String::new()
        },
    ));

    out.push_str("<ul class=\"assertions\">\n");
    for assertion in &grading.assertion_results {
        out.push_str(&render_assertion(assertion));
    }
    out.push_str("</ul>\n");
    out
}

fn assertion_outcome_label(assertion: &AssertionGradeResult) -> &'static str {
    if assertion.is_excluded() {
        "excluded"
    } else if assertion.is_unsupported() {
        "unsupported"
    } else if assertion.passed {
        "passed"
    } else {
        "failed"
    }
}

fn grader_kind_label(kind: GraderKind) -> &'static str {
    match kind {
        GraderKind::Mechanical => "mechanical",
        GraderKind::Declarative => "declarative",
        GraderKind::Llm => "llm",
        GraderKind::Script => "script",
        GraderKind::NeedsLlm => "needs_llm",
        GraderKind::None => "none",
    }
}

fn render_grader_info(grader: &GraderInfo) -> String {
    let mut parts = vec![grader_kind_label(grader.kind).to_string()];
    if let Some(model) = &grader.model {
        parts.push(esc(model).to_string());
    }
    if let Some(command) = &grader.command {
        parts.push(esc(command).to_string());
    }
    parts.join(" ")
}

fn render_assertion(assertion: &AssertionGradeResult) -> String {
    let outcome = assertion_outcome_label(assertion);
    let mut out = format!(
        "<li class=\"outcome-{outcome}\"><strong>[{outcome}]</strong> {text}",
        text = esc(&assertion.assertion),
    );

    if let Some(name) = &assertion.name {
        out.push_str(&format!(" <span class=\"muted\">({})</span>", esc(name)));
    }

    out.push_str(&format!(
        "<br><span class=\"evidence\">evidence: {}</span>",
        esc(&assertion.evidence)
    ));

    if let Some(rationale) = &assertion.rationale {
        out.push_str(&format!(
            "<br><span class=\"muted\">rationale: {}</span>",
            esc(rationale)
        ));
    }

    if let Some(reason) = &assertion.unsupported {
        out.push_str(&format!(
            "<br><span class=\"muted\">unsupported: {}</span>",
            esc(reason)
        ));
    }

    if let Some(reason) = &assertion.excluded {
        out.push_str(&format!("<br><span class=\"muted\">excluded: {}</span>", esc(reason)));
    }

    if let Some(votes) = assertion.votes {
        out.push_str(&format!(
            "<br><span class=\"muted\">votes: {}/{} passed</span>",
            votes.passed,
            votes.total()
        ));
    }

    out.push_str(&format!(
        "<br><span class=\"muted\">grader: {}</span></li>\n",
        render_grader_info(&assertion.grader)
    ));
    out
}

fn run_dir_and_workspace_dir(report_dir: &Path, run: &RunRecord) -> (PathBuf, PathBuf) {
    let workspace_dir = report_dir.join(&run.paths.workspace);
    let run_dir = workspace_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| workspace_dir.clone());
    (run_dir, workspace_dir)
}

/// Finds and parses a run's `grading.json`.
///
/// `eval grade` writes it beside `workspace/`, in the run directory; some fixtures in this
/// crate's own tests write it directly into the workspace instead. Both are checked, the
/// same way `iteration-summary` and `verify` already look in both places.
fn load_run_grading(report_dir: &Path, run: &RunRecord) -> Option<GradingFile> {
    let (run_dir, workspace_dir) = run_dir_and_workspace_dir(report_dir, run);
    [run_dir.join("grading.json"), workspace_dir.join("grading.json")]
        .into_iter()
        .find_map(|path| fs::read_to_string(path).ok())
        .and_then(|content| serde_json::from_str(&content).ok())
}

fn load_run_final_text(report_dir: &Path, run: &RunRecord) -> Option<String> {
    let workspace_dir = report_dir.join(&run.paths.workspace);
    let final_path = workspace_dir.join(OUTPUTS_DIR).join(FINAL_MD);
    fs::read_to_string(final_path).ok().map(|text| excerpt(&text))
}

fn excerpt(text: &str) -> String {
    if text.len() <= FINAL_TEXT_EXCERPT_BYTES {
        return text.to_string();
    }
    let mut end = FINAL_TEXT_EXCERPT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated after {end} bytes]", &text[..end])
}

const STYLE: &str = "<style>\n\
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::grading::{GraderInfo, GraderKind};
    use crate::agentskills::report::{
        CiSection, DimensionsSection, ProducerSection, ReportSection, RunMetrics, RunPaths, SuiteSection,
        SummariesSection,
    };

    const SCRIPT_PAYLOAD: &str = "<script>alert(1)</script>";
    const IMG_PAYLOAD: &str = "\"><img src=x onerror=alert(1)>";

    fn minimal_document() -> ReportDocument {
        ReportDocument {
            report: ReportSection {
                id: "report-1".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                iteration: 1,
                producer: ProducerSection {
                    name: "trg".to_string(),
                    version: "0.0.0".to_string(),
                },
                runner: None,
                runner_binary: None,
                runner_version: None,
                environment: Default::default(),
                permission: Default::default(),
                ci: None::<CiSection>,
            },
            suite: SuiteSection {
                skill_name: "demo-skill".to_string(),
                skill_path: "demo-skill".to_string(),
                skill_hash: "sha256:abc".to_string(),
                evals_path: "demo-skill/evals/evals.json".to_string(),
                evals_hash: "sha256:def".to_string(),
                old_skill_path: None,
                old_skill_hash: None,
                case_selection: None,
            },
            dimensions: DimensionsSection {
                eval_cases: Vec::new(),
                assertions: Vec::new(),
                skill_revisions: Vec::new(),
                model_configs: Vec::new(),
                scenarios: Vec::new(),
                graders: Vec::new(),
            },
            runs: vec![RunRecord {
                id: "run-001".to_string(),
                eval_case_id: SCRIPT_PAYLOAD.to_string(),
                eval_slug: "case".to_string(),
                scenario_id: ScenarioKind::WithSkill,
                iteration: 1,
                model_config_id: "default".to_string(),
                skill_revision_id: "current".to_string(),
                attempt: 1,
                status: "completed".to_string(),
                runner_invocations: 1,
                failure_kind: None,
                paths: RunPaths {
                    workspace: "runs/run-001/workspace".to_string(),
                    outputs: "runs/run-001/workspace/outputs".to_string(),
                },
                mirror_path: "iteration-1/eval-case/with_skill/".to_string(),
                artifacts: vec![serde_json::json!({
                    "kind": "output",
                    "path": IMG_PAYLOAD,
                })],
                metrics: RunMetrics::default(),
                cache: None,
                skill_integrity: None,
                warnings: Vec::new(),
            }],
            assertion_results: Vec::new(),
            summaries: SummariesSection {
                by_scenario: Vec::new(),
                human_feedback: None,
            },
            improvement_feedback: Vec::new(),
            comparisons: Vec::new(),
            iteration_summary: None,
            budget: None,
        }
    }

    #[test]
    fn escapes_the_five_reserved_characters() {
        assert_eq!(esc("a & b").to_string(), "a &amp; b");
        assert_eq!(esc("<tag>").to_string(), "&lt;tag&gt;");
        assert_eq!(esc("\"quoted\"").to_string(), "&quot;quoted&quot;");
        assert_eq!(esc("it's").to_string(), "it&#39;s");
    }

    #[test]
    fn an_href_percent_encodes_what_a_browser_would_otherwise_read_as_url_syntax() {
        assert_eq!(Href::new("outputs/a#b.md").to_string(), "outputs/a%23b.md");
        assert_eq!(Href::new("outputs/a?b.md").to_string(), "outputs/a%3Fb.md");
        assert_eq!(Href::new("outputs/a%2Fb.md").to_string(), "outputs/a%252Fb.md");
        assert_eq!(Href::new("outputs/my report.md").to_string(), "outputs/my%20report.md");
    }

    #[test]
    fn an_href_keeps_separators_and_unreserved_characters_intact() {
        assert_eq!(
            Href::new("outputs/sub-dir/final_v1.2~draft.md").to_string(),
            "outputs/sub-dir/final_v1.2~draft.md"
        );
    }

    #[test]
    fn an_href_cannot_close_the_attribute_it_sits_in() {
        let rendered = Href::new(IMG_PAYLOAD).to_string();
        for reserved in ['"', '<', '>', '\'', '&'] {
            assert!(!rendered.contains(reserved), "href still carries {reserved}");
        }
    }

    #[test]
    fn injected_markup_in_an_assertion_evidence_string_does_not_survive_rendering() {
        let assertion = AssertionGradeResult {
            assertion: "outputs/report.md exists".to_string(),
            passed: true,
            evidence: SCRIPT_PAYLOAD.to_string(),
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            name: None,
            rationale: Some(IMG_PAYLOAD.to_string()),
            unsupported: None,
            excluded: None,
            votes: None,
        };

        let html = render_assertion(&assertion);

        assert!(!html.contains(SCRIPT_PAYLOAD));
        assert!(!html.contains(IMG_PAYLOAD));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&quot;&gt;&lt;img src=x onerror=alert(1)&gt;"));
    }

    #[test]
    fn injected_markup_in_a_file_path_does_not_survive_rendering() {
        let mut run = minimal_run();
        run.artifacts = vec![serde_json::json!({ "kind": "output", "path": IMG_PAYLOAD })];

        let html = render_run_artifacts(&run);

        assert!(!html.contains(IMG_PAYLOAD));
        assert!(html.contains("&quot;&gt;&lt;img src=x onerror=alert(1)&gt;"));
    }

    #[test]
    fn injected_markup_in_a_runs_final_text_does_not_survive_rendering() {
        let report_dir = tempfile::tempdir().unwrap();
        let run = minimal_run();
        let outputs_dir = report_dir.path().join(&run.paths.workspace).join(OUTPUTS_DIR);
        fs::create_dir_all(&outputs_dir).unwrap();
        fs::write(outputs_dir.join(FINAL_MD), SCRIPT_PAYLOAD).unwrap();

        let final_text = load_run_final_text(report_dir.path(), &run).expect("final.md was written");
        let html = format!("<pre>{}</pre>", esc(&final_text));

        assert!(!html.contains(SCRIPT_PAYLOAD));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn a_full_report_with_adversarial_ids_never_leaks_raw_markup() {
        let document = minimal_document();
        let temp = tempfile::tempdir().unwrap();
        let html = render_report_html(&document, temp.path());

        assert!(!html.contains(SCRIPT_PAYLOAD));
        assert!(!html.contains(IMG_PAYLOAD));
    }

    fn minimal_run() -> RunRecord {
        RunRecord {
            id: "run-001".to_string(),
            eval_case_id: "case".to_string(),
            eval_slug: "case".to_string(),
            scenario_id: ScenarioKind::WithSkill,
            iteration: 1,
            model_config_id: "default".to_string(),
            skill_revision_id: "current".to_string(),
            attempt: 1,
            status: "completed".to_string(),
            runner_invocations: 1,
            failure_kind: None,
            paths: RunPaths {
                workspace: "runs/run-001/workspace".to_string(),
                outputs: "runs/run-001/workspace/outputs".to_string(),
            },
            mirror_path: "iteration-1/eval-case/with_skill/".to_string(),
            artifacts: Vec::new(),
            metrics: RunMetrics::default(),
            cache: None,
            skill_integrity: None,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn write_html_report_reads_the_bundle_and_writes_report_html() {
        let report_dir = tempfile::tempdir().unwrap();
        let document = minimal_document();
        fs::write(
            report_dir.path().join("report.json"),
            serde_json::to_string_pretty(&document).unwrap(),
        )
        .unwrap();

        let out_path = write_html_report(report_dir.path()).unwrap();

        assert_eq!(out_path, report_dir.path().join(HTML_REPORT_FILENAME));
        let html = fs::read_to_string(&out_path).unwrap();
        assert!(html.contains("demo-skill"));
        assert!(!html.contains(SCRIPT_PAYLOAD));
    }

    #[test]
    fn render_report_html_references_nothing_over_the_network() {
        let document = minimal_document();
        let temp = tempfile::tempdir().unwrap();
        let html = render_report_html(&document, temp.path());

        for marker in ["http://", "https://", "//cdn", "<script"] {
            assert!(!html.contains(marker), "found network or script marker: {marker}");
        }
    }
}

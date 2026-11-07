use clippy_utils::diagnostics::span_lint_and_then;
use rustc_ast::Attribute;
use rustc_lint::{EarlyContext, EarlyLintPass};
use rustc_session::lint::Lint;

use crate::POLICY_LINT_SUPPRESSION_WITHOUT_REASON;
use crate::serde_json_macro_allow_without_reason::{is_serde_json_macro, is_stated_reason};

pub(crate) struct PolicyLintSuppressionWithoutReason {
    lints: &'static [&'static Lint],
}

impl PolicyLintSuppressionWithoutReason {
    pub(crate) fn new(lints: &'static [&'static Lint]) -> Self {
        Self { lints }
    }
}

impl EarlyLintPass for PolicyLintSuppressionWithoutReason {
    fn check_attribute(&mut self, cx: &EarlyContext<'_>, attr: &Attribute) {
        let Some(level) = attr.name() else {
            return;
        };
        if !matches!(level.as_str(), "allow" | "expect") {
            return;
        }

        let Some(items) = attr.meta_item_list() else {
            return;
        };
        // The legacy serde-specific check already diagnoses the whole attribute.
        if items.iter().any(is_serde_json_macro) || items.iter().any(is_stated_reason) {
            return;
        }

        let Some(lint) = items
            .iter()
            .filter(|item| item.is_word())
            .filter_map(|item| item.name())
            .find(|name| {
                self.lints
                    .iter()
                    .any(|lint| lint.name.eq_ignore_ascii_case(name.as_str()))
            })
        else {
            return;
        };

        span_lint_and_then(
            cx,
            POLICY_LINT_SUPPRESSION_WITHOUT_REASON,
            attr.span,
            format!("`{lint}` suppressed by `{level}` without a stated reason"),
            |diag| {
                diag.help("state the technical reason for this exception in a nonempty `reason = \"...\"`");
            },
        );
    }
}

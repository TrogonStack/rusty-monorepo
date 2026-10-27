use clippy_utils::diagnostics::span_lint_and_then;
use rustc_ast::{Item, ItemKind, UseTree, UseTreeKind};
use rustc_lint::{EarlyContext, EarlyLintPass};
use rustc_span::Span;

use crate::MULTI_MODULE_USE;

/// HIR flattens a `use` group into one item per leaf, losing the brace
/// structure, so the shape of the tree is only visible to an early (AST) pass.
pub(crate) struct MultiModuleUse;

impl EarlyLintPass for MultiModuleUse {
    fn check_item(&mut self, cx: &EarlyContext<'_>, item: &Item) {
        if item.span.from_expansion() {
            return;
        }

        let ItemKind::Use(tree) = &item.kind else {
            return;
        };

        let mut offenders = Vec::new();
        collect_offenders(tree, &mut offenders);
        if offenders.is_empty() {
            return;
        }

        span_lint_and_then(
            cx,
            MULTI_MODULE_USE,
            item.span,
            "`use` statement imports items from more than one module",
            |diag| {
                for span in offenders {
                    diag.span_label(span, "reaches into another module");
                }
                diag.help("split it into one `use` per module, with no `::` inside the braces");
            },
        );
    }
}

fn collect_offenders(tree: &UseTree, offenders: &mut Vec<Span>) {
    let UseTreeKind::Nested { items, .. } = &tree.kind else {
        return;
    };

    for (child, _) in items {
        if crosses_module(child) {
            offenders.push(child.span());
        } else {
            collect_offenders(child, offenders);
        }
    }
}

/// An entry inside braces stays in the group's module only when it names a
/// single item: `Bar`, `Bar as Baz`, or `self`. A longer path, or a nested
/// group or glob behind a path, names items of a submodule.
fn crosses_module(tree: &UseTree) -> bool {
    match tree.kind {
        UseTreeKind::Simple(_) => tree.prefix.segments.len() > 1,
        UseTreeKind::Nested { .. } | UseTreeKind::Glob(_) => !tree.prefix.segments.is_empty(),
    }
}

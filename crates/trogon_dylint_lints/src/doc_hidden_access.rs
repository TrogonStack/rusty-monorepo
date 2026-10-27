use std::collections::HashSet;

use clippy_utils::diagnostics::span_lint_hir_and_then;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::{Expr, ExprKind, HirId, Path, QPath};
use rustc_lint::LateContext;
use rustc_span::{Ident, Span};

use crate::DOC_HIDDEN_ACCESS;

#[derive(Default, serde::Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    owned_crates: Vec<String>,
}

pub(crate) struct DocHiddenAccess {
    config: Config,
    reported: HashSet<Span>,
}

impl Default for DocHiddenAccess {
    fn default() -> Self {
        Self {
            config: dylint_linting::config_or_default("doc_hidden_access"),
            reported: HashSet::new(),
        }
    }
}

impl DocHiddenAccess {
    pub(crate) fn check_path(&mut self, cx: &LateContext<'_>, path: &Path<'_>, hir_id: HirId) {
        let Some(last) = path.segments.last() else {
            return;
        };

        if !is_written(cx, last.ident) {
            return;
        }

        let hidden = path
            .segments
            .iter()
            .map(|segment| segment.res)
            .chain([path.res])
            .find_map(|res| match res {
                Res::Def(_, def_id) if self.is_hidden(cx, def_id) => Some(def_id),
                _ => None,
            })
            .or_else(|| self.hidden_module_of_use(cx, path));

        if let Some(hidden) = hidden {
            self.report(cx, hidden, path.span, hir_id);
        }
    }

    pub(crate) fn check_expr(&mut self, cx: &LateContext<'_>, expr: &Expr<'_>) {
        if expr.span.from_expansion() {
            return;
        }

        let ident = match expr.kind {
            ExprKind::MethodCall(segment, ..) | ExprKind::Path(QPath::TypeRelative(_, segment)) => segment.ident,
            _ => return,
        };

        if !is_written(cx, ident) {
            return;
        }

        let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) else {
            return;
        };

        let hidden = [Some(def_id), cx.tcx.opt_parent(def_id)]
            .into_iter()
            .flatten()
            .find(|&candidate| self.is_hidden(cx, candidate));

        if let Some(hidden) = hidden {
            self.report(cx, hidden, ident.span, expr.hir_id);
        }
    }

    fn hidden_module_of_use(&self, cx: &LateContext<'_>, path: &Path<'_>) -> Option<DefId> {
        let (first, rest) = path.segments.split_first()?;
        if !matches!(first.res, Res::Err) {
            return None;
        }

        let krate = cx
            .tcx
            .crates(())
            .iter()
            .copied()
            .find(|&krate| cx.tcx.crate_name(krate) == first.ident.name)?;
        let mut current = krate.as_def_id();
        if self.is_hidden(cx, current) {
            return Some(current);
        }

        for segment in rest.split_last()?.1 {
            let module = cx
                .tcx
                .module_children(current)
                .iter()
                .find_map(|child| match child.res {
                    Res::Def(DefKind::Mod | DefKind::Enum | DefKind::Trait, def_id)
                        if child.ident.name == segment.ident.name =>
                    {
                        Some(def_id)
                    }
                    _ => None,
                })?;

            if self.is_hidden(cx, module) {
                return Some(module);
            }

            current = module;
        }

        None
    }

    fn is_hidden(&self, cx: &LateContext<'_>, def_id: DefId) -> bool {
        !def_id.is_local() && cx.tcx.is_doc_hidden(def_id) && !self.is_owned(cx, def_id)
    }

    fn report(&mut self, cx: &LateContext<'_>, hidden: DefId, span: Span, hir_id: HirId) {
        if span.from_expansion() || !self.reported.insert(span) {
            return;
        }

        let message = format!("`{}` is `#[doc(hidden)]`", cx.tcx.def_path_str(hidden));

        span_lint_hir_and_then(cx, DOC_HIDDEN_ACCESS, hir_id, span, message, |diag| {
            diag.help(
                "hidden items are the internals of their crate, exposed only for its own macros, and carry no \
                 stability promise; use the public API instead",
            );
            diag.note(
                "if the crate is owned by this codebase, list it in `owned_crates` under `[doc_hidden_access]` in \
                 `dylint.toml`",
            );
        });
    }

    fn is_owned(&self, cx: &LateContext<'_>, def_id: DefId) -> bool {
        let name = cx.tcx.crate_name(def_id.krate);
        self.config
            .owned_crates
            .iter()
            .any(|owned| owned.as_str() == name.as_str())
    }
}

/// Proc-macro output, such as a serde derive, can carry the user's own spans
/// without an expansion context, so a path whose source text is not the
/// identifier it resolves through was not written by the user.
fn is_written(cx: &LateContext<'_>, ident: Ident) -> bool {
    cx.tcx
        .sess
        .source_map()
        .span_to_snippet(ident.span)
        .is_ok_and(|snippet| snippet.trim_start_matches("r#") == ident.name.as_str())
}

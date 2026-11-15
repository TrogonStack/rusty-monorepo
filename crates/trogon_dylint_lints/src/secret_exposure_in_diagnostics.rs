use std::collections::HashSet;

use clippy_utils::diagnostics::span_lint_and_then;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind, Node};
use rustc_lint::LateContext;
use rustc_span::{ExpnKind, Span};

use crate::SECRET_EXPOSURE_IN_DIAGNOSTICS;

#[derive(Default)]
pub(crate) struct SecretExposureInDiagnostics {
    reported: HashSet<Span>,
}

impl SecretExposureInDiagnostics {
    pub(crate) fn check_expr<'tcx>(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() || !is_secret_exposure(cx, expr) || !is_diagnostic_argument(cx, expr) {
            return;
        }
        if !self.reported.insert(expr.span) {
            return;
        }

        span_lint_and_then(
            cx,
            SECRET_EXPOSURE_IN_DIAGNOSTICS,
            expr.span,
            "an exposed secret is passed directly to a diagnostic",
            |diag| {
                diag.help("record a redacted value or safe metadata instead of exposing the secret");
            },
        );
    }
}

fn is_secret_exposure(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let def_id = match expr.kind {
        ExprKind::MethodCall(..) => cx.typeck_results().type_dependent_def_id(expr.hir_id),
        ExprKind::Call(function, _) => match function.kind {
            ExprKind::Path(ref path) => match cx.qpath_res(path, function.hir_id) {
                Res::Def(DefKind::AssocFn, def_id) => Some(def_id),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    };
    let Some(def_id) = def_id else {
        return false;
    };
    if cx.tcx.item_name(def_id).as_str() != "expose_secret" {
        return false;
    }
    let trait_item = cx
        .tcx
        .opt_associated_item(def_id)
        .and_then(|item| item.trait_item_def_id())
        .unwrap_or(def_id);
    let parent = cx.tcx.parent(trait_item);
    cx.tcx.crate_name(parent.krate).as_str() == "secrecy"
        && cx.tcx.def_kind(parent) == DefKind::Trait
        && cx.tcx.item_name(parent).as_str() == "ExposeSecret"
}

fn is_diagnostic_argument(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    for (_, node) in cx.tcx.hir_parent_iter(expr.hir_id) {
        if let Node::Expr(parent) = node {
            if diagnostic_macro(cx, parent.span) {
                return true;
            }
            if !parent.span.from_expansion() {
                return false;
            }
        }
    }
    false
}

fn diagnostic_macro(cx: &LateContext<'_>, mut span: Span) -> bool {
    while span.from_expansion() {
        let expansion = span.ctxt().outer_expn_data();
        if let ExpnKind::Macro(..) = expansion.kind
            && let Some(def_id) = expansion.macro_def_id
        {
            let name = cx.tcx.item_name(def_id);
            let name = name.as_str();
            let krate = cx.tcx.crate_name(def_id.krate);
            if match krate.as_str() {
                "tracing" | "log" => matches!(name, "event" | "trace" | "debug" | "info" | "warn" | "error" | "log"),
                "std" | "core" => matches!(
                    name,
                    "print" | "println" | "eprint" | "eprintln" | "dbg" | "panic" | "panic_2015" | "panic_2021"
                ),
                _ => false,
            } {
                return true;
            }
        }
        span = expansion.call_site;
    }
    false
}

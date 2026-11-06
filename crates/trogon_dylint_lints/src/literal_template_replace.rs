use clippy_utils::diagnostics::span_lint_and_then;
use rustc_ast::LitKind;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;
use rustc_middle::ty;

use crate::LITERAL_TEMPLATE_REPLACE;

pub(crate) fn check_expr<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
    let ExprKind::MethodCall(segment, receiver, _, _) = expr.kind else {
        return;
    };
    if expr.span.from_expansion() || receiver.span.from_expansion() {
        return;
    }

    let name = segment.ident.name.as_str();
    if !matches!(name, "replace" | "replacen") || !is_inherent_str_method(cx, expr) {
        return;
    }

    if !is_literal_template(cx, receiver) {
        return;
    }

    span_lint_and_then(
        cx,
        LITERAL_TEMPLATE_REPLACE,
        expr.span,
        format!("a literal template is filled in with `str::{name}`"),
        |diag| {
            diag.span_label(receiver.span, "used as a template");
            diag.help(
                "use `format!` (or `write!` to write into a buffer) with a standard format string instead of \
                 inventing a templating syntax",
            );
        },
    );
}

fn is_inherent_str_method<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> bool {
    let Some(method) = cx.typeck_results().type_dependent_def_id(expr.hir_id) else {
        return false;
    };
    let Some(implementation) = cx.tcx.impl_of_assoc(method) else {
        return false;
    };
    cx.tcx.crate_name(method.krate) == rustc_span::sym::alloc
        && matches!(cx.tcx.type_of(implementation).instantiate_identity().kind(), ty::Str)
}

fn is_literal_template<'tcx>(cx: &LateContext<'tcx>, receiver: &'tcx Expr<'tcx>) -> bool {
    match receiver.kind {
        ExprKind::Lit(lit) => matches!(lit.node, LitKind::Str(..)),
        ExprKind::Path(ref qpath) => {
            matches!(
                cx.qpath_res(qpath, receiver.hir_id),
                Res::Def(
                    DefKind::Const { .. } | DefKind::AssocConst { .. } | DefKind::Static { .. },
                    _
                )
            ) && matches!(cx.typeck_results().expr_ty(receiver).kind(), ty::Ref(_, inner, _) if inner.is_str())
        }
        _ => false,
    }
}

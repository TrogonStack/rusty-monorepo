use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::res::MaybeDef;
use clippy_utils::sym;
use clippy_utils::visitors::is_local_used;
use rustc_hir::{BindingMode, Expr, ExprKind, PatKind};
use rustc_lint::LateContext;
use rustc_middle::ty::{self, Ty};
use rustc_span::Span;

use crate::DISCARDED_ERROR_SOURCE;
use crate::test_context::is_test_context;

pub(crate) fn check_expr<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
    let ExprKind::MethodCall(segment, receiver, args, _) = expr.kind else {
        return;
    };
    if expr.span.from_expansion() {
        return;
    }

    let span = match (segment.ident.name.as_str(), args) {
        ("map_err", [mapper]) => discarded_parameter(cx, receiver, mapper),
        ("ok_or" | "ok_or_else", [_]) => discarded_by_ok(cx, receiver),
        _ => None,
    };
    let Some(span) = span else {
        return;
    };

    if span.from_expansion() || is_test_context(cx, expr.hir_id, span) {
        return;
    }

    span_lint_and_then(
        cx,
        DISCARDED_ERROR_SOURCE,
        span,
        "the original error is discarded instead of being carried as a source",
        |diag| {
            diag.help(
                "store the error in the new one, for example `Variant(#[source] E)` with `.map_err(Variant)`, so \
                 the cause stays reachable through `Error::source`",
            );
            diag.note(
                "if dropping the error is deliberate, opt out at the site with `#[cfg_attr(dylint_lib = \
                 \"trogon_dylint_lints\", expect(discarded_error_source, reason = \"...\"))]`",
            );
        },
    );
}

/// The span of the closure parameter that `map_err` receives and never reads.
fn discarded_parameter<'tcx>(
    cx: &LateContext<'tcx>,
    receiver: &'tcx Expr<'tcx>,
    mapper: &'tcx Expr<'tcx>,
) -> Option<Span> {
    if !has_error_to_lose(cx, receiver) {
        return None;
    }

    let ExprKind::Closure(closure) = mapper.kind else {
        return None;
    };
    let body = cx.tcx.hir_body(closure.body);
    let [param] = body.params else {
        return None;
    };

    match param.pat.kind {
        PatKind::Wild => Some(param.pat.span),
        PatKind::Binding(BindingMode::NONE, hir_id, _, None) if !is_local_used(cx, body.value, hir_id) => {
            Some(param.pat.span)
        }
        _ => None,
    }
}

/// The span of the `.ok()` call that feeds `ok_or` / `ok_or_else`.
fn discarded_by_ok<'tcx>(cx: &LateContext<'tcx>, option: &'tcx Expr<'tcx>) -> Option<Span> {
    let ExprKind::MethodCall(segment, result, [], _) = option.kind else {
        return None;
    };
    if segment.ident.name != sym::ok || !has_error_to_lose(cx, result) {
        return None;
    }

    Some(option.span.with_lo(result.span.hi()))
}

/// Whether `receiver` is a `Result` whose error type holds anything worth
/// keeping as a source.
fn has_error_to_lose<'tcx>(cx: &LateContext<'tcx>, receiver: &'tcx Expr<'tcx>) -> bool {
    let ty = cx.typeck_results().expr_ty(receiver);
    if !ty.is_diag_item(cx, sym::Result) {
        return false;
    }
    let ty::Adt(_, args) = ty.kind() else {
        return false;
    };

    let error = args.type_at(1);
    !(error.is_unit() || error.is_never() || is_infallible(cx, error))
}

fn is_infallible<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    ty.ty_adt_def().is_some_and(|adt| {
        cx.tcx.item_name(adt.did()).as_str() == "Infallible" && cx.tcx.crate_name(adt.did().krate).as_str() == "core"
    })
}

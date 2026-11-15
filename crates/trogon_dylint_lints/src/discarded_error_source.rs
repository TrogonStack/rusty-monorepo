use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::res::MaybeDef;
use clippy_utils::sym;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::is_local_used;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{BindingMode, Expr, ExprKind, HirId, PatKind, StructTailExpr};
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

    let discarded = match (segment.ident.name.as_str(), args) {
        ("map_err", [mapper]) => discarded_parameter(cx, receiver, mapper),
        ("ok_or" | "ok_or_else", [_]) => discarded_by_ok(cx, receiver),
        _ => None,
    };
    let Some((span, error)) = discarded else {
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
            if is_error(cx, error) {
                diag.help(
                    "store the error in the new one, for example `Variant(#[source] E)` with `.map_err(Variant)`, \
                     so the cause stays reachable through `Error::source`",
                );
            } else {
                diag.help(format!(
                    "`{error}` is not an `Error`, so it cannot be a `#[source]`; keep it in the new error, for \
                     example `Variant(E)` with `.map_err(Variant)`, or wrap it in a type that implements `Error`"
                ));
            }
            diag.note(
                "if dropping the error is deliberate, opt out at the site with `#[cfg_attr(dylint_lib = \
                 \"trogon_dylint_lints\", expect(discarded_error_source, reason = \"...\"))]`",
            );
        },
    );
}

fn discarded_parameter<'tcx>(
    cx: &LateContext<'tcx>,
    receiver: &'tcx Expr<'tcx>,
    mapper: &'tcx Expr<'tcx>,
) -> Option<(Span, Ty<'tcx>)> {
    let error = error_to_lose(cx, receiver)?;

    let ExprKind::Closure(closure) = mapper.kind else {
        return None;
    };
    let body = cx.tcx.hir_body(closure.body);
    let [param] = body.params else {
        return None;
    };

    match param.pat.kind {
        PatKind::Wild => Some((param.pat.span, error)),
        PatKind::Binding(BindingMode::NONE, hir_id, _, None) if !is_local_used(cx, body.value, hir_id) => {
            Some((param.pat.span, error))
        }
        PatKind::Binding(BindingMode::NONE, hir_id, _, None) if is_error(cx, error) => {
            stringified_parameter(cx, body.value, hir_id).map(|span| (span, error))
        }
        _ => None,
    }
}

fn stringified_parameter<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>, parameter: HirId) -> Option<Span> {
    if expr.span.from_expansion() {
        return None;
    }

    if let Some(receiver) = to_string_receiver(cx, expr)
        && is_parameter(cx, receiver, parameter)
    {
        return Some(expr.span);
    }

    match expr.kind {
        ExprKind::Call(callee, arguments) => {
            let ExprKind::Path(qpath) = callee.kind else {
                return None;
            };
            if !matches!(cx.qpath_res(&qpath, callee.hir_id), Res::Def(DefKind::Ctor(..), _)) {
                return None;
            }
            stringified_arguments(cx, arguments.iter(), parameter)
        }
        ExprKind::Struct(_, fields, StructTailExpr::None) => {
            stringified_arguments(cx, fields.iter().map(|field| field.expr), parameter)
        }
        ExprKind::Block(block, _) if block.stmts.is_empty() => {
            block.expr.and_then(|tail| stringified_parameter(cx, tail, parameter))
        }
        _ => None,
    }
}

fn stringified_arguments<'tcx>(
    cx: &LateContext<'tcx>,
    arguments: impl Iterator<Item = &'tcx Expr<'tcx>>,
    parameter: HirId,
) -> Option<Span> {
    let mut stringification = None;
    for argument in arguments {
        if is_local_used(cx, argument, parameter) {
            // A separate use may retain the typed cause beside its display text.
            stringification = Some(stringified_parameter(cx, argument, parameter)?);
        }
    }
    stringification
}

fn to_string_receiver<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> Option<&'tcx Expr<'tcx>> {
    let (method, receiver) = match expr.kind {
        ExprKind::MethodCall(_, receiver, [], _) => (cx.typeck_results().type_dependent_def_id(expr.hir_id)?, receiver),
        ExprKind::Call(callee, [receiver]) => {
            let ExprKind::Path(qpath) = callee.kind else {
                return None;
            };
            let Res::Def(_, method) = cx.qpath_res(&qpath, callee.hir_id) else {
                return None;
            };
            (method, receiver)
        }
        _ => return None,
    };

    cx.tcx
        .is_diagnostic_item(sym::to_string_method, method)
        .then_some(receiver)
}

fn is_parameter(cx: &LateContext<'_>, expr: &Expr<'_>, parameter: HirId) -> bool {
    match expr.kind {
        ExprKind::Path(qpath) => matches!(cx.qpath_res(&qpath, expr.hir_id), Res::Local(local) if local == parameter),
        ExprKind::AddrOf(_, _, inner) | ExprKind::DropTemps(inner) | ExprKind::Use(inner, _) => {
            is_parameter(cx, inner, parameter)
        }
        _ => false,
    }
}

/// The span of the `.ok()` call that feeds `ok_or` / `ok_or_else`, with the
/// error type it drops.
fn discarded_by_ok<'tcx>(cx: &LateContext<'tcx>, option: &'tcx Expr<'tcx>) -> Option<(Span, Ty<'tcx>)> {
    let ExprKind::MethodCall(segment, result, [], _) = option.kind else {
        return None;
    };
    if segment.ident.name != sym::ok {
        return None;
    }
    let error = error_to_lose(cx, result)?;

    Some((option.span.with_lo(result.span.hi()), error))
}

/// The error type of `receiver`, once autoderef reaches the `Result` the
/// method runs on, when it holds anything worth keeping as a source.
fn error_to_lose<'tcx>(cx: &LateContext<'tcx>, receiver: &'tcx Expr<'tcx>) -> Option<Ty<'tcx>> {
    let ty = cx.typeck_results().expr_ty_adjusted(receiver);
    if !ty.is_diag_item(cx, sym::Result) {
        return None;
    }
    let ty::Adt(_, args) = ty.kind() else {
        return None;
    };

    let error = args.type_at(1);
    (!(error.is_unit() || error.is_never() || is_infallible(cx, error))).then_some(error)
}

fn is_error<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    cx.tcx
        .get_diagnostic_item(sym::Error)
        .is_some_and(|error| implements_trait(cx, ty, error, &[]))
}

fn is_infallible<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    ty.ty_adt_def().is_some_and(|adt| {
        cx.tcx.item_name(adt.did()).as_str() == "Infallible" && cx.tcx.crate_name(adt.did().krate).as_str() == "core"
    })
}

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::sym;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind, Stmt, StmtKind};
use rustc_lint::LateContext;
use rustc_middle::ty::Ty;

use crate::DISCARDED_TASK_HANDLE;

pub(crate) fn check_stmt<'tcx>(cx: &LateContext<'tcx>, stmt: &'tcx Stmt<'tcx>) {
    let StmtKind::Semi(expr) = stmt.kind else {
        return;
    };
    if !expr.span.from_expansion() && is_task_handle(cx, cx.typeck_results().expr_ty(expr)) {
        report(cx, expr);
    }
}

pub(crate) fn check_expr<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
    if expr.span.from_expansion() {
        return;
    }
    let ExprKind::Call(function, [argument]) = expr.kind else {
        return;
    };
    let ExprKind::Path(path) = &function.kind else {
        return;
    };
    let Res::Def(DefKind::Fn, def_id) = cx.qpath_res(path, function.hir_id) else {
        return;
    };
    if cx.tcx.is_diagnostic_item(sym::mem_drop, def_id) && is_task_handle(cx, cx.typeck_results().expr_ty(argument)) {
        report(cx, expr);
    }
}

fn is_task_handle(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    ty.ty_adt_def().is_some_and(|adt| {
        cx.tcx.crate_name(adt.did().krate).as_str() == "tokio"
            && matches!(
                cx.tcx.def_path_str(adt.did()).as_str(),
                "tokio::runtime::task::join::JoinHandle"
                    | "tokio::runtime::task::JoinHandle"
                    | "tokio::task::JoinHandle"
            )
    })
}

fn report(cx: &LateContext<'_>, expr: &Expr<'_>) {
    span_lint_and_then(
        cx,
        DISCARDED_TASK_HANDLE,
        expr.span,
        "task handle is discarded without an explicit detachment policy",
        |diag| {
            diag.help(
                "await the handle or transfer it to a task owner; if detachment is deliberate, explain it with \
                 `expect(discarded_task_handle, reason = \"...\")`",
            );
        },
    );
}

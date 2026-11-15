use clippy_utils::diagnostics::span_lint_and_then;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{ClosureKind, CoroutineDesugaring, CoroutineKind, Expr, ExprKind, Node};
use rustc_lint::LateContext;

use crate::BLOCKING_CALL_IN_ASYNC;

pub(crate) fn check_expr<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
    if expr.span.from_expansion() {
        return;
    }

    let ExprKind::Call(function, _) = expr.kind else {
        return;
    };
    let ExprKind::Path(path) = &function.kind else {
        return;
    };
    let Res::Def(DefKind::Fn, def_id) = cx.qpath_res(path, function.hir_id) else {
        return;
    };
    if cx.tcx.crate_name(def_id.krate).as_str() != "std"
        || !matches!(
            cx.tcx.def_path_str(def_id).as_str(),
            "std::thread::sleep"
                | "std::thread::park"
                | "std::thread::park_timeout"
                | "std::thread::functions::sleep"
                | "std::thread::functions::park"
                | "std::thread::functions::park_timeout"
                | "std::fs::read"
                | "std::fs::read_to_string"
                | "std::fs::write"
        )
        || !is_async_execution(cx, expr)
    {
        return;
    }

    span_lint_and_then(
        cx,
        BLOCKING_CALL_IN_ASYNC,
        function.span,
        "blocking call runs directly in an async body",
        |diag| {
            diag.help("use an async operation or move the blocking work into a dedicated blocking worker");
        },
    );
}

fn is_async_execution(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    for (_, node) in cx.tcx.hir_parent_iter(expr.hir_id) {
        match node {
            Node::Expr(closure_expr) => {
                let ExprKind::Closure(closure) = closure_expr.kind else {
                    continue;
                };
                if matches!(
                    closure.kind,
                    ClosureKind::Coroutine(CoroutineKind::Desugared(CoroutineDesugaring::Async, _))
                ) {
                    return true;
                }
                if !is_immediately_called(cx, closure_expr) {
                    return false;
                }
            }
            Node::Item(_) | Node::ImplItem(_) | Node::TraitItem(_) => return false,
            _ => {}
        }
    }
    false
}

fn is_immediately_called(cx: &LateContext<'_>, closure: &Expr<'_>) -> bool {
    matches!(
        cx.tcx.hir_parent_iter(closure.hir_id).next(),
        Some((_, Node::Expr(Expr { kind: ExprKind::Call(callee, _), .. })))
            if callee.hir_id == closure.hir_id
    )
}

use clippy_utils::diagnostics::span_lint_and_then;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;

use crate::PROCESS_GLOBAL_MUTATION_IN_TESTS;
use crate::test_context::is_test_context;

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
            "std::env::set_var" | "std::env::remove_var" | "std::env::set_current_dir"
        )
        || !is_test_context(cx, expr.hir_id, expr.span)
    {
        return;
    }

    span_lint_and_then(
        cx,
        PROCESS_GLOBAL_MUTATION_IN_TESTS,
        function.span,
        "process-global state is mutated in test code",
        |diag| {
            diag.help("inject environment or working-directory state, or run the mutation in an isolated subprocess");
            diag.note("a test-local lock does not protect other code in the process that reads this state");
        },
    );
}

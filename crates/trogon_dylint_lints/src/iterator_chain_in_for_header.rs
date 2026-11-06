use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::higher::ForLoop;
use clippy_utils::sym;
use clippy_utils::ty::implements_trait;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::LateContext;

use crate::ITERATOR_CHAIN_IN_FOR_HEADER;

pub(crate) fn check_expr<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
    let Some(for_loop) = ForLoop::hir(expr) else {
        return;
    };
    let iterated = for_loop.arg;
    if for_loop.span.from_expansion() || iterated.span.from_expansion() || !chain_takes_closure(cx, iterated) {
        return;
    }

    span_lint_and_then(
        cx,
        ITERATOR_CHAIN_IN_FOR_HEADER,
        iterated.span,
        "an iterator chain with a closure sits in the header of a `for` loop",
        |diag| {
            diag.help("assign the chain to a variable whose name says what it yields, then loop over that variable");
        },
    );
}

/// Whether some call in the receiver chain hands a closure to an iterator
/// adapter, since the closure is the logic that deserves a name. Adapters that
/// take no closure, such as `enumerate`, `rev`, `zip` and `lines`, read plainly
/// in the header, and a closure given to a call that does not yield an
/// iterator, such as `Option::map`, is not a combinator chain.
fn chain_takes_closure<'tcx>(cx: &LateContext<'tcx>, mut expr: &'tcx Expr<'tcx>) -> bool {
    while let ExprKind::MethodCall(_, receiver, args, _) = expr.kind {
        let takes_closure = args
            .iter()
            .any(|arg| matches!(arg.kind, ExprKind::Closure(..)) && !arg.span.from_expansion());
        if takes_closure && yields_iterator(cx, expr) {
            return true;
        }
        expr = receiver;
    }
    false
}

fn yields_iterator<'tcx>(cx: &LateContext<'tcx>, call: &'tcx Expr<'tcx>) -> bool {
    cx.tcx
        .get_diagnostic_item(sym::Iterator)
        .is_some_and(|iterator| implements_trait(cx, cx.typeck_results().expr_ty(call), iterator, &[]))
}

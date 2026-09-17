use std::path::Path;

use clippy_utils::diagnostics::span_lint_and_then;
use rustc_hir::def::DefKind;
use rustc_hir::def_id::LocalDefId;
use rustc_hir::{Item, ItemKind};
use rustc_lint::LateContext;
use rustc_span::{FileName, SourceFile};

use crate::CONSTANT_OUTSIDE_CONSTANTS_MODULE;
use crate::test_context::{is_test_module_name, is_test_or_bench_source};

pub(crate) fn check_item<'tcx>(cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
    if item.span.from_expansion() {
        return;
    }

    let ItemKind::Const(ident, ..) = item.kind else {
        return;
    };

    // Only module-level (and crate-root) constants are policed. A `const`
    // declared inside a function body is a local implementation detail whose
    // parent is the enclosing item, not a module, so it is left alone.
    // Associated consts live in `impl`/`trait` blocks and are `ImplItem`s /
    // `TraitItem`s, so they never reach `check_item` at all.
    if !matches!(
        cx.tcx.def_kind(cx.tcx.local_parent(item.owner_id.def_id)),
        DefKind::Mod
    ) {
        return;
    }

    let file = cx.tcx.sess.source_map().lookup_char_pos(item.span.lo()).file;

    // Generated files (proto codegen, etc.) emit module-level constants whose
    // placement is dictated by codegen and cannot be hand-edited. Skip any file
    // carrying the conventional `@generated` marker near its top.
    if is_generated(&file) {
        return;
    }

    let FileName::Real(real) = &file.name else {
        return;
    };
    let Some(path) = real.local_path() else {
        return;
    };

    // `constants.rs` is the one place these belong, so it is never flagged.
    // Test and benchmark sources carry fixtures and per-case tuning values that
    // are not crate configuration, so they are exempt too, whether that shows up
    // as the file path or an enclosing inline `tests`/`benches` module.
    if is_constants_file(path)
        || is_test_or_bench_source(cx, item.span)
        || is_inside_test_or_bench_module(cx, item.owner_id.def_id)
    {
        return;
    }

    span_lint_and_then(
        cx,
        CONSTANT_OUTSIDE_CONSTANTS_MODULE,
        item.span.with_hi(ident.span.hi()),
        format!("constant `{ident}` declared outside the `constants` module"),
        |diag| {
            diag.help(format!(
                "move `{ident}` into a `constants` module (`constants.rs`) and refer to it as `constants::{ident}`"
            ));
        },
    );
}

fn is_constants_file(path: &Path) -> bool {
    file_stem(path) == Some("constants")
}

fn file_stem(path: &Path) -> Option<&str> {
    path.file_stem().and_then(|stem| stem.to_str())
}

/// A `const` in an inline `tests`/`*_tests` (or `benches`/`*_benches`) module
/// sits in a production file, so the path checks miss it. Walk the module
/// ancestors and exempt the constant when any enclosing module is a test or
/// benchmark module.
fn is_inside_test_or_bench_module(cx: &LateContext<'_>, def_id: LocalDefId) -> bool {
    let mut current = def_id.to_def_id();
    while let Some(parent) = cx.tcx.opt_parent(current) {
        if cx.tcx.def_kind(parent) == DefKind::Mod
            && cx
                .tcx
                .opt_item_name(parent)
                .is_some_and(|name| is_test_module_name(name.as_str()))
        {
            return true;
        }
        current = parent;
    }
    false
}

fn is_generated(file: &SourceFile) -> bool {
    let Some(src) = &file.src else {
        return false;
    };
    // Recognize both generator banners used in this workspace: buffa-codegen
    // (proto) stamps `@generated`, Weaver (semconv) stamps `DO NOT EDIT`.
    src.lines()
        .take(5)
        .any(|line| line.contains("@generated") || line.contains("DO NOT EDIT"))
}

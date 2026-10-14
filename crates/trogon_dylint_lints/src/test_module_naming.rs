use std::path::{Path, PathBuf};

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::is_test_function;
use rustc_hir::{Item, ItemKind, Mod, Node};
use rustc_lint::LateContext;
use rustc_span::{FileName, Span};

use crate::TEST_MODULE_NAMING;
use crate::test_context::is_integration_or_bench_target;

pub(crate) fn check_crate(cx: &LateContext<'_>) {
    if cx.tcx.sess.opts.test || is_integration_or_bench_target() {
        return;
    }
    let module = cx.tcx.hir_root_module();
    if is_generated_source(cx, module.spans.inner_span) {
        return;
    }
    if source_path(cx, module.spans.inner_span).is_some_and(|path| is_test_source(&path)) {
        span_lint_and_then(
            cx,
            TEST_MODULE_NAMING,
            module.spans.inner_span.shrink_to_lo(),
            "unit-test source is compiled in a non-test target",
            |diag| {
                diag.help(
                    "keep unit-test sources behind `#[cfg(test)]`; use a production filename for production code",
                );
            },
        );
    }
}

pub(crate) fn check_item<'tcx>(cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
    if item.span.from_expansion() || is_integration_or_bench_target() || is_generated_source(cx, item.span) {
        return;
    }

    let ItemKind::Mod(ident, module) = item.kind else {
        check_test_function(cx, item);
        return;
    };
    if is_generated_source(cx, module.spans.inner_span) {
        return;
    }

    let name = ident.as_str();
    let reserved_name = is_unit_test_module_name(name);
    let body_path = source_path(cx, module.spans.inner_span);
    if !cx.tcx.sess.opts.test {
        if reserved_name || body_path.as_deref().is_some_and(is_test_source) {
            span_lint_and_then(
                cx,
                TEST_MODULE_NAMING,
                item.span.with_hi(ident.span.hi()),
                format!("unit-test module `{ident}` is compiled in a non-test target"),
                |diag| {
                    diag.help(
                        "gate the module with `#[cfg(test)]`; use a production module and filename for production code",
                    );
                },
            );
        }
        return;
    }

    if !reserved_name && !module_holds_tests(cx, module) {
        return;
    }
    if !reserved_name {
        span_lint_and_then(
            cx,
            TEST_MODULE_NAMING,
            item.span.with_hi(ident.span.hi()),
            format!("test module `{ident}` is not named `tests` or `*_tests`"),
            |diag| {
                diag.help(format!(
                    "extract the tests into a child module named `tests` or `{ident}_tests`, using the owner's sibling test file"
                ));
            },
        );
        return;
    }

    let Some(owner) = owner_module(cx, item) else {
        span_lint_and_then(
            cx,
            TEST_MODULE_NAMING,
            item.span.with_hi(ident.span.hi()),
            format!("unit-test module `{ident}` must be declared at module scope"),
            |diag| {
                diag.help("declare the test module in its owning module rather than inside a function");
            },
        );
        return;
    };
    // Inline modules have their own diagnostic, including the correct sibling
    // path. Reporting the same layout problem twice obscures that action.
    if source_path(cx, owner.spans.inner_span) == body_path {
        return;
    }
    let Some(expected) = sibling_test_path(cx, item, name) else {
        return;
    };
    if body_path.is_some_and(|actual| same_file(&actual, &expected)) {
        return;
    }
    let Some(filename) = expected.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    span_lint_and_then(
        cx,
        TEST_MODULE_NAMING,
        item.span.with_hi(ident.span.hi()),
        format!("unit-test module `{ident}` must load sibling file `{filename}`"),
        |diag| {
            if let Some(declaration) = test_module_declaration(cx, item, name) {
                diag.help(format!("declare `{declaration}` in the owning module"));
            }
        },
    );
}

fn check_test_function(cx: &LateContext<'_>, item: &Item<'_>) {
    let ItemKind::Fn { ident, .. } = item.kind else {
        return;
    };
    if !is_test_function(cx.tcx, item.owner_id.def_id) || owner_name(cx, item).is_some() {
        return;
    }
    span_lint_and_then(
        cx,
        TEST_MODULE_NAMING,
        ident.span,
        format!("unit test `{ident}` must live in a child test module"),
        |diag| {
            diag.help("move the test into the owner's sibling test file and load it with `#[cfg(test)]`");
        },
    );
}

pub(crate) fn sibling_test_path(cx: &LateContext<'_>, item: &Item<'_>, name: &str) -> Option<PathBuf> {
    let owner = owner_module(cx, item)?;
    let path = source_path(cx, owner.spans.inner_span)?;
    let stem = path.file_stem()?.to_str()?;
    let filename = match (stem, name) {
        ("mod", _) => format!("{name}.rs"),
        (_, "tests") => format!("{stem}_tests.rs"),
        _ => format!("{stem}_{name}.rs"),
    };
    Some(path.parent()?.join(filename))
}

pub(crate) fn test_module_declaration(cx: &LateContext<'_>, item: &Item<'_>, name: &str) -> Option<String> {
    let owner = owner_module(cx, item)?;
    let owner_path = source_path(cx, owner.spans.inner_span)?;
    if owner_path.file_stem()?.to_str()? == "mod" {
        return Some(format!("#[cfg(test)] mod {name};"));
    }
    let test_path = sibling_test_path(cx, item, name)?;
    let filename = test_path.file_name()?.to_str()?;
    Some(format!("#[cfg(test)] #[path = \"{filename}\"] mod {name};"))
}

fn owner_module<'tcx>(cx: &LateContext<'tcx>, item: &Item<'_>) -> Option<&'tcx Mod<'tcx>> {
    let owner = cx.tcx.hir_get_parent_item(item.hir_id()).def_id;
    match cx.tcx.hir_node_by_def_id(owner) {
        Node::Item(Item {
            kind: ItemKind::Mod(_, module),
            ..
        }) => Some(module),
        Node::Crate(module) => Some(module),
        _ => None,
    }
}

fn owner_name<'tcx>(cx: &LateContext<'tcx>, item: &Item<'_>) -> Option<&'tcx str> {
    let owner = cx.tcx.hir_get_parent_item(item.hir_id()).def_id;
    match cx.tcx.hir_node_by_def_id(owner) {
        Node::Item(Item {
            kind: ItemKind::Mod(name, _),
            ..
        }) => Some(name.as_str()),
        _ => None,
    }
}

pub(crate) fn is_unit_test_module_name(name: &str) -> bool {
    name == "tests" || name.ends_with("_tests")
}

fn is_test_source(path: &Path) -> bool {
    path.file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(is_unit_test_module_name)
}

fn source_path(cx: &LateContext<'_>, span: Span) -> Option<PathBuf> {
    let file = cx.tcx.sess.source_map().lookup_char_pos(span.lo()).file;
    let FileName::Real(real) = &file.name else {
        return None;
    };
    real.local_path().map(Path::to_path_buf)
}

fn same_file(actual: &Path, expected: &Path) -> bool {
    actual
        .canonicalize()
        .ok()
        .zip(expected.canonicalize().ok())
        .is_some_and(|(actual, expected)| actual == expected)
}

pub(crate) fn is_generated_source(cx: &LateContext<'_>, span: Span) -> bool {
    let file = cx.tcx.sess.source_map().lookup_char_pos(span.lo()).file;
    file.src.as_ref().is_some_and(|src| {
        src.lines()
            .take(5)
            .any(|line| line.contains("@generated") || line.contains("DO NOT EDIT"))
    })
}

fn module_holds_tests<'tcx>(cx: &LateContext<'tcx>, module: &'tcx Mod<'tcx>) -> bool {
    module
        .item_ids
        .iter()
        .any(|item_id| is_test_function(cx.tcx, item_id.owner_id.def_id))
}

use std::path::{Path, PathBuf};

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::is_test_function;
use rustc_hir::{Item, ItemKind, Mod, Node};
use rustc_lint::LateContext;
use rustc_span::{FileName, Ident, Span};

use crate::TEST_MODULE_NAMING;
use crate::test_context::is_integration_or_bench_target;

const OWNING_MODULE: &str = "the owning module";

/// The file a unit-test module's body belongs in, and the declaration that
/// loads it from there. `display` is `path` relative to the owner's directory;
/// `host` names where `declaration` goes.
pub(crate) struct TestModuleHome {
    pub(crate) path: PathBuf,
    pub(crate) display: String,
    pub(crate) declaration: String,
    pub(crate) host: String,
}

enum Owner<'tcx> {
    Crate(&'tcx Mod<'tcx>),
    Module {
        item: &'tcx Item<'tcx>,
        ident: Ident,
        module: &'tcx Mod<'tcx>,
    },
}

impl<'tcx> Owner<'tcx> {
    fn of(cx: &LateContext<'tcx>, item: &Item<'_>) -> Option<Self> {
        let owner = cx.tcx.hir_get_parent_item(item.hir_id()).def_id;
        match cx.tcx.hir_node_by_def_id(owner) {
            Node::Item(item) => match item.kind {
                ItemKind::Mod(ident, module) => Some(Self::Module { item, ident, module }),
                _ => None,
            },
            Node::Crate(module) => Some(Self::Crate(module)),
            _ => None,
        }
    }

    fn module(&self) -> &'tcx Mod<'tcx> {
        match self {
            Self::Crate(module) | Self::Module { module, .. } => module,
        }
    }

    fn is_test_module(&self) -> bool {
        matches!(self, Self::Module { ident, .. } if is_unit_test_module_name(ident.as_str()))
    }

    fn is_mislaid_test_module(&self, cx: &LateContext<'_>) -> bool {
        let Self::Module { item, ident, module } = *self else {
            return false;
        };
        is_unit_test_module_name(ident.as_str())
            && !test_module_home(cx, item, ident.as_str(), module).is_some_and(|home| {
                source_path(cx, module.spans.inner_span).is_some_and(|body| same_file(&body, &home.path))
            })
    }
}

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
    if is_integration_or_bench_target() || is_generated_source(cx, item.span) {
        return;
    }

    let ItemKind::Mod(ident, module) = item.kind else {
        check_test_function(cx, item);
        return;
    };
    if item.span.from_expansion() || is_generated_source(cx, module.spans.inner_span) {
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
            format!("module `{ident}` holds unit tests but is not a `tests` module"),
            |diag| {
                diag.help("move the tests into the owner's `tests` module, or into a `*_tests` child module of it");
            },
        );
        return;
    }

    let Some(owner) = Owner::of(cx, item) else {
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
    let inline = same_source_file(cx, owner.module().spans.inner_span, module.spans.inner_span);
    if name != "tests" && !owner.is_test_module() && !inline {
        span_lint_and_then(
            cx,
            TEST_MODULE_NAMING,
            item.span.with_hi(ident.span.hi()),
            format!("unit-test module `{ident}` must be a child of the owner's `tests` module"),
            |diag| {
                if let Some(TestModuleHome {
                    display,
                    declaration,
                    host,
                    ..
                }) = test_module_home(cx, item, name, module)
                {
                    diag.help(format!("move it to `{display}` and declare `{declaration}` in {host}"));
                }
            },
        );
        return;
    }
    // Inline modules have their own diagnostic, including the expected file.
    // Reporting the same layout problem twice obscures that action.
    if inline {
        return;
    }
    // A child's home follows from where its `tests` owner belongs, so a mislaid
    // owner is reported once, on itself, rather than again on every child.
    if owner.is_mislaid_test_module(cx) {
        return;
    }
    let Some(TestModuleHome {
        path,
        display,
        declaration,
        host,
    }) = test_module_home(cx, item, name, module)
    else {
        return;
    };
    if body_path.is_some_and(|actual| same_file(&actual, &path)) {
        return;
    }
    span_lint_and_then(
        cx,
        TEST_MODULE_NAMING,
        item.span.with_hi(ident.span.hi()),
        format!("unit-test module `{ident}` must load `{display}`"),
        |diag| {
            diag.help(format!("declare `{declaration}` in {host}"));
        },
    );
}

fn check_test_function(cx: &LateContext<'_>, item: &Item<'_>) {
    let ItemKind::Fn { ident, .. } = item.kind else {
        return;
    };
    // Attribute macros such as `#[tokio::test]` re-emit the whole item from
    // expansion but keep the name the author wrote, while a code generator
    // names the function itself; only the author's tests are placement errors.
    if ident.span.from_expansion() || !is_test_function(cx.tcx, item.owner_id.def_id) || owner_name(cx, item).is_some()
    {
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

/// The owner's module directory follows the standard resolution rules even
/// when the owner itself was loaded through `#[path]`. Placement compares
/// resolved files, so any `#[path]` that lands on the expected file passes.
/// A child of an inline module or of a test module is placed under where that
/// module belongs, not where it currently sits, so every diagnostic describes
/// the same end state.
pub(crate) fn test_module_home(
    cx: &LateContext<'_>,
    item: &Item<'_>,
    name: &str,
    module: &Mod<'_>,
) -> Option<TestModuleHome> {
    let owner = Owner::of(cx, item)?;
    let owner_path = owner_home(cx, &owner)?;
    let owner_dir = owner_path.parent()?;
    let stem = owner_path.file_stem()?.to_str()?;
    let module_dir = module_dir(&owner, &owner_path)?;
    let home = |path: PathBuf, declaration: String, host: String| {
        Some(TestModuleHome {
            display: relative_display(&path, owner_dir)?,
            path,
            declaration,
            host,
        })
    };

    if owner.is_test_module() {
        home(
            module_dir.join(format!("{name}.rs")),
            format!("mod {name};"),
            OWNING_MODULE.to_owned(),
        )
    } else if name != "tests" {
        let host = format!("`{}`", relative_display(&module_dir.join("tests.rs"), owner_dir)?);
        home(
            module_dir.join("tests").join(format!("{name}.rs")),
            format!("mod {name};"),
            host,
        )
    } else if stem == "mod" || has_child_module(cx, module) {
        home(
            module_dir.join("tests.rs"),
            "#[cfg(test)] mod tests;".to_owned(),
            OWNING_MODULE.to_owned(),
        )
    } else {
        let filename = format!("{stem}_tests.rs");
        let declaration = format!("#[cfg(test)] #[path = \"{filename}\"] mod tests;");
        home(owner_dir.join(filename), declaration, OWNING_MODULE.to_owned())
    }
}

/// Where the owner's body belongs: an inline module goes where
/// `inline_module_block` sends it and a test module where this lint sends it,
/// so a child's home follows the intended layout rather than the file the
/// owner is written in today.
fn owner_home(cx: &LateContext<'_>, owner: &Owner<'_>) -> Option<PathBuf> {
    let Owner::Module { item, ident, module } = *owner else {
        return source_path(cx, owner.module().spans.inner_span);
    };
    if is_unit_test_module_name(ident.as_str()) {
        return Some(test_module_home(cx, item, ident.as_str(), module)?.path);
    }
    let parent = Owner::of(cx, item)?;
    if !same_source_file(cx, parent.module().spans.inner_span, module.spans.inner_span) {
        return source_path(cx, module.spans.inner_span);
    }
    let parent_path = owner_home(cx, &parent)?;
    Some(module_dir(&parent, &parent_path)?.join(format!("{ident}.rs")))
}

/// The directory the owner's child modules resolve in, which `lib.rs`,
/// `main.rs`, and `mod.rs` share with the file itself.
fn module_dir(owner: &Owner<'_>, owner_path: &Path) -> Option<PathBuf> {
    let owner_dir = owner_path.parent()?;
    let stem = owner_path.file_stem()?;
    Some(if matches!(owner, Owner::Crate(_)) || stem == "mod" {
        owner_dir.to_path_buf()
    } else {
        owner_dir.join(stem)
    })
}

/// An inline child counts too: `inline_module_block` sends it to its own file,
/// and the suite must already be laid out for the file it will become.
fn has_child_module(cx: &LateContext<'_>, module: &Mod<'_>) -> bool {
    module.item_ids.iter().any(|&item_id| {
        let child = cx.tcx.hir_item(item_id);
        matches!(child.kind, ItemKind::Mod(..)) && !child.span.from_expansion()
    })
}

fn same_source_file(cx: &LateContext<'_>, left: Span, right: Span) -> bool {
    let source_map = cx.tcx.sess.source_map();
    source_map.lookup_char_pos(left.lo()).file.name == source_map.lookup_char_pos(right.lo()).file.name
}

fn relative_display(path: &Path, base: &Path) -> Option<String> {
    let segments = path
        .strip_prefix(base)
        .ok()?
        .iter()
        .map(|segment| segment.to_str())
        .collect::<Option<Vec<_>>>()?;
    Some(segments.join("/"))
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

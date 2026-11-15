use std::collections::HashSet;

use clippy_utils::diagnostics::span_lint_hir_and_then;
use clippy_utils::{is_cfg_test, is_in_test, is_test_function};
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::intravisit::{self, FnKind, Visitor};
use rustc_hir::{AmbigArg, Body, Expr, ExprKind, HirId, Node, Pat, PatKind, Path, QPath, Ty, TyKind, find_attr};
use rustc_lint::LateContext;
use rustc_middle::ty::{self, TyCtxt};
use rustc_session::config::CrateType;
use rustc_span::Span;

use crate::PREFER_INTEGRATION_TESTS;
use crate::test_context::is_integration_or_bench_target;
use crate::test_module_naming::is_generated_source;

pub(crate) fn check_fn<'tcx>(cx: &LateContext<'tcx>, kind: FnKind<'tcx>, body: &'tcx Body<'tcx>, def_id: LocalDefId) {
    let FnKind::ItemFn(ident, ..) = kind else {
        return;
    };
    if !cx.tcx.sess.opts.test
        || !is_test_function(cx.tcx, def_id)
        || ident.span.from_expansion()
        || is_generated_source(cx, ident.span)
        || is_integration_or_bench_target()
        || !is_library_target(cx)
    {
        return;
    }

    let mut dependencies = Dependencies {
        cx,
        visited: HashSet::from([def_id]),
        needs_private_access: false,
    };
    dependencies.visit_body(body);
    if dependencies.needs_private_access {
        return;
    }

    span_lint_hir_and_then(
        cx,
        PREFER_INTEGRATION_TESTS,
        cx.tcx.local_def_id_to_hir_id(def_id),
        ident.span,
        format!("test `{ident}` does not require private implementation access"),
        |diag| {
            diag.help(
                "move this test to the crate's `tests/` directory beside `src/`, and import the public API through the crate name",
            );
        },
    );
}

fn is_library_target(cx: &LateContext<'_>) -> bool {
    if std::env::var_os("CARGO_BIN_NAME").is_some() {
        return false;
    }
    // Cargo's library test invocation omits --crate-type even with a custom
    // [lib] path; its binary counterpart supplies CARGO_BIN_NAME instead.
    std::env::var_os("CARGO_CRATE_NAME").is_some()
        || cx
            .tcx
            .sess
            .opts
            .crate_types
            .iter()
            .any(|kind| *kind != CrateType::Executable)
        || cx
            .tcx
            .sess
            .local_crate_source_file()
            .and_then(|file| file.into_local_path())
            .is_some_and(|path| path.file_name().is_some_and(|name| name == "lib.rs"))
}

struct Dependencies<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    visited: HashSet<LocalDefId>,
    needs_private_access: bool,
}

impl<'tcx> Dependencies<'_, 'tcx> {
    fn reference(&mut self, def_id: DefId) {
        if self.needs_private_access {
            return;
        }
        let Some(local) = def_id.as_local() else {
            return;
        };
        if matches!(
            self.cx.tcx.def_kind(def_id),
            DefKind::TyParam | DefKind::ConstParam | DefKind::LifetimeParam | DefKind::Mod
        ) {
            return;
        }

        if self.is_test_only(def_id) {
            if !self.visited.insert(local) {
                return;
            }
            match self.cx.tcx.hir_node_by_def_id(local) {
                Node::Item(item) => self.visit_item(item),
                Node::ImplItem(item) => self.visit_impl_item(item),
                Node::TraitItem(item) => self.visit_trait_item(item),
                _ => {}
            }
            return;
        }

        if matches!(self.cx.tcx.def_kind(def_id), DefKind::Ctor(..))
            && find_attr!(self.cx.tcx, self.cx.tcx.parent(def_id), NonExhaustive(..))
        {
            self.needs_private_access = true;
            return;
        }

        // Trait impl items inherit the trait's public contract, not the
        // visibility rustc records on the implementation method itself.
        let contract = self
            .cx
            .tcx
            .opt_associated_item(def_id)
            .and_then(|item| item.trait_item_def_id())
            .unwrap_or(def_id);
        if let Some(local) = contract.as_local() {
            self.needs_private_access |= !self.cx.effective_visibilities.is_exported(local);
        }
    }

    fn qpath(&mut self, qpath: &QPath<'tcx>, hir_id: HirId) {
        if let Res::Def(_, def_id) = self.cx.qpath_res(qpath, hir_id) {
            self.reference(def_id);
        }
    }

    fn field(&mut self, def_id: DefId) {
        if def_id.is_local() && !self.is_test_only(def_id) {
            self.needs_private_access |= !self.cx.tcx.visibility(def_id).is_public();
        }
    }

    fn is_test_only(&self, def_id: DefId) -> bool {
        def_id.as_local().is_some_and(|local| {
            let hir_id = self.cx.tcx.local_def_id_to_hir_id(local);
            is_cfg_test(self.cx.tcx, hir_id) || is_in_test(self.cx.tcx, hir_id)
        })
    }

    fn non_exhaustive_construction(&mut self, variant: &ty::VariantDef) {
        self.needs_private_access |=
            variant.def_id.is_local() && !self.is_test_only(variant.def_id) && variant.is_field_list_non_exhaustive();
    }

    fn macros(&mut self, mut span: Span) {
        while span.from_expansion() {
            let expansion = span.ctxt().outer_expn_data();
            if let Some(def_id) = expansion.macro_def_id {
                self.reference(def_id);
            }
            span = expansion.call_site;
        }
    }
}

impl<'tcx> Visitor<'tcx> for Dependencies<'_, 'tcx> {
    type NestedFilter = rustc_middle::hir::nested_filter::OnlyBodies;

    fn maybe_tcx(&mut self) -> TyCtxt<'tcx> {
        self.cx.tcx
    }

    fn visit_path(&mut self, path: &Path<'tcx>, _: HirId) {
        if let Res::Def(_, def_id) = path.res {
            self.reference(def_id);
        }
        intravisit::walk_path(self, path);
    }

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        self.macros(expr.span);
        if self.needs_private_access {
            return;
        }
        let typeck = self.cx.tcx.typeck(expr.hir_id.owner.def_id);
        match expr.kind {
            ExprKind::Path(ref qpath) => self.qpath(qpath, expr.hir_id),
            ExprKind::MethodCall(..) => {
                if let Some(def_id) = typeck.type_dependent_def_id(expr.hir_id) {
                    self.reference(def_id);
                }
            }
            ExprKind::Field(base, _) => {
                if let ty::Adt(adt, _) = typeck.expr_ty_adjusted(base).kind()
                    && let Some(index) = typeck.opt_field_index(expr.hir_id)
                {
                    self.field(adt.non_enum_variant().fields[index].did);
                }
            }
            ExprKind::Struct(qpath, _, _) => {
                self.qpath(qpath, expr.hir_id);
                if let ty::Adt(adt, _) = typeck.expr_ty(expr).kind() {
                    let variant = adt.variant_of_res(typeck.qpath_res(qpath, expr.hir_id));
                    self.non_exhaustive_construction(variant);
                    for field in &variant.fields {
                        self.field(field.did);
                    }
                }
            }
            _ => {}
        }
        intravisit::walk_expr(self, expr);
    }

    fn visit_pat(&mut self, pat: &'tcx Pat<'tcx>) {
        self.macros(pat.span);
        let typeck = self.cx.tcx.typeck(pat.hir_id.owner.def_id);
        match pat.kind {
            PatKind::Struct(ref qpath, fields, rest) => {
                self.qpath(qpath, pat.hir_id);
                if let ty::Adt(adt, _) = typeck.pat_ty(pat).peel_refs().kind() {
                    let variant = adt.variant_of_res(typeck.qpath_res(qpath, pat.hir_id));
                    if rest.is_none() {
                        self.non_exhaustive_construction(variant);
                    }
                    for field in fields {
                        if let Some(index) = typeck.opt_field_index(field.hir_id) {
                            self.field(variant.fields[index].did);
                        }
                    }
                }
            }
            PatKind::TupleStruct(ref qpath, _, _) => self.qpath(qpath, pat.hir_id),
            _ => {}
        }
        intravisit::walk_pat(self, pat);
    }

    fn visit_ty(&mut self, ty: &'tcx Ty<'tcx, AmbigArg>) {
        self.macros(ty.span);
        if let TyKind::Path(ref qpath) = ty.kind {
            self.qpath(qpath, ty.hir_id);
        }
        intravisit::walk_ty(self, ty);
    }
}

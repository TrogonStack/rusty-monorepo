use clippy_utils::diagnostics::span_lint_and_then;
use rustc_hir::def::{CtorOf, DefKind, Res};
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::intravisit::FnKind;
use rustc_hir::{Body, Expr, ExprKind, HirId, PatKind, StructTailExpr};
use rustc_lint::LateContext;
use rustc_middle::ty::{self, GenericParamDefKind};
use rustc_span::{Ident, Span};

use crate::NEW_INSTEAD_OF_FROM;
use crate::test_context::is_test_context;

const ALLOWED_CONVERSIONS: &[&str] = &["into", "to_owned", "to_string", "into_owned"];

#[derive(serde::Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    avoid_breaking_exported_api: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            avoid_breaking_exported_api: true,
        }
    }
}

pub(crate) struct NewInsteadOfFrom {
    config: Config,
}

impl Default for NewInsteadOfFrom {
    fn default() -> Self {
        Self {
            config: dylint_linting::config_or_default("new_instead_of_from"),
        }
    }
}

impl NewInsteadOfFrom {
    pub(crate) fn check_fn<'tcx>(
        &self,
        cx: &LateContext<'tcx>,
        kind: FnKind<'tcx>,
        body: &'tcx Body<'tcx>,
        span: Span,
        def_id: LocalDefId,
    ) {
        if span.from_expansion() {
            return;
        }

        let Some(ident) = constructor_ident(kind) else {
            return;
        };

        if !has_qualifying_signature(cx, kind, def_id) {
            return;
        }

        let Some(param_hir_id) = sole_simple_param(body) else {
            return;
        };

        let Some((impl_def_id, adt_def)) = self_adt_with_one_field(cx, def_id) else {
            return;
        };

        let Some(tail) = fn_tail(body) else {
            return;
        };

        if !tail_wraps_parameter(cx, tail, impl_def_id, adt_def, param_hir_id) {
            return;
        }

        if is_test_context(cx, cx.tcx.local_def_id_to_hir_id(def_id), span) {
            return;
        }

        if self.config.avoid_breaking_exported_api && cx.effective_visibilities.is_exported(def_id) {
            return;
        }

        let name = cx.tcx.item_name(adt_def.did());
        let param_ty = cx.tcx.fn_sig(def_id).instantiate_identity().input(0).skip_binder();

        span_lint_and_then(
            cx,
            NEW_INSTEAD_OF_FROM,
            ident.span,
            format!("`new` only wraps its argument into `{name}`, which is a conversion"),
            |diag| {
                diag.help(format!(
                    "implement `impl From<{param_ty}> for {name}` and construct with `{name}::from(..)` or \
                     `.into()` instead of `{name}::new(..)`"
                ));
                diag.note(
                    "if the implicit conversion would weaken the type, such as an id or a unit that must stay \
                     distinct from its inner representation, opt out at the site with \
                     `#[cfg_attr(dylint_lib = \"trogon_dylint_lints\", allow(new_instead_of_from, reason = \"...\"))]`",
                );
            },
        );
    }
}

fn constructor_ident(kind: FnKind<'_>) -> Option<Ident> {
    let ident = match kind {
        FnKind::ItemFn(ident, ..) | FnKind::Method(ident, _) => ident,
        FnKind::Closure => return None,
    };

    (ident.name.as_str() == "new").then_some(ident)
}

/// No `self` receiver, not `async`, and no type or const generics. A generic
/// constructor such as `fn new(v: impl Into<String>)` has no `From`
/// equivalent: the blanket impl would overlap core's `impl<T> From<T> for T`
/// (E0119).
fn has_qualifying_signature<'tcx>(cx: &LateContext<'tcx>, kind: FnKind<'tcx>, def_id: LocalDefId) -> bool {
    let no_self = match kind {
        FnKind::Method(_, sig) => !sig.decl.implicit_self.has_implicit_self(),
        FnKind::ItemFn(..) => true,
        FnKind::Closure => false,
    };

    no_self && !cx.tcx.asyncness(def_id).is_async() && !has_type_or_const_generics(cx, def_id)
}

/// Lifetimes are ignored, since `fn new(v: &str)` carries one.
fn has_type_or_const_generics(cx: &LateContext<'_>, def_id: LocalDefId) -> bool {
    cx.tcx
        .generics_of(def_id)
        .own_params
        .iter()
        .any(|param| !matches!(param.kind, GenericParamDefKind::Lifetime))
}

fn sole_simple_param(body: &Body<'_>) -> Option<HirId> {
    let [param] = body.params else {
        return None;
    };
    let PatKind::Binding(_, hir_id, _, None) = param.pat.kind else {
        return None;
    };

    Some(hir_id)
}

/// A generic impl is skipped rather than threading its parameters into the
/// suggested `From` impl. The impl's `DefId` is returned too, because `Self`
/// in the body resolves through it, not through the struct.
fn self_adt_with_one_field<'tcx>(cx: &LateContext<'tcx>, def_id: LocalDefId) -> Option<(DefId, ty::AdtDef<'tcx>)> {
    let parent = cx.tcx.opt_parent(def_id.to_def_id())?;
    if !matches!(cx.tcx.def_kind(parent), DefKind::Impl { of_trait: false }) {
        return None;
    }

    if !cx.tcx.generics_of(parent).own_params.is_empty() {
        return None;
    }

    let self_ty = cx.tcx.type_of(parent).instantiate_identity();
    let output_ty = cx.tcx.fn_sig(def_id).instantiate_identity().output().skip_binder();
    if self_ty != output_ty {
        return None;
    }

    let adt_def = self_ty.ty_adt_def()?;
    if !adt_def.is_struct() {
        return None;
    }

    (adt_def.non_enum_variant().fields.len() == 1).then_some((parent, adt_def))
}

fn fn_tail<'tcx>(body: &Body<'tcx>) -> Option<&'tcx Expr<'tcx>> {
    let ExprKind::Block(block, _) = body.value.kind else {
        return None;
    };

    if !block.stmts.is_empty() {
        return None;
    }

    block.expr
}

fn tail_wraps_parameter<'tcx>(
    cx: &LateContext<'tcx>,
    tail: &'tcx Expr<'tcx>,
    impl_def_id: DefId,
    adt_def: ty::AdtDef<'tcx>,
    param_hir_id: HirId,
) -> bool {
    match tail.kind {
        ExprKind::Call(callee, args) => {
            let [arg] = args else {
                return false;
            };
            let ExprKind::Path(qpath) = callee.kind else {
                return false;
            };

            let wraps = match cx.qpath_res(&qpath, callee.hir_id) {
                Res::Def(DefKind::Ctor(CtorOf::Struct, _), ctor_def_id) => cx.tcx.parent(ctor_def_id) == adt_def.did(),
                Res::SelfCtor(self_impl_def_id) => self_impl_def_id == impl_def_id,
                _ => false,
            };

            wraps && is_parameter_value(cx, arg, param_hir_id)
        }
        ExprKind::Struct(qpath, fields, StructTailExpr::None) => {
            let [field] = fields else {
                return false;
            };

            let wraps = match cx.qpath_res(qpath, tail.hir_id) {
                Res::Def(DefKind::Struct, struct_def_id) => struct_def_id == adt_def.did(),
                Res::SelfTyAlias { alias_to, .. } => alias_to == impl_def_id,
                _ => false,
            };

            wraps && is_parameter_value(cx, field.expr, param_hir_id)
        }
        _ => false,
    }
}

fn is_parameter_value<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>, param_hir_id: HirId) -> bool {
    match expr.kind {
        ExprKind::MethodCall(segment, receiver, [], _) => {
            ALLOWED_CONVERSIONS.contains(&segment.ident.name.as_str()) && is_parameter(cx, receiver, param_hir_id)
        }
        _ => is_parameter(cx, expr, param_hir_id),
    }
}

fn is_parameter(cx: &LateContext<'_>, expr: &Expr<'_>, param_hir_id: HirId) -> bool {
    let ExprKind::Path(qpath) = expr.kind else {
        return false;
    };

    matches!(cx.qpath_res(&qpath, expr.hir_id), Res::Local(hir_id) if hir_id == param_hir_id)
}

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::sym;
use clippy_utils::ty::implements_trait;
use rustc_hir::{GenericParamKind, Item, ItemKind, VariantData};
use rustc_lint::LateContext;

use crate::OPTIONS_STRUCT_WITHOUT_DEFAULT;

pub(crate) fn check_item<'tcx>(cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
    if item.span.from_expansion() {
        return;
    }

    let ItemKind::Struct(ident, generics, VariantData::Struct { fields, .. }) = item.kind else {
        return;
    };

    if !ident.name.as_str().ends_with("Options") || fields.is_empty() {
        return;
    }

    if fields.iter().any(|field| field.vis_span.is_empty()) {
        return;
    }

    if generics
        .params
        .iter()
        .any(|param| !matches!(param.kind, GenericParamKind::Lifetime { .. }))
    {
        return;
    }

    let Some(default_trait) = cx.tcx.get_diagnostic_item(sym::Default) else {
        return;
    };

    let ty = cx.tcx.type_of(item.owner_id).instantiate_identity();
    if implements_trait(cx, ty, default_trait, &[]) {
        return;
    }

    span_lint_and_then(
        cx,
        OPTIONS_STRUCT_WITHOUT_DEFAULT,
        ident.span,
        format!("options struct `{}` does not implement `Default`", ident.name),
        |diag| {
            diag.help(
                "derive or implement `Default` so callers can set one field and take the rest with \
                 `..Default::default()`",
            );
        },
    );
}

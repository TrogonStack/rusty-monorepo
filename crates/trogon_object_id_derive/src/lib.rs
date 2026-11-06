#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields, LitStr, Type};

fn runtime_path() -> syn::Result<proc_macro2::TokenStream> {
    match crate_name("trogon_object_id") {
        Ok(FoundCrate::Itself) => Ok(quote!(::trogon_object_id)),
        Ok(FoundCrate::Name(name)) => {
            let name = format_ident!("{}", name);
            Ok(quote!(::#name))
        }
        Err(_) => Err(syn::Error::new(proc_macro2::Span::call_site(), "add trogon_object_id as a dependency before using its derives")),
    }
}

#[proc_macro_derive(ObjectIdDomain, attributes(object_id))]
pub fn derive_domain(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_domain(input).unwrap_or_else(syn::Error::into_compile_error).into()
}

fn expand_domain(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    reject_generics(&input, "ObjectIdDomain")?;
    if !matches!(&input.data, Data::Struct(data) if matches!(&data.fields, Fields::Unit)) {
        return Err(syn::Error::new_spanned(&input.ident, "ObjectIdDomain requires a unit struct"));
    }
    let path = runtime_path()?;
    let mut prefix: Option<LitStr> = None;
    let mut separator: Option<LitStr> = None;
    let mut payload: Option<Type> = None;
    for attribute in &input.attrs {
        if !attribute.path().is_ident("object_id") { continue; }
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("prefix") {
                if prefix.is_some() { return Err(meta.error("duplicate object_id prefix")); }
                prefix = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("separator") {
                if separator.is_some() { return Err(meta.error("duplicate object_id separator")); }
                separator = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("payload") {
                if payload.is_some() { return Err(meta.error("duplicate object_id payload")); }
                payload = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("expected prefix, separator, or payload"));
            }
            Ok(())
        })?;
    }
    let prefix = prefix.ok_or_else(|| syn::Error::new_spanned(&input.ident, "missing #[object_id(prefix = \"...\")] on domain marker"))?;
    let separator = separator.unwrap_or_else(|| LitStr::new("_", proc_macro2::Span::call_site()));
    let payload = payload.map(|ty| quote!(#ty)).unwrap_or_else(|| quote!(#path::Text));
    let name = &input.ident;
    Ok(quote! {
        const _: #path::Definition = #path::Definition::new(#prefix, #separator);
        impl #path::Domain for #name {
            const DEFINITION: #path::Definition = #path::Definition::new(#prefix, #separator);
            type Payload = #payload;
        }
    })
}

fn reject_generics(input: &DeriveInput, derive_name: &str) -> syn::Result<()> {
    if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(&input.generics, format!("{derive_name} does not support generic declarations")));
    }
    Ok(())
}

#[proc_macro_derive(ObjectIdUnion)]
pub fn derive_union(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_union(input).unwrap_or_else(syn::Error::into_compile_error).into()
}

fn expand_union(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    reject_generics(&input, "ObjectIdUnion")?;
    let path = runtime_path()?;
    let name = &input.ident;
    let data = match &input.data {
        Data::Enum(data) => data,
        _ => return Err(syn::Error::new_spanned(name, "ObjectIdUnion requires an enum")),
    };
    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(name, "ObjectIdUnion requires at least one variant"));
    }
    let mut variants = Vec::new();
    let mut members = Vec::new();
    for variant in &data.variants {
        let field = match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => fields.unnamed.first(),
            _ => None,
        }.ok_or_else(|| syn::Error::new_spanned(variant, "ObjectIdUnion variants must have exactly one unnamed identifier field"))?;
        variants.push(&variant.ident);
        members.push(&field.ty);
    }
    Ok(quote! {
        const _: () = #path::assert_disjoint(&[#(<#members as #path::Identifier>::DEFINITION),*]);
        #(impl ::core::convert::From<#members> for #name {
            fn from(value: #members) -> Self { Self::#variants(value) }
        })*
        impl ::core::str::FromStr for #name {
            type Err = #path::ParseError;
            fn from_str(value: &str) -> ::core::result::Result<Self, Self::Err> {
                #(if <#members as #path::Identifier>::DEFINITION.strip_prefix(value).is_some() {
                    return value.parse::<#members>().map(Self::#variants);
                })*
                Err(#path::ParseError::UnknownPrefix)
            }
        }
        impl ::core::convert::TryFrom<&str> for #name {
            type Error = #path::ParseError;
            fn try_from(value: &str) -> ::core::result::Result<Self, Self::Error> {
                value.parse()
            }
        }
        impl ::core::convert::TryFrom<::std::string::String> for #name {
            type Error = #path::ParseError;
            fn try_from(mut value: ::std::string::String) -> ::core::result::Result<Self, Self::Error> {
                #(if let Some(raw) = <#members as #path::Identifier>::DEFINITION.strip_prefix(&value) {
                    let prefix_len = value.len() - raw.len();
                    value.drain(..prefix_len);
                    return <#members as #path::Identifier>::try_from_owned(value).map(Self::#variants);
                })*
                Err(#path::ParseError::UnknownPrefix)
            }
        }
        impl ::core::fmt::Display for #name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match self { #(Self::#variants(value) => ::core::fmt::Display::fmt(value, formatter)),* }
            }
        }
        #path::__impl_serde!(#name);
    })
}

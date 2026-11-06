#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote};
use syn::{parse_macro_input, Data, DeriveInput, Fields};

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
    let path = match runtime_path() { Ok(path) => path, Err(error) => return error.into_compile_error().into() };
    let name = input.ident;
    quote! {
        impl #path::Domain for #name {
            const DEFINITION: #path::Definition = #path::Definition::new("stub", "_");
            type Payload = #path::Text;
        }
    }.into()
}

#[proc_macro_derive(ObjectIdUnion)]
pub fn derive_union(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let path = match runtime_path() { Ok(path) => path, Err(error) => return error.into_compile_error().into() };
    let name = input.ident;
    let variants = match input.data {
        Data::Enum(data) => data.variants,
        _ => return syn::Error::new_spanned(name, "ObjectIdUnion requires an enum").into_compile_error().into(),
    };
    let conversions = variants.iter().filter_map(|variant| {
        let Fields::Unnamed(fields) = &variant.fields else { return None; };
        let field = fields.unnamed.first()?;
        let member = &field.ty;
        let variant = &variant.ident;
        Some(quote! { impl ::core::convert::From<#member> for #name { fn from(value: #member) -> Self { Self::#variant(value) } } })
    });
    quote! {
        #(#conversions)*
        impl ::core::str::FromStr for #name {
            type Err = #path::ParseError;
            fn from_str(_value: &str) -> ::core::result::Result<Self, Self::Err> { Err(#path::ParseError::UnknownPrefix) }
        }
        impl ::core::fmt::Display for #name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result { formatter.write_str("") }
        }
    }.into()
}

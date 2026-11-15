#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote};
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        doc_hidden_access,
        reason = "Syn documents LitStr as a public root re-export; this import is reported hidden by the pinned policy driver."
    )
)]
use syn::LitStr;
use syn::{parse_macro_input, Data, DeriveInput, Fields, Type};

fn runtime_path() -> syn::Result<proc_macro2::TokenStream> {
    match crate_name("trogon_object_id") {
        Ok(FoundCrate::Itself) => Ok(quote!(::trogon_object_id)),
        Ok(FoundCrate::Name(name)) => {
            let name = format_ident!("{}", name);
            Ok(quote!(::#name))
        }
        Err(_) => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "add trogon_object_id as a dependency before using its derives",
        )),
    }
}

struct IdOptions {
    prefix: LitStr,
    separator: LitStr,
    template: Option<Type>,
    fallible: bool,
}

fn id_options(input: &DeriveInput) -> syn::Result<IdOptions> {
    let mut prefix = None;
    let mut separator = None;
    let mut template = None;
    let mut fallible = false;
    for attribute in &input.attrs {
        if !attribute.path().is_ident("object_id") {
            continue;
        }
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("prefix") {
                if prefix.is_some() {
                    return Err(meta.error("duplicate object_id prefix"));
                }
                prefix = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("separator") {
                if separator.is_some() {
                    return Err(meta.error("duplicate object_id separator"));
                }
                separator = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("fallible") {
                if fallible {
                    return Err(meta.error("duplicate object_id fallible"));
                }
                fallible = true;
            } else if meta.path.is_ident("payload") {
                return Err(meta.error("ObjectId infers its payload from the tuple field; remove the payload option"));
            } else if meta.path.is_ident("template") {
                if template.is_some() {
                    return Err(meta.error("duplicate object_id template"));
                }
                template = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error("expected prefix, separator, template or fallible"));
            }
            Ok(())
        })?;
    }
    Ok(IdOptions {
        fallible,
        prefix: prefix
            .ok_or_else(|| syn::Error::new_spanned(&input.ident, "missing #[object_id(prefix = \"...\")]"))?,
        separator: separator.unwrap_or_else(|| LitStr::new("_", proc_macro2::Span::call_site())),
        template,
    })
}

#[proc_macro_derive(ObjectId, attributes(object_id))]
pub fn derive_object_id(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_object_id(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_object_id(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    reject_generics(&input, "ObjectId")?;
    match &input.data {
        Data::Struct(_) => expand_struct(input),
        Data::Enum(_) => expand_enum(input),
        Data::Union(_) => Err(syn::Error::new_spanned(
            &input.ident,
            "ObjectId requires a payload newtype struct or an enum of identifiers",
        )),
    }
}

fn expand_struct(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let field = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => fields.unnamed.first(),
            _ => None,
        },
        _ => None,
    }
    .ok_or_else(|| syn::Error::new_spanned(name, "ObjectId requires exactly one private unnamed payload field"))?;
    if !matches!(field.vis, syn::Visibility::Inherited) {
        return Err(syn::Error::new_spanned(
            &field.vis,
            "ObjectId payload field must be private",
        ));
    }
    let mut transparent = false;
    for attribute in &input.attrs {
        if attribute.path().is_ident("repr") {
            let arguments = attribute
                .parse_args_with(syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)?;
            transparent |= arguments
                .iter()
                .any(|argument| matches!(argument, syn::Meta::Path(path) if path.is_ident("transparent")));
        }
    }
    if !transparent {
        return Err(syn::Error::new_spanned(
            name,
            "ObjectId requires #[repr(transparent)] to preserve the payload layout",
        ));
    }
    let options = id_options(&input)?;
    let path = runtime_path()?;
    let prefix = &options.prefix;
    let separator = &options.separator;
    let payload = &field.ty;
    let template = options
        .template
        .map(|template| quote!(#path::__impl_template_domain!(#name, #template, direct);));
    let conversion = if options.fallible {
        quote! {
            impl ::core::convert::TryFrom<#payload> for #name {
                type Error = #path::ParseError;
                fn try_from(value: #payload) -> ::core::result::Result<Self, Self::Error> {
                    Self::try_from_payload(value)
                }
            }
        }
    } else {
        quote! {
            impl ::core::convert::From<#payload> for #name {
                fn from(value: #payload) -> Self {
                    Self(#path::ObjectId::<Self>::from(value).into_payload())
                }
            }
        }
    };
    Ok(quote! {
        #conversion
        const _: #path::Definition = #path::Definition::new(#prefix, #separator);
        impl #path::Domain for #name {
            const DEFINITION: #path::Definition = #path::Definition::new(#prefix, #separator);
            type Payload = #payload;
        }
        impl #name {
            pub fn try_from_raw(value: &str) -> ::core::result::Result<Self, #path::ParseError> {
                #path::ObjectId::<Self>::try_from_raw(value).map(|value| Self(value.into_payload()))
            }
            #path::__with_alloc! {
            pub fn try_from_owned(value: #path::__private::String) -> ::core::result::Result<Self, #path::ParseError> {
                #path::ObjectId::<Self>::try_from_owned(value).map(|value| Self(value.into_payload()))
            }
            }
            pub fn try_from_payload(value: #payload) -> ::core::result::Result<Self, #path::ParseError> {
                #path::ObjectId::<Self>::try_from_payload(value).map(|value| Self(value.into_payload()))
            }
            pub fn payload(&self) -> &#payload { &self.0 }
            pub fn into_payload(self) -> #payload { self.0 }
            pub fn as_raw(&self) -> &<#payload as #path::Payload>::Raw {
                <#payload as #path::Payload>::as_raw(&self.0)
            }
            pub fn generate<G: #path::Generator<Payload = #payload>>() -> ::core::result::Result<Self, #path::ParseError> {
                #path::ObjectId::<Self>::generate::<G>().map(|value| Self(value.into_payload()))
            }
        }
        impl #path::Identifier for #name {
            const DEFINITION: #path::Definition = <Self as #path::Domain>::DEFINITION;
            type Raw = <#payload as #path::Payload>::Raw;
            fn try_from_raw(value: &str) -> ::core::result::Result<Self, #path::ParseError> { Self::try_from_raw(value) }
            #path::__with_alloc! {
            fn try_from_owned(value: #path::__private::String) -> ::core::result::Result<Self, #path::ParseError> { Self::try_from_owned(value) }
            }
            fn as_raw(&self) -> &Self::Raw { self.as_raw() }
        }
        impl ::core::str::FromStr for #name {
            type Err = #path::ParseError;
            fn from_str(value: &str) -> ::core::result::Result<Self, Self::Err> {
                value.parse::<#path::ObjectId<Self>>().map(|value| Self(value.into_payload()))
            }
        }
        impl ::core::convert::TryFrom<&str> for #name {
            type Error = #path::ParseError;
            fn try_from(value: &str) -> ::core::result::Result<Self, Self::Error> { value.parse() }
        }
        #path::__with_alloc! {
        impl ::core::convert::TryFrom<#path::__private::String> for #name {
            type Error = #path::ParseError;
            fn try_from(value: #path::__private::String) -> ::core::result::Result<Self, Self::Error> {
                #path::ObjectId::<Self>::try_from(value).map(|value| Self(value.into_payload()))
            }
        }
        }
        impl ::core::fmt::Display for #name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                let definition = <Self as #path::Domain>::DEFINITION;
                formatter.write_str(definition.object_type())?;
                formatter.write_str(definition.separator())?;
                ::core::fmt::Display::fmt(&self.0, formatter)
            }
        }
        #path::__impl_serde!(#name);
        #template
    })
}

fn reject_generics(input: &DeriveInput, derive_name: &str) -> syn::Result<()> {
    if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            format!("{derive_name} does not support generic declarations"),
        ));
    }
    Ok(())
}

fn expand_enum(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if let Some(attribute) = input
        .attrs
        .iter()
        .find(|attribute| attribute.path().is_ident("object_id"))
    {
        return Err(syn::Error::new_spanned(
            attribute,
            "ObjectId enums inherit their prefixes and payloads from their variants; remove #[object_id(...)] from the enum",
        ));
    }
    let path = runtime_path()?;
    let name = &input.ident;
    let data = match &input.data {
        Data::Enum(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                name,
                "ObjectId requires a payload newtype struct or an enum of identifiers",
            ))
        }
    };
    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
            name,
            "ObjectId enums require at least one variant",
        ));
    }
    let mut variants = Vec::new();
    let mut members = Vec::new();
    for variant in &data.variants {
        if let Some(attribute) = variant
            .attrs
            .iter()
            .chain(variant.fields.iter().flat_map(|field| &field.attrs))
            .find(|attribute| attribute.path().is_ident("object_id"))
        {
            return Err(syn::Error::new_spanned(
                attribute,
                "ObjectId enum variants and fields do not accept #[object_id(...)]",
            ));
        }
        let field = match &variant.fields {
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => fields.unnamed.first(),
            _ => None,
        }
        .ok_or_else(|| {
            syn::Error::new_spanned(
                variant,
                "ObjectId enum variants must have exactly one unnamed identifier field",
            )
        })?;
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
        #path::__with_alloc! {
        impl ::core::convert::TryFrom<#path::__private::String> for #name {
            type Error = #path::ParseError;
            fn try_from(mut value: #path::__private::String) -> ::core::result::Result<Self, Self::Error> {
                #(if let Some(raw) = <#members as #path::Identifier>::DEFINITION.strip_prefix(&value) {
                    let prefix_len = value.len() - raw.len();
                    value.drain(..prefix_len);
                    return <#members as #path::Identifier>::try_from_owned(value).map(Self::#variants);
                })*
                Err(#path::ParseError::UnknownPrefix)
            }
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

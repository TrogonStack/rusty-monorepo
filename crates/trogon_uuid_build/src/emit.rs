use std::collections::HashSet;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Ident, LitStr, Path};

use crate::descriptor::Recipe;
use crate::BuildError;

pub(crate) fn identifier(name: &str, context: &'static str) -> Result<Ident, BuildError> {
    syn::parse_str(name).map_err(|source| BuildError::RustSyntax { context, source })
}

enum Part {
    Literal(String),
    Field(Ident),
}

fn parts(recipe: &Recipe) -> Result<(Vec<Part>, Vec<Ident>), BuildError> {
    let text = &recipe.template;
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut fields = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = 0;
    let mut start = 0;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'{' => {
                if start < cursor {
                    parts.push(Part::Literal(text[start..cursor].to_owned()));
                }
                let closing = text[cursor + 1..]
                    .find('}')
                    .map(|index| cursor + 1 + index)
                    .ok_or_else(|| recipe.selection.error("template contains an unclosed placeholder"))?;
                let name = &text[cursor + 1..closing];
                if name.is_empty()
                    || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    || name.as_bytes()[0].is_ascii_digit()
                {
                    return Err(recipe
                        .selection
                        .error("template placeholders must be Rust field identifiers"));
                }
                let field = syn::parse_str::<Ident>(name)
                    .or_else(|_| syn::parse_str::<Ident>(&format!("r#{name}")))
                    .map_err(|source| BuildError::RustSyntax {
                        context: "template placeholder",
                        source,
                    })?;
                if seen.insert(name.to_owned()) {
                    fields.push(field.clone());
                }
                parts.push(Part::Field(field));
                cursor = closing + 1;
                start = cursor;
            }
            b'}' => return Err(recipe.selection.error("template contains an unmatched closing brace")),
            _ => cursor += 1,
        }
    }
    if start < text.len() {
        parts.push(Part::Literal(text[start..].to_owned()));
    }
    Ok((parts, fields))
}

pub(crate) fn recipe(
    recipe: &Recipe,
    marker: &Ident,
    input: &Ident,
    runtime: &Path,
) -> Result<TokenStream, BuildError> {
    let (parts, fields) = parts(recipe)?;
    let namespace = recipe.namespace.as_bytes();
    let namespace_value = quote!(#runtime::Uuid::from_bytes([#(#namespace),*]));
    if fields.is_empty() {
        let value = trogon_uuid::uuid5(&recipe.namespace, recipe.template.as_bytes());
        let value = value.as_bytes();
        return Ok(quote! {
            #[derive(Debug, Clone, Copy)]
            pub struct #marker;
            impl #marker {
                pub const NAMESPACE: #runtime::Uuid = #namespace_value;
                pub const VALUE: #runtime::Uuid = #runtime::Uuid::from_bytes([#(#value),*]);
                pub fn uuid5(input: ()) -> #runtime::Uuid {
                    <Self as #runtime::UuidTemplate>::uuid5(input)
                }
            }
            impl #runtime::UuidTemplate for #marker {
                type Input<'a> = ();
                const NAMESPACE: #runtime::Uuid = Self::NAMESPACE;
                fn uuid5(_input: ()) -> #runtime::Uuid { Self::VALUE }
            }
        });
    }
    let rendered = parts.iter().map(|part| match part {
        Part::Literal(text) => {
            let text = LitStr::new(text, Span::call_site());
            quote!(#text.as_bytes())
        }
        Part::Field(field) => quote!(input.#field.as_bytes()),
    });
    Ok(quote! {
        #[derive(Debug, Clone, Copy)]
        pub struct #marker;
        #[derive(Debug, Clone, Copy)]
        pub struct #input<'a> { #(pub #fields: &'a str),* }
        impl #marker {
            pub const NAMESPACE: #runtime::Uuid = #namespace_value;
            pub fn uuid5(input: #input<'_>) -> #runtime::Uuid {
                <Self as #runtime::UuidTemplate>::uuid5(input)
            }
        }
        impl #runtime::UuidTemplate for #marker {
            type Input<'a> = #input<'a>;
            const NAMESPACE: #runtime::Uuid = Self::NAMESPACE;
            fn uuid5(input: #input<'_>) -> #runtime::Uuid {
                #runtime::uuid5_parts(&Self::NAMESPACE, [#(#rendered),*])
            }
        }
    })
}

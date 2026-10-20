use proc_macro::{Delimiter, Group, Ident, Punct, Spacing, Span, TokenStream, TokenTree};
use trogon_claims_syntax::{validate_object_id, validate_object_type, SyntaxViolation};

use crate::diagnostic::compile_error;
use crate::literal::string_value;

#[derive(Clone, Copy)]
pub(crate) enum Target {
    ObjectId,
    ObjectType,
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Target::ObjectId => "object_id",
            Target::ObjectType => "object_type",
        }
    }

    fn type_name(self) -> &'static str {
        match self {
            Target::ObjectId => "ObjectId",
            Target::ObjectType => "ObjectType",
        }
    }

    fn validate(self, value: &str) -> Result<(), SyntaxViolation> {
        match self {
            Target::ObjectId => validate_object_id(value),
            Target::ObjectType => validate_object_type(value),
        }
    }
}

struct Invocation {
    path: Vec<TokenTree>,
    literal: TokenTree,
}

fn parse(input: TokenStream) -> Result<Invocation, (Span, &'static str)> {
    let mut path = Vec::new();
    let mut tokens = input.into_iter();
    let mut comma_span = None;
    for token in tokens.by_ref() {
        match &token {
            TokenTree::Punct(punct) if punct.as_char() == ',' => {
                comma_span = Some(punct.span());
                break;
            }
            _ => path.push(token),
        }
    }
    let Some(comma_span) = comma_span else {
        return Err((Span::call_site(), "expected a crate path and a string literal"));
    };
    let Some(literal) = tokens.next() else {
        return Err((comma_span, "expected a string literal"));
    };
    if let Some(extra) = tokens.next() {
        return Err((extra.span(), "expected exactly one string literal"));
    }
    Ok(Invocation { path, literal })
}

fn unwrap_invisible(token: TokenTree) -> TokenTree {
    match token {
        TokenTree::Group(group) if group.delimiter() == Delimiter::None => {
            let mut inner = group.stream().into_iter();
            match (inner.next(), inner.next()) {
                (Some(only), None) => unwrap_invisible(only),
                _ => TokenTree::Group(group),
            }
        }
        other => other,
    }
}

pub(crate) fn expand(input: TokenStream, target: Target) -> TokenStream {
    let invocation = match parse(input) {
        Ok(invocation) => invocation,
        Err((span, message)) => return compile_error(span, message),
    };
    let literal = unwrap_invisible(invocation.literal);
    let invocation = Invocation { literal, ..invocation };
    let span = invocation.literal.span();
    let text = match &invocation.literal {
        TokenTree::Literal(literal) => literal.to_string(),
        _ => return compile_error(span, "expected a string literal"),
    };
    let value = match string_value(&text) {
        Ok(value) => value,
        Err(message) => return compile_error(span, message),
    };
    if let Err(violation) = target.validate(&value) {
        let message = format!("{} {:?}: {}", target.label(), value, violation);
        return compile_error(span, &message);
    }
    build(invocation, target, span)
}

fn build(invocation: Invocation, target: Target, span: Span) -> TokenStream {
    let mut output: Vec<TokenTree> = invocation.path;
    output.extend([
        Punct::new(':', Spacing::Joint).into(),
        Punct::new(':', Spacing::Alone).into(),
        Ident::new(target.type_name(), span).into(),
        Punct::new(':', Spacing::Joint).into(),
        Punct::new(':', Spacing::Alone).into(),
        Ident::new("from_static_unchecked", span).into(),
        Group::new(Delimiter::Parenthesis, TokenStream::from(invocation.literal)).into(),
    ]);
    output.into_iter().collect()
}

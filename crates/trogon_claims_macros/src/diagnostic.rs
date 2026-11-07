use proc_macro::{Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream, TokenTree};

fn spanned<T: Into<TokenTree>>(span: Span, token: T) -> TokenTree {
    let mut token = token.into();
    token.set_span(span);
    token
}

pub(crate) fn compile_error(span: Span, message: &str) -> TokenStream {
    let mut body = TokenStream::new();
    body.extend([spanned(span, Literal::string(message))]);
    [
        spanned(span, Punct::new(':', Spacing::Joint)),
        spanned(span, Punct::new(':', Spacing::Alone)),
        spanned(span, Ident::new("core", span)),
        spanned(span, Punct::new(':', Spacing::Joint)),
        spanned(span, Punct::new(':', Spacing::Alone)),
        spanned(span, Ident::new("compile_error", span)),
        spanned(span, Punct::new('!', Spacing::Alone)),
        spanned(span, Group::new(Delimiter::Brace, body)),
    ]
    .into_iter()
    .collect()
}

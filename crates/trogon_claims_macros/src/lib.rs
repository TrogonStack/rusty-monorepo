mod constants;
mod diagnostic;
mod expand;
mod literal;

use proc_macro::TokenStream;

use expand::{expand, Target};

#[proc_macro]
pub fn object_id_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::ObjectId)
}

#[proc_macro]
pub fn object_type_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::ObjectType)
}

#[proc_macro]
pub fn principal_kind_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::PrincipalKind)
}

#[proc_macro]
pub fn relation_name_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::RelationName)
}

#[proc_macro]
pub fn param_name_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::ParamName)
}

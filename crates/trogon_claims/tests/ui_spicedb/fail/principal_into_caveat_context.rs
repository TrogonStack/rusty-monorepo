use prost_types::Struct;
use trogon_claims::Principal;

fn caveat_context<T>(value: T) -> Struct
where
    Struct: From<T>,
{
    Struct::from(value)
}

fn main() {
    let principal = Principal::anonymous();
    let _ = caveat_context(&principal);
}

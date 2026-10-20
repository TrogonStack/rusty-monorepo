use prost_types::Struct;
use trogon_claims::Principal;

fn main() {
    let principal = Principal::anonymous();
    let _ = Struct::from(&principal);
}

use trogon_claims_macros::principal_kind_impl;

fn main() {
    let kind = principal_kind_impl!(trogon_claims, "人/Person");
    assert_eq!(kind.as_str(), "人/Person");
}

use trogon_claims::__private::principal_kind_impl;

fn main() {
    let kind = principal_kind_impl!(trogon_claims, "person");
    assert_eq!(kind.as_str(), "person");
}

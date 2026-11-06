use trogon_claims::{principal_kind, PrincipalKind};

fn main() {
    const KIND: PrincipalKind = principal_kind!("Person");
    assert_eq!(KIND.as_str(), "Person");
}

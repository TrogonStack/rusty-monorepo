use trogon_claims::{principal_kind, PrincipalKind};

fn main() {
    let person: PrincipalKind = principal_kind!("person");
    assert_eq!(person.as_str(), "person");
}

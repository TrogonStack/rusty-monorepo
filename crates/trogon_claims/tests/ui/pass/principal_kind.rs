use trogon_claims::{principal_kind, PrincipalKind};

fn main() {
    let person: PrincipalKind = principal_kind!("person");
    assert_eq!(person.as_str(), "person");
    let short: PrincipalKind = principal_kind!("x");
    assert_eq!(short.as_str(), "x");
    let uppercase: PrincipalKind = principal_kind!("Person");
    assert_eq!(uppercase.as_str(), "Person");
    let opaque: PrincipalKind = principal_kind!("人/app/Person");
    assert_eq!(opaque.as_str(), "人/app/Person");
    let long: PrincipalKind =
        principal_kind!("An application classification exceeding the former sixty-four-byte limit");
    assert_eq!(
        long.as_str(),
        "An application classification exceeding the former sixty-four-byte limit"
    );
}

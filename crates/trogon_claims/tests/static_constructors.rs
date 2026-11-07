use trogon_claims::PrincipalKind;

#[cfg(feature = "macros")]
use trogon_claims::principal_kind;

#[test]
#[should_panic(expected = "invalid principal kind literal")]
fn static_principal_kind_rejects_empty_text() {
    let _ = PrincipalKind::from_static_unchecked("");
}

#[test]
fn valid_static_construction_remains_const_capable() {
    const KIND: PrincipalKind = PrincipalKind::from_static_unchecked("person");
    assert_eq!(KIND.as_str(), "person");
    const OPAQUE_KIND: PrincipalKind = PrincipalKind::from_static_unchecked("人/Person");
    assert_eq!(OPAQUE_KIND.as_str(), "人/Person");
}

#[cfg(feature = "macros")]
#[test]
fn literal_macros_remain_const_capable() {
    const KIND: PrincipalKind = principal_kind!("person");
    assert_eq!(KIND, PrincipalKind::from_static_unchecked("person"));
}

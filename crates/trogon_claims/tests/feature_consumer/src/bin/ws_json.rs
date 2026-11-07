use trogon_claims::{
    AuthenticationType, ClaimType, DynamicClaims, Identity, IngestProfile, NameClaimType, RawClaims, RoleClaimType,
    RoleName, WellKnownClaim,
};

fn main() {
    let raw = RawClaims::from_slice(br#"{"iss":"issuer","sub":"subject","http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name":"Display Name","http://schemas.microsoft.com/ws/2008/06/identity/claims/role":"admin"}"#).unwrap();
    for (name, _) in raw.iter() {
        assert_eq!(
            name.as_str(),
            ClaimType::try_new(name.as_str().to_owned()).unwrap().as_str()
        );
        if name.as_str().starts_with("http:") {
            #[cfg(not(feature = "ws-claims"))]
            assert_eq!(name.well_known(), None);
            #[cfg(feature = "ws-claims")]
            assert!(name.well_known().is_some());
        }
    }
    let name = ClaimType::try_new("http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name").unwrap();
    let role = ClaimType::try_new("http://schemas.microsoft.com/ws/2008/06/identity/claims/role").unwrap();
    let profile = IngestProfile::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(name),
        RoleClaimType::new(role),
    );
    let identity = Identity::<DynamicClaims>::ingest(&raw, &profile).unwrap();
    assert_eq!(identity.identity().name(), Some("Display Name"));
    assert!(identity.identity().is_in_role(&RoleName::try_new("admin").unwrap()));
    let core = IngestProfile::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(WellKnownClaim::PreferredUsername.into()),
        RoleClaimType::new(WellKnownClaim::Roles.into()),
    );
    let identity = Identity::<DynamicClaims>::ingest(&raw, &core).unwrap();
    assert_eq!(identity.identity().name(), None);
    assert!(!identity.identity().is_in_role(&RoleName::try_new("admin").unwrap()));
}

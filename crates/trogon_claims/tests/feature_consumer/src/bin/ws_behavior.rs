use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use trogon_claims::{ClaimType, NameClaimType, RoleClaimType, WellKnownClaim};

fn hash(value: &ClaimType) -> u64 {
    let mut state = DefaultHasher::new();
    value.hash(&mut state);
    state.finish()
}

fn main() {
    let name = "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/nameidentifier";
    let claim = ClaimType::try_new(name).unwrap();
    let owned = ClaimType::try_new(name.to_owned()).unwrap();
    assert_eq!(claim.as_str(), name);
    assert_eq!(claim, owned);
    assert_eq!(hash(&claim), hash(&owned));
    assert_eq!(NameClaimType::new(claim.clone()).claim_type(), &claim);
    assert_eq!(RoleClaimType::new(claim.clone()).claim_type(), &claim);
    #[cfg(not(feature = "ws-claims"))]
    {
        assert_eq!(claim.well_known(), None);
        assert_eq!(WellKnownClaim::from_name(name), None);
    }
    #[cfg(feature = "ws-claims")]
    {
        assert_eq!(claim.well_known(), Some(WellKnownClaim::WsNameIdentifier));
        assert_eq!(WellKnownClaim::WsNameIdentifier.as_str(), name);
    }
    let altered = ClaimType::try_new(name.to_uppercase()).unwrap();
    assert_eq!(altered.well_known(), None);
    assert_ne!(altered, claim);
    assert!(ClaimType::try_new("").is_err());
    for known in [WellKnownClaim::Sub, WellKnownClaim::Upn, WellKnownClaim::Roles] {
        let claim = ClaimType::try_new(known.as_str()).unwrap();
        assert_eq!(claim.well_known(), Some(known));
        assert_eq!(claim.as_str(), known.as_str());
    }
}

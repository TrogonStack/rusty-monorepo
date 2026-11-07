use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::{ClaimType, Repr};
use crate::constants::WS_NAME_IDENTIFIER;
use crate::well_known_claim::WellKnownClaim;

fn hash_of(value: &ClaimType) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn bypassed_normalization_still_equals_and_hashes_like_the_well_known_twin() {
    let well_known = ClaimType::from(WellKnownClaim::Sub);
    let custom = ClaimType(Repr::Custom("sub".into()));
    assert_eq!(well_known, custom);
    assert_eq!(custom, well_known);
    assert_eq!(hash_of(&well_known), hash_of(&custom));

    let uri = ClaimType::from(WellKnownClaim::WsNameIdentifier);
    let custom_uri = ClaimType(Repr::Custom(WS_NAME_IDENTIFIER.into()));
    assert_eq!(uri, custom_uri);
    assert_eq!(hash_of(&uri), hash_of(&custom_uri));
}

#[test]
fn constructor_normalizes_exact_matches_only() {
    assert!(matches!(
        ClaimType::try_new("sub").map(|value| value.0),
        Ok(Repr::WellKnown(WellKnownClaim::Sub))
    ));
    assert!(matches!(
        ClaimType::try_new("Sub").map(|value| value.0),
        Ok(Repr::Custom(_))
    ));
}

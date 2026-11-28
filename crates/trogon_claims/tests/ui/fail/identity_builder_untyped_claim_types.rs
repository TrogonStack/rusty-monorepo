use trogon_claims::{AuthenticationType, ClaimType, IdentityBuilder, WellKnownClaim};

fn main() {
    let name = ClaimType::from(WellKnownClaim::Sub);
    let role = ClaimType::from(WellKnownClaim::Roles);
    let _ = IdentityBuilder::new(AuthenticationType::try_new("jwt").unwrap(), name, role);
}

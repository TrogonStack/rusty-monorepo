use trogon_claims::{AuthenticationType, ClaimType, IngestProfile, WellKnownClaim};

fn main() {
    let name = ClaimType::from(WellKnownClaim::Sub);
    let role = ClaimType::from(WellKnownClaim::Roles);
    let _ = IngestProfile::new(AuthenticationType::try_new("jwt").unwrap(), name, role);
}

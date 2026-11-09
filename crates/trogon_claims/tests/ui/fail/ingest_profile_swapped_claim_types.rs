use trogon_claims::{AuthenticationType, IngestProfile, NameClaimType, RoleClaimType, WellKnownClaim};

fn main() {
    let _ = IngestProfile::new(
        AuthenticationType::try_new("jwt").unwrap(),
        RoleClaimType::new(WellKnownClaim::Roles.into()),
        NameClaimType::new(WellKnownClaim::Sub.into()),
    );
}

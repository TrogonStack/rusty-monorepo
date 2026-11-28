use trogon_claims::{AuthenticationType, IdentityBuilder, NameClaimType, RoleClaimType, WellKnownClaim};

fn main() {
    let _ = IdentityBuilder::new(
        AuthenticationType::try_new("jwt").unwrap(),
        RoleClaimType::new(WellKnownClaim::Roles.into()),
        NameClaimType::new(WellKnownClaim::Sub.into()),
    );
}

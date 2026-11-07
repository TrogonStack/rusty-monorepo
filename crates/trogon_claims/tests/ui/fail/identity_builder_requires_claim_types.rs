use trogon_claims::{AuthenticationType, IdentityBuilder};

fn main() {
    let _ = IdentityBuilder::new(AuthenticationType::try_new("jwt").unwrap());
}

use trogon_claims::{AuthenticationType, IngestProfile};

fn main() {
    let _ = IngestProfile::new(AuthenticationType::try_new("jwt").unwrap());
}

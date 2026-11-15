use trogon_claims::{Identity, PrincipalKind};

fn main() {
    let _: Identity = Identity::anonymous().with_kind(PrincipalKind::try_new("person").unwrap());
}

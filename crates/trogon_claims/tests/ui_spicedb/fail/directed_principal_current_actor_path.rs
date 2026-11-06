use trogon_claims::{ObjectType, Base64UrlUnpadded, SubjectReference, UnverifiedDirectedPrincipal};

fn reject(hint: UnverifiedDirectedPrincipal) {
    let _ = SubjectReference::try_from_current_actor::<Base64UrlUnpadded>(&hint, ObjectType::try_new("service").unwrap());
}

fn main() {}

use trogon_claims::{object_type, Base64UrlUnpadded, SubjectReference, UnverifiedDirectedPrincipal};

fn reject(hint: UnverifiedDirectedPrincipal) {
    let _ = SubjectReference::try_from_current_actor::<Base64UrlUnpadded>(&hint, object_type!("service"));
}

fn main() {}

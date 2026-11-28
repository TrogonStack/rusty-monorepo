use trogon_claims::{SubjectReference, UnverifiedDirectedPrincipal};

fn reject(hint: UnverifiedDirectedPrincipal) {
    let _ = SubjectReference::from(hint);
}

fn main() {}

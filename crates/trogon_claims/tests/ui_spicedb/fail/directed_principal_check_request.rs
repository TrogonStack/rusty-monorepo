use trogon_claims::{relation_name, CheckRequest, ObjectReference, UnverifiedDirectedPrincipal};

fn reject(resource: ObjectReference, hint: UnverifiedDirectedPrincipal) {
    let _ = CheckRequest::new(resource, relation_name!("view"), &hint, None);
}

fn main() {}

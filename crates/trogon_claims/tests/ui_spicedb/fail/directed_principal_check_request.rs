use trogon_claims::{RelationName, CheckRequest, ObjectReference, UnverifiedDirectedPrincipal};

fn reject(resource: ObjectReference, hint: UnverifiedDirectedPrincipal) {
    let _ = CheckRequest::new(resource, RelationName::try_new("view").unwrap(), &hint, None);
}

fn main() {}

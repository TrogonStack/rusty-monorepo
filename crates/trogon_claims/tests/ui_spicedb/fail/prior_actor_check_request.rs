use trogon_claims::{RelationName, CheckRequest, ObjectReference, PriorActor};

fn reject(resource: ObjectReference, prior: PriorActor) {
    let _ = CheckRequest::new(resource, RelationName::try_new("view").unwrap(), &prior, None);
}

fn main() {}

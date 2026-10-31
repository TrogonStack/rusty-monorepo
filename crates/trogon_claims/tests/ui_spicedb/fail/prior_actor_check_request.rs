use trogon_claims::{relation_name, CheckRequest, ObjectReference, PriorActor};

fn reject(resource: ObjectReference, prior: PriorActor) {
    let _ = CheckRequest::new(resource, relation_name!("view"), &prior, None);
}

fn main() {}

use trogon_claims::{PriorActor, SubjectReference};

fn reject(prior: PriorActor) {
    let _ = SubjectReference::from(prior);
}

fn main() {}

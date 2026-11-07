use trogon_claims::{ObjectType, Base64UrlUnpadded, PriorActor, SubjectReference};

fn reject(prior: PriorActor) {
    let _ = SubjectReference::try_from_current_actor::<Base64UrlUnpadded>(&prior, ObjectType::try_new("service").unwrap());
}

fn main() {}

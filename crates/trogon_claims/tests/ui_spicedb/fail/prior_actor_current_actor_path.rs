use trogon_claims::{object_type, Base64UrlUnpadded, PriorActor, SubjectReference};

fn reject(prior: PriorActor) {
    let _ = SubjectReference::try_from_current_actor::<Base64UrlUnpadded>(&prior, object_type!("service"));
}

fn main() {}

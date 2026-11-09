use std::sync::Arc;

use trogon_claims::{AuthenticatedIdentity, CurrentActor, Ingested};

#[allow(dead_code)]
fn replace_actor(identity: Arc<AuthenticatedIdentity>, actor: Arc<CurrentActor>) {
    let _ = Ingested::Delegated { identity, actor };
}

fn main() {}

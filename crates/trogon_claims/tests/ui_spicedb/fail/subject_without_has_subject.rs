use std::sync::Arc;

use trogon_claims::{AuthenticatedPrincipal, ClaimSet, Principal};

struct NoSubject;

impl ClaimSet for NoSubject {}

fn main() {
    let principal = Arc::new(Principal::<NoSubject>::anonymous());
    if let Ok(wrapper) = AuthenticatedPrincipal::try_new(principal) {
        let _ = wrapper.subject();
    }
}

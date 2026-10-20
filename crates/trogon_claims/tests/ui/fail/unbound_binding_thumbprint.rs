use std::sync::Arc;

use trogon_claims::{AuthenticatedPrincipal, Principal};

fn main() {
    let principal = Arc::new(Principal::anonymous());
    if let Ok(wrapper) = AuthenticatedPrincipal::try_new(principal) {
        let _ = wrapper.binding().thumbprint();
    }
}

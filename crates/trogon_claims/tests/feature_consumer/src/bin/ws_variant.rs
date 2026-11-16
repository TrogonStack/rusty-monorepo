use trogon_claims::WellKnownClaim::WsNameIdentifier;
fn main() {
    assert!(!WsNameIdentifier.as_str().is_empty());
}

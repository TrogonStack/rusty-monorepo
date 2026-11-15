use trogon_claims::CheckOutcome;

fn main() {
    let outcome = CheckOutcome::NoPermission;
    let _ = format!("{outcome}");
}

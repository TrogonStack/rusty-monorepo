use trogon_claims::{ClaimType, WellKnownClaim};

fn main() {
    let name = std::env::args().nth(1).expect("claim name argument");
    let claim = ClaimType::try_new(name.clone()).expect("nonempty claim name");
    println!("{}\t{}", claim.as_str(), claim.well_known().is_some());
    match WellKnownClaim::from_name(&name) {
        Some(known) => println!("{}", known.as_str()),
        None => println!("custom"),
    }
}

use trogon_claims::{param_name, relation_name};

fn main() {
    assert_eq!(relation_name!("can_read").as_str(), "can_read");
    assert_eq!(param_name!("tenant_id").as_str(), "tenant_id");
}

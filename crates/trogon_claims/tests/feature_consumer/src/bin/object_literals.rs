use trogon_claims::{object_id, object_type};

fn main() {
    assert_eq!(object_id!("document-1").as_str(), "document-1");
    assert_eq!(object_type!("document").as_str(), "document");
}

use trogon_claims_macros::object_id_impl;
use trogon_claims_macros::object_type_impl;
use trogon_claims_macros::param_name_impl;
use trogon_claims_macros::relation_name_impl;

fn main() {
    assert_eq!(object_id_impl!(trogon_claims, "aZ09/_|-=+").as_str(), "aZ09/_|-=+");
    assert_eq!(
        object_type_impl!(trogon_claims, "myapp/document").as_str(),
        "myapp/document"
    );
    assert_eq!(param_name_impl!(trogon_claims, "tenant_id").as_str(), "tenant_id");
    assert_eq!(relation_name_impl!(trogon_claims, "can_read").as_str(), "can_read");
}

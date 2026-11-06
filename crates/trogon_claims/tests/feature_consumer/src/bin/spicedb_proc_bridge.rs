use trogon_claims::__private::{object_id_impl, object_type_impl, param_name_impl, relation_name_impl};

fn main() {
    assert_eq!(object_id_impl!(trogon_claims, "document-1").as_str(), "document-1");
    assert_eq!(object_type_impl!(trogon_claims, "document").as_str(), "document");
    assert_eq!(param_name_impl!(trogon_claims, "tenant_id").as_str(), "tenant_id");
    assert_eq!(relation_name_impl!(trogon_claims, "can_read").as_str(), "can_read");
}

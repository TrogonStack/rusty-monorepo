use trogon_claims::__private::object_id_impl;

fn main() {
    let id = object_id_impl!(trogon_claims, "document-1");
    assert_eq!(id.as_str(), "document-1");
}

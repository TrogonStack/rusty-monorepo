use trogon_claims_syntax::validate_object_id;
use trogon_claims_syntax::validate_object_type;
use trogon_claims_syntax::validate_param_name;
use trogon_claims_syntax::validate_relation_name;

fn main() {
    assert!(validate_object_id("aZ09/_|-=+").is_ok());
    assert!(validate_object_id(&"a".repeat(1024)).is_ok());
    assert!(validate_object_id(&"a".repeat(1025)).is_err());
    assert!(validate_object_id("*").is_err());
    assert!(validate_object_id("user@example.com").is_err());
    assert!(validate_object_type("myapp/document").is_ok());
    assert!(validate_relation_name("can_read").is_ok());
    assert!(validate_param_name("tenant_id").is_ok());
}

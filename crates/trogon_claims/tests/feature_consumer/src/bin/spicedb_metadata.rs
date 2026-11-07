use trogon_claims::constants::OBJECT_SEPARATOR;
use trogon_claims::ValueKind::ObjectId;

fn main() {
    assert_eq!(OBJECT_SEPARATOR, ':');
    assert_eq!(ObjectId.name(), "object_id");
}

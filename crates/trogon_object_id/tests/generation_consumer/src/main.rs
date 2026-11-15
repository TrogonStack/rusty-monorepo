#[derive(ids::ObjectId)]
#[object_id(prefix = "generated")]
#[repr(transparent)]
struct Generated(ids::Uuid);

fn main() {
    let random = Generated::generate::<ids::Uuid4>().expect("platform UUID4 backend");
    let timed = Generated::generate::<ids::Uuid7>().expect("platform UUID7 backend");
    assert_eq!(random.payload().get_version_num(), 4);
    assert_eq!(timed.payload().get_version_num(), 7);
    assert!(Generated::try_from("generated_550e8400-e29b-41d4-a716-446655440000").is_ok());
}

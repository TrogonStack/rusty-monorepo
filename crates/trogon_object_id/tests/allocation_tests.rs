#![cfg(feature = "uuid")]

use std::alloc::System;
use std::fmt::Write;
use std::hint::black_box;

use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};
use trogon_object_id::{Definition, Domain, ObjectId, Text, Uuid, Uuid4, Uuid7};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

struct SessionDomain;
impl Domain for SessionDomain {
    const DEFINITION: Definition = Definition::new("session", "_");
    type Payload = Uuid;
}
type SessionId = ObjectId<SessionDomain>;

struct TextDomain;
impl Domain for TextDomain {
    const DEFINITION: Definition = Definition::new("text", "_");
    type Payload = Text;
}
type TextId = ObjectId<TextDomain>;

struct Buffer {
    bytes: [u8; 64],
    len: usize,
}
impl Write for Buffer {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let end = self.len + value.len();
        self.bytes
            .get_mut(self.len..end)
            .ok_or(std::fmt::Error)?
            .copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

fn without_allocations<T>(action: impl FnOnce() -> T) -> T {
    let region = Region::new(GLOBAL);
    let result = action();
    let delta = region.change();
    assert_eq!(delta.allocations, 0, "operation must not allocate");
    assert_eq!(delta.reallocations, 0, "operation must not reallocate");
    result
}

#[test]
fn typed_uuid_operations_and_owned_text_need_no_extra_allocations() {
    let region = Region::new(GLOBAL);
    black_box(String::from("positive allocation control"));
    assert!(
        region.change().allocations > 0,
        "allocator instrumentation must observe real allocations"
    );

    let parsed = without_allocations(|| {
        black_box("session_550E8400-E29B-41D4-A716-446655440000")
            .parse::<SessionId>()
            .expect("valid UUID")
    });
    let copied = without_allocations(|| black_box(parsed));
    let typed = without_allocations(|| SessionId::try_from_payload(black_box(*copied.payload())).expect("typed UUID"));
    without_allocations(|| {
        let mut buffer = Buffer { bytes: [0; 64], len: 0 };
        write!(&mut buffer, "{typed}").expect("fixed output capacity");
        black_box(buffer);
    });

    let _warm_random = SessionId::generate::<Uuid4>().expect("warm UUID4 system randomness");
    let _warm_time = SessionId::generate::<Uuid7>().expect("warm UUID7 system randomness");
    without_allocations(|| {
        black_box(SessionId::generate::<Uuid4>().expect("UUID4"));
        black_box(SessionId::generate::<Uuid7>().expect("UUID7"));
    });

    let text = String::from("owned-value");
    let pointer = text.as_ptr();
    let id = without_allocations(|| TextId::try_from_owned(text).expect("owned raw text"));
    assert_eq!(id.as_raw().as_ptr(), pointer, "owned raw text keeps its buffer");
    let full = String::from("text_owned-value");
    let pointer = full.as_ptr();
    let id = without_allocations(|| TextId::try_from(full).expect("owned full text"));
    assert_eq!(id.as_raw().as_ptr(), pointer, "owned full text keeps its buffer");

    #[cfg(feature = "serde")]
    without_allocations(|| {
        let mut bytes = [0_u8; 64];
        let mut output = std::io::Cursor::new(bytes.as_mut_slice());
        serde_json::to_writer(&mut output, &typed).expect("fixed JSON output capacity");
        black_box(output.position());
        black_box(
            serde_json::from_str::<SessionId>("\"session_550e8400-e29b-41d4-a716-446655440000\"")
                .expect("borrowed JSON UUID"),
        );
    });

    #[cfg(feature = "derive")]
    {
        #[derive(Clone, Copy, trogon_object_id::ObjectId)]
        #[object_id(prefix = "direct")]
        #[repr(transparent)]
        struct DirectId(Uuid);

        let direct = without_allocations(|| {
            black_box("direct_550e8400-e29b-41d4-a716-446655440000")
                .parse::<DirectId>()
                .expect("direct UUID")
        });
        without_allocations(|| {
            black_box(DirectId::try_from_payload(black_box(*direct.payload())).expect("direct payload"));
            let mut buffer = Buffer { bytes: [0; 64], len: 0 };
            write!(&mut buffer, "{direct}").expect("fixed direct output capacity");
            black_box(buffer);
        });

        #[derive(trogon_object_id::ObjectId)]
        #[object_id(prefix = "direct_text")]
        #[repr(transparent)]
        struct DirectTextId(Text);
        let raw = String::from("owned-value");
        let pointer = raw.as_ptr();
        let direct_text = without_allocations(|| DirectTextId::try_from_owned(raw).expect("direct owned raw"));
        assert_eq!(direct_text.as_raw().as_ptr(), pointer, "direct text keeps its buffer");

        #[cfg(feature = "template")]
        {
            struct Identity;
            impl trogon_object_id::UuidTemplate for Identity {
                type Input<'a> = &'a str;
                const NAMESPACE: Uuid = Uuid::NAMESPACE_DNS;
                fn uuid5(input: Self::Input<'_>) -> Uuid {
                    trogon_uuid::uuid5(&Self::NAMESPACE, input.as_bytes())
                }
            }

            #[derive(trogon_object_id::ObjectId)]
            #[object_id(prefix = "recipe", template = Identity)]
            #[repr(transparent)]
            struct RecipeId(Uuid);
            let generated =
                without_allocations(|| RecipeId::from_identity(black_box("example.org")).expect("typed UUID recipe"));
            assert_eq!(generated.payload().get_version_num(), 5, "recipe preserves UUIDv5");
        }

        #[cfg(feature = "serde")]
        without_allocations(|| {
            let mut bytes = [0_u8; 64];
            let mut output = std::io::Cursor::new(bytes.as_mut_slice());
            serde_json::to_writer(&mut output, &direct).expect("fixed direct JSON output capacity");
            black_box(output.position());
            black_box(
                serde_json::from_str::<DirectId>("\"direct_550e8400-e29b-41d4-a716-446655440000\"")
                    .expect("borrowed direct JSON UUID"),
            );
        });
    }
}

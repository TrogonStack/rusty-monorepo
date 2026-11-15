use std::alloc::System;
use std::hint::black_box;

use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};
use trogon_uuid::{uuid5, uuid5_parts, Namespace};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn uuid_derivation_uses_no_heap_storage() {
    let region = Region::new(GLOBAL);
    black_box(String::from("allocator positive control"));
    assert!(
        region.change().allocations > 0,
        "the allocator must observe the positive control"
    );

    let region = Region::new(GLOBAL);
    let namespace = black_box(Namespace::Dns("example.com")).version_namespace("IDENTITY_VERSION_V1");
    let contiguous = uuid5(&namespace, black_box(b"item/alpha/42"));
    let streamed = uuid5_parts(&namespace, black_box(["item/", "alpha", "/", "42"]));
    black_box((
        contiguous,
        streamed,
        Namespace::Url("https://example.com/items").resolve(),
    ));
    let delta = region.change();
    assert_eq!(delta.allocations, 0, "UUID derivation must not allocate");
    assert_eq!(delta.reallocations, 0, "UUID derivation must not reallocate");
    assert_eq!(contiguous, streamed, "streaming keeps the exact name bytes");
    assert_eq!(streamed.get_version_num(), 5, "streaming produces UUIDv5");
}

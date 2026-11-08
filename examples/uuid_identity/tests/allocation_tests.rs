#![forbid(unsafe_code)]

use std::alloc::System;
use std::hint::black_box;

use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};
use uuid_identity_example::generated::{SkuIdentityV1, SkuIdentityV1Input};
use uuid_identity_example::SkuId;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn generated_recipe_and_bound_object_id_do_not_allocate() {
    let control = Region::new(GLOBAL);
    black_box(String::from("positive allocation control"));
    assert!(
        control.change().allocations > 0,
        "instrumentation observes actual allocations"
    );

    let measured = Region::new(GLOBAL);
    let input = SkuIdentityV1Input {
        warehouse_id: black_box("warehouse-17"),
        sku_code: black_box("ABC-123"),
    };
    let uuid = black_box(SkuIdentityV1::uuid5(black_box(input)));
    let id = black_box(SkuId::from_identity(black_box(input)).expect("valid generated UUID"));
    let delta = measured.change();
    assert_eq!(
        delta.allocations, 0,
        "generated recipe and object binding allocate nothing"
    );
    assert_eq!(
        delta.reallocations, 0,
        "generated recipe and object binding reallocate nothing"
    );
    assert_eq!(*id.payload(), uuid, "bound ID wraps the recipe result");
}

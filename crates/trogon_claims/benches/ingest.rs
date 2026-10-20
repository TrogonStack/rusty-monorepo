#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use trogon_claims::{
    AuthenticationType, Claim, ClaimText, ClaimValue, DynamicClaims, Identity, IdentityBuilder, IngestProfile, Issuer,
    RawClaims, WellKnownClaim,
};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        System.realloc(pointer, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

const PAYLOAD: &str = r#"{
  "iss": "https://issuer.example.com",
  "sub": "user-4f9c1e",
  "aud": ["https://api.example.com", "https://admin.example.com"],
  "exp": 1443904177,
  "nbf": 1443904077,
  "iat": 1443904077,
  "jti": "5b1e2f2a-9d8c-4d0f-8f6a-0d6b6f6f7d11",
  "scope": "read write",
  "client_id": "web-frontend",
  "email": "user@example.com",
  "email_verified": true,
  "name": "Ada Lovelace"
}"#;

const ITERATIONS: usize = 20_000;

fn measure(label: &str, mut work: impl FnMut()) {
    for _ in 0..1_000 {
        work();
    }
    let allocations_before = ALLOCATIONS.load(Ordering::Relaxed);
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        work();
    }
    let elapsed = started.elapsed();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations_before;
    println!(
        "{label:<28} {:>8} ns/iter {:>6} allocs/iter",
        elapsed.as_nanos() / ITERATIONS as u128,
        allocations / ITERATIONS,
    );
}

fn main() {
    let profile = IngestProfile::new(AuthenticationType::try_new("jwt").unwrap());
    let raw = RawClaims::from_slice(PAYLOAD.as_bytes()).unwrap();
    println!("payload members: {}", raw.len());

    measure("RawClaims::from_slice", || {
        black_box(RawClaims::from_slice(black_box(PAYLOAD.as_bytes())).unwrap());
    });

    measure("Identity::ingest (raw ready)", || {
        black_box(Identity::<DynamicClaims>::ingest(black_box(&raw), &profile).unwrap());
    });

    measure("from_slice + ingest", || {
        let raw = RawClaims::from_slice(black_box(PAYLOAD.as_bytes())).unwrap();
        black_box(Identity::<DynamicClaims>::ingest(&raw, &profile).unwrap());
    });

    let issuer = Issuer::try_new("https://issuer.example.com").unwrap();
    measure("IdentityBuilder::try_build", || {
        let mut builder = IdentityBuilder::new(AuthenticationType::try_new("jwt").unwrap());
        for index in 0..12 {
            let claim_type = if index == 0 {
                WellKnownClaim::Sub.into()
            } else {
                WellKnownClaim::Scope.into()
            };
            builder = builder.push(
                Claim::builder(claim_type, ClaimValue::Text(ClaimText::from("value")))
                    .issuer(issuer.clone())
                    .build(),
            );
        }
        black_box(builder.try_build().unwrap());
    });
}

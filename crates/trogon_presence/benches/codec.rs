use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use trogon_presence::codec::{decode, encode};

fn codec(c: &mut Criterion) {
    let ascii = b"user_0123456789-abcdef".as_slice();
    let mixed = "jos\u{e9}.garc\u{ed}a@ex\u{e1}mple.io:\u{1f600}".as_bytes();
    let ascii_token = encode(ascii).into_owned();
    let mixed_token = encode(mixed).into_owned();

    c.bench_function("encode/ascii", |b| b.iter(|| encode(black_box(ascii))));
    c.bench_function("encode/mixed_utf8", |b| b.iter(|| encode(black_box(mixed))));
    c.bench_function("decode/ascii", |b| b.iter(|| decode(black_box(&ascii_token))));
    c.bench_function("decode/mixed_utf8", |b| b.iter(|| decode(black_box(&mixed_token))));
}

criterion_group!(benches, codec);
criterion_main!(benches);

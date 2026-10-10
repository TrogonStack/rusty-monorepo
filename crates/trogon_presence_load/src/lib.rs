//! Local load measurement for the presence build: auth callout admission and the service under a
//! fixed writer, topic and reader profile, run against each pinned nats-server.
//!
//! The crate has no library code. The harness lives in `tests/load` so it can reuse the auth
//! four-account fixture and the end-to-end browser support through `#[path]`, exactly as the
//! end-to-end crate does. It records numbers; it does not assert capacity.
